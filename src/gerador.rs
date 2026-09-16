//! Modo gerador: roda dentro do container e injeta carga no relay.
//!
//! Cada thread mantém seu próprio socket e seu próprio ritmo. A numeração de
//! sequência é global ao gerador, então `(nome, seq)` identifica a mensagem
//! unicamente em todo o teste.

use anyhow::{Context, Result};
use chrono::{SecondsFormat, Utc};
use clap::Args;
use serde::Serialize;
use std::fmt::Write as _;
use std::io::Write as _;
use std::net::{TcpStream, ToSocketAddrs, UdpSocket};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use crate::config::Proto;

#[derive(Args, Debug, Clone)]
pub struct ArgsGerador {
    #[arg(long)]
    pub nome: String,
    #[arg(long)]
    pub proto: Proto,
    /// Destino no formato host:porta.
    #[arg(long)]
    pub alvo: String,
    /// Mensagens por segundo para este gerador.
    #[arg(long)]
    pub taxa: u64,
    #[arg(long, default_value_t = 1)]
    pub threads: usize,
    /// Tamanho do frame syslog em bytes, sem o `\n` do TCP.
    #[arg(long)]
    pub tamanho: usize,
    #[arg(long, default_value_t = 60.0)]
    pub duracao: f64,
    #[arg(long, default_value_t = 0.0)]
    pub aquecimento: f64,
    /// 0 = ilimitado, usa apenas a duração.
    #[arg(long, default_value_t = 0)]
    pub total: u64,
    /// Diretório onde o relatório JSON é gravado.
    #[arg(long, default_value = "/out")]
    pub saida: PathBuf,
}

#[derive(Serialize)]
pub struct RelatorioGerador {
    pub nome: String,
    pub proto: String,
    pub taxa_alvo: u64,
    pub threads: usize,
    pub tamanho_mensagem: usize,
    pub inicio_ns: i128,
    pub inicio_medicao_ns: i128,
    pub fim_ns: i128,
    pub enviadas_total: u64,
    pub enviadas_uteis: u64,
    pub bytes_enviados: u64,
    pub erros_envio: u64,
    pub taxa_real: f64,
}

fn agora_ns() -> i128 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos() as i128)
        .unwrap_or(0)
}

pub fn executar(args: ArgsGerador) -> Result<()> {
    let endereco = args
        .alvo
        .to_socket_addrs()
        .with_context(|| format!("não foi possível resolver o alvo {:?}", args.alvo))?
        .next()
        .with_context(|| format!("o alvo {:?} não resolveu para nenhum endereço", args.alvo))?;

    let threads = args.threads.max(1);
    let sequencia = Arc::new(AtomicU64::new(0));
    let parar = Arc::new(AtomicBool::new(false));

    {
        let parar = Arc::clone(&parar);
        let _ = ctrlc::set_handler(move || parar.store(true, Ordering::Relaxed));
    }

    let inicio = Instant::now();
    let inicio_ns = agora_ns();
    let inicio_medicao_ns = inicio_ns + (args.aquecimento * 1e9) as i128;

    let duracao = if args.total > 0 && args.duracao <= 0.0 {
        Duration::MAX
    } else {
        Duration::from_secs_f64(args.duracao)
    };

    // A cota de cada thread; o resto é distribuído entre as primeiras.
    let base = args.taxa / threads as u64;
    let resto = args.taxa % threads as u64;

    let mut handles = Vec::with_capacity(threads);
    for i in 0..threads {
        let taxa_thread = base + if (i as u64) < resto { 1 } else { 0 };
        let cota_total = if args.total > 0 {
            let b = args.total / threads as u64;
            let r = args.total % threads as u64;
            b + if (i as u64) < r { 1 } else { 0 }
        } else {
            0
        };

        let args = args.clone();
        let sequencia = Arc::clone(&sequencia);
        let parar = Arc::clone(&parar);

        handles.push(std::thread::spawn(move || {
            trabalhar(
                &args,
                endereco,
                taxa_thread,
                cota_total,
                duracao,
                inicio,
                inicio_medicao_ns,
                sequencia,
                parar,
            )
        }));
    }

    let mut enviadas_total = 0u64;
    let mut enviadas_uteis = 0u64;
    let mut bytes = 0u64;
    let mut erros = 0u64;
    for h in handles {
        match h.join() {
            Ok(r) => {
                enviadas_total += r.total;
                enviadas_uteis += r.uteis;
                bytes += r.bytes;
                erros += r.erros;
            }
            Err(_) => erros += 1,
        }
    }

    let fim_ns = agora_ns();
    let decorrido = inicio.elapsed().as_secs_f64();
    let janela_util = (fim_ns - inicio_medicao_ns) as f64 / 1e9;

    let relatorio = RelatorioGerador {
        nome: args.nome.clone(),
        proto: args.proto.texto().to_string(),
        taxa_alvo: args.taxa,
        threads,
        tamanho_mensagem: args.tamanho,
        inicio_ns,
        inicio_medicao_ns,
        fim_ns,
        enviadas_total,
        enviadas_uteis,
        bytes_enviados: bytes,
        erros_envio: erros,
        taxa_real: if janela_util > 0.0 {
            enviadas_uteis as f64 / janela_util
        } else {
            0.0
        },
    };

    std::fs::create_dir_all(&args.saida).ok();
    let destino = args.saida.join(format!("gerador-{}.json", args.nome));
    let json = serde_json::to_string_pretty(&relatorio)?;
    std::fs::write(&destino, json)
        .with_context(|| format!("não foi possível gravar {}", destino.display()))?;

    eprintln!(
        "gerador {}: {} mensagens em {:.1}s ({:.0} msg/s), {} erros",
        args.nome, enviadas_total, decorrido, relatorio.taxa_real, erros
    );
    Ok(())
}

struct Resultado {
    total: u64,
    uteis: u64,
    bytes: u64,
    erros: u64,
}

enum Canal {
    Tcp(TcpStream),
    Udp(UdpSocket, std::net::SocketAddr),
}

impl Canal {
    fn abrir(proto: Proto, endereco: std::net::SocketAddr) -> Result<Canal> {
        match proto {
            Proto::Tcp => {
                let fluxo = TcpStream::connect(endereco)
                    .with_context(|| format!("não foi possível conectar em {endereco}"))?;
                fluxo.set_nodelay(true).ok();
                Ok(Canal::Tcp(fluxo))
            }
            Proto::Udp => {
                let soquete =
                    UdpSocket::bind("0.0.0.0:0").context("não foi possível abrir socket UDP")?;
                Ok(Canal::Udp(soquete, endereco))
            }
        }
    }

    fn enviar(&mut self, quadro: &str) -> std::io::Result<usize> {
        match self {
            // Framing tradicional: o LF delimita as mensagens no fluxo TCP.
            Canal::Tcp(f) => {
                f.write_all(quadro.as_bytes())?;
                f.write_all(b"\n")?;
                Ok(quadro.len() + 1)
            }
            Canal::Udp(s, destino) => s.send_to(quadro.as_bytes(), *destino),
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn trabalhar(
    args: &ArgsGerador,
    endereco: std::net::SocketAddr,
    taxa: u64,
    cota: u64,
    duracao: Duration,
    inicio: Instant,
    inicio_medicao_ns: i128,
    sequencia: Arc<AtomicU64>,
    parar: Arc<AtomicBool>,
) -> Resultado {
    let mut res = Resultado {
        total: 0,
        uteis: 0,
        bytes: 0,
        erros: 0,
    };

    let mut canal = match Canal::abrir(args.proto, endereco) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("gerador {}: {e:#}", args.nome);
            res.erros += 1;
            return res;
        }
    };

    // Uma única alocação de padding, reaproveitada em todas as mensagens.
    let mut enchimento = String::with_capacity(args.tamanho + 1);
    enchimento.push(' ');
    enchimento.extend(std::iter::repeat_n('A', args.tamanho));
    let mut quadro = String::with_capacity(args.tamanho + 8);

    let ritmo_ativo = taxa > 0;

    loop {
        if parar.load(Ordering::Relaxed) {
            break;
        }
        let decorrido = inicio.elapsed();
        if decorrido >= duracao {
            break;
        }
        if cota > 0 && res.total >= cota {
            break;
        }

        // Quantas mensagens já deveriam ter saído a esta altura.
        let devidas = if ritmo_ativo {
            (decorrido.as_secs_f64() * taxa as f64) as u64
        } else {
            u64::MAX
        };

        if res.total >= devidas {
            std::thread::sleep(Duration::from_micros(200));
            continue;
        }

        let lote = (devidas - res.total).min(1024);
        for _ in 0..lote {
            if cota > 0 && res.total >= cota {
                break;
            }
            let seq = sequencia.fetch_add(1, Ordering::Relaxed);
            let agora = agora_ns();
            montar(&mut quadro, args, seq, agora, &enchimento);

            match canal.enviar(&quadro) {
                Ok(n) => {
                    res.total += 1;
                    res.bytes += n as u64;
                    if agora >= inicio_medicao_ns {
                        res.uteis += 1;
                    }
                }
                Err(_) => {
                    res.erros += 1;
                    // Reconecta o TCP; o UDP não tem conexão para refazer.
                    if matches!(args.proto, Proto::Tcp) {
                        std::thread::sleep(Duration::from_millis(50));
                        if let Ok(c) = Canal::abrir(args.proto, endereco) {
                            canal = c;
                        }
                    }
                }
            }
        }
    }

    if let Canal::Tcp(f) = &mut canal {
        let _ = f.flush();
    }
    res
}

/// Monta um frame RFC5424 com exatamente `args.tamanho` bytes.
///
/// Os campos de medição vêm no início do corpo para caberem no recorte
/// `%msg:1:90%` que o receptor grava.
fn montar(quadro: &mut String, args: &ArgsGerador, seq: u64, agora: i128, enchimento: &str) {
    quadro.clear();
    let ts = Utc::now().to_rfc3339_opts(SecondsFormat::Micros, true);
    let _ = write!(
        quadro,
        "<134>1 {ts} {nome} hsb - - - g={nome} s={seq} t={agora}",
        nome = args.nome
    );

    if quadro.len() < args.tamanho {
        let faltam = args.tamanho - quadro.len();
        quadro.push_str(&enchimento[..faltam.min(enchimento.len())]);
    } else {
        quadro.truncate(args.tamanho);
    }
}
