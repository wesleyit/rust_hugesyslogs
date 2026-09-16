//! Ciclo de vida do teste: sobe a rede, os receptores, o relay e os geradores,
//! espera, drena as filas, derruba tudo e monta o relatório.

use anyhow::{bail, Context, Result};
use chrono::Local;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::Ordering;
use std::time::{Duration, Instant};

use crate::config::{Config, GeradorPlano, ModoTls};
use crate::metricas;
use crate::podman::{self, Montagem};
use crate::relatorio::{self, EnvioGerador};
use crate::rsyslog::{self, PORTA_ENTRADA};
use crate::{INTERROMPIDO, PREFIXO, REDE};

/// Estrutura de diretórios de uma execução.
pub struct Execucao {
    pub id: String,
    pub raiz: PathBuf,
    pub conf: PathBuf,
    pub certs: PathBuf,
    pub out: PathBuf,
}

impl Execucao {
    pub fn criar(base: &Path) -> Result<Execucao> {
        let id = Local::now().format("%Y-%m-%d_%H%M%S").to_string();
        let raiz = base.join(&id);
        let conf = raiz.join("conf");
        let certs = raiz.join("certs");
        let out = raiz.join("out");
        for d in [&raiz, &conf, &certs, &out] {
            std::fs::create_dir_all(d)
                .with_context(|| format!("não foi possível criar {}", d.display()))?;
        }
        Ok(Execucao {
            id,
            raiz,
            conf,
            certs,
            out,
        })
    }
}

fn interrompido() -> bool {
    INTERROMPIDO.load(Ordering::Relaxed)
}

fn checar_interrupcao() -> Result<()> {
    if interrompido() {
        bail!("execução interrompida pelo usuário");
    }
    Ok(())
}

/// Grava as configurações rsyslog e as submete a `rsyslogd -N1` dentro da imagem.
pub fn gerar_e_validar_confs(cfg: &Config, exec: &Execucao, imagem: &str) -> Result<()> {
    let relay = rsyslog::conf_relay(cfg);
    std::fs::write(exec.conf.join("relay.conf"), &relay)?;
    for i in 1..=cfg.receptores.quantidade {
        let c = rsyslog::conf_receptor(cfg, i);
        std::fs::write(exec.conf.join(format!("recv-{i}.conf")), &c)?;
    }

    if !podman::imagem_existe(imagem) {
        println!(
            "  · imagem {imagem} ainda não existe — validação do rsyslog adiada (rode 'hugesyslogs imagem')"
        );
        return Ok(());
    }

    // O rsyslogd checa o acesso aos arquivos de certificado já na validação,
    // então eles precisam existir e estar montados também aqui.
    let usa_certs = cfg.tls.modo == ModoTls::Certvalid;
    if usa_certs && !exec.certs.join("cert.pem").exists() {
        crate::certs::gerar(&exec.certs)?;
    }

    // Basta validar o relay e um receptor: os demais são idênticos a menos do índice.
    for arquivo in ["relay.conf", "recv-1.conf"] {
        let caminho = exec.conf.join(arquivo);
        let mut cmd = std::process::Command::new("podman");
        cmd.arg("run")
            .arg("--rm")
            .arg("-v")
            .arg(format!("{}:/tmp/teste.conf:ro,Z", caminho.display()));
        if usa_certs {
            cmd.arg("-v")
                .arg(format!("{}:/etc/hsb/certs:ro,Z", exec.certs.display()));
        }
        let saida = cmd
            .arg(imagem)
            .args(["rsyslogd", "-N1", "-f", "/tmp/teste.conf"])
            .output()
            .context("falha ao validar a configuração com o rsyslogd")?;

        let texto = format!(
            "{}{}",
            String::from_utf8_lossy(&saida.stdout),
            String::from_utf8_lossy(&saida.stderr)
        );
        if !saida.status.success() || texto.contains("error") || texto.contains("errors occured") {
            bail!("a configuração {arquivo} não passou no rsyslogd -N1:\n{texto}");
        }
        println!("  · {arquivo} validada pelo rsyslogd");
    }
    Ok(())
}

/// Espera o container abrir a porta, com limite de tempo.
fn esperar_porta(
    container: &str,
    porta: u16,
    udp: bool,
    limite: Duration,
    oque: &str,
) -> Result<()> {
    let inicio = Instant::now();
    while inicio.elapsed() < limite {
        checar_interrupcao()?;
        if !podman::esta_rodando(container) {
            bail!("o container {container} morreu antes de abrir a porta {porta}");
        }
        if podman::porta_escutando(container, porta, udp) {
            return Ok(());
        }
        std::thread::sleep(Duration::from_millis(200));
    }
    bail!(
        "tempo esgotado esperando {oque} (a porta {porta} não abriu em {:.0}s)",
        limite.as_secs_f64()
    )
}

pub fn executar(cfg: &Config, imagem: &str, manter: bool) -> Result<()> {
    if !podman::disponivel() {
        bail!("o podman não está disponível no PATH");
    }
    if !podman::imagem_existe(imagem) {
        bail!("a imagem {imagem} não existe — rode 'hugesyslogs imagem' primeiro");
    }

    let plano = cfg.plano();
    let exec = Execucao::criar(&cfg.saida.diretorio)?;
    println!("Execução {} em {}", exec.id, exec.raiz.display());

    // Sobras de uma execução anterior atrapalhariam os nomes dos containers.
    limpar(false);

    let resultado = rodar_ciclo(cfg, &plano, &exec, imagem);

    if !manter {
        println!("\n[8/8] Removendo containers e rede...");
        limpar(true);
    } else {
        println!("\n[8/8] --manter ativo: containers preservados para inspeção.");
    }

    resultado
}

fn rodar_ciclo(
    cfg: &Config,
    plano: &[GeradorPlano],
    exec: &Execucao,
    imagem: &str,
) -> Result<()> {
    // ---- 1. Rede ----
    println!("\n[1/8] Criando a rede {REDE}...");
    podman::rede_criar(REDE)?;
    checar_interrupcao()?;

    // ---- 2. Certificado ----
    println!("[2/8] Certificado ({:?})...", cfg.tls.modo);
    if cfg.tls.modo == ModoTls::Certvalid {
        crate::certs::gerar(&exec.certs)?;
        println!("  · certificado self-signed gerado");
    } else {
        println!("  · modo anon: nenhum certificado necessário");
    }
    checar_interrupcao()?;

    // ---- 3. Configurações rsyslog ----
    println!("[3/8] Gerando e validando as configurações rsyslog...");
    gerar_e_validar_confs(cfg, exec, imagem)?;
    checar_interrupcao()?;

    // ---- 4. Receptores ----
    println!("[4/8] Subindo {} receptores...", cfg.receptores.quantidade);
    for i in 1..=cfg.receptores.quantidade {
        let apelido = format!("recv-{i}");
        let nome = format!("{PREFIXO}{apelido}");
        let mut montagens = vec![
            Montagem::leitura(exec.conf.join(format!("recv-{i}.conf")), "/etc/hsb/recv.conf"),
            Montagem::escrita(&exec.out, "/out"),
        ];
        if cfg.tls.modo == ModoTls::Certvalid {
            montagens.push(Montagem::leitura(&exec.certs, "/etc/hsb/certs"));
        }
        podman::subir(
            &nome,
            imagem,
            REDE,
            &apelido,
            &montagens,
            &comando_rsyslog("/etc/hsb/recv.conf"),
        )?;
    }
    for i in 1..=cfg.receptores.quantidade {
        let container = format!("{PREFIXO}recv-{i}");
        esperar_porta(
            &container,
            rsyslog::PORTA_TLS,
            false,
            Duration::from_secs(30),
            &format!("o receptor recv-{i} abrir a porta TLS"),
        )
        .map_err(|e| diagnosticar(e, &container))?;
    }
    println!("  · todos os receptores escutando na porta TLS");

    // ---- 5. Relay ----
    println!("[5/8] Subindo o relay...");
    let mut montagens_relay = vec![
        Montagem::leitura(exec.conf.join("relay.conf"), "/etc/hsb/relay.conf"),
        Montagem::escrita(&exec.out, "/out"),
    ];
    if cfg.tls.modo == ModoTls::Certvalid {
        montagens_relay.push(Montagem::leitura(&exec.certs, "/etc/hsb/certs"));
    }
    podman::subir(
        &format!("{PREFIXO}relay"),
        imagem,
        REDE,
        "relay",
        &montagens_relay,
        &comando_rsyslog("/etc/hsb/relay.conf"),
    )?;
    esperar_porta(
        &format!("{PREFIXO}relay"),
        PORTA_ENTRADA,
        false,
        Duration::from_secs(30),
        "o relay abrir a porta TCP de entrada",
    )
    .map_err(|e| diagnosticar(e, &format!("{PREFIXO}relay")))?;
    esperar_porta(
        &format!("{PREFIXO}relay"),
        PORTA_ENTRADA,
        true,
        Duration::from_secs(30),
        "o relay abrir a porta UDP de entrada",
    )
    .map_err(|e| diagnosticar(e, &format!("{PREFIXO}relay")))?;
    println!("  · relay escutando em {PORTA_ENTRADA} (TCP e UDP)");

    // ---- 6. Geradores ----
    println!("[6/8] Subindo {} geradores...", plano.len());
    for p in plano {
        let apelido = format!("gen-{}", p.nome);
        podman::subir(
            &p.container(),
            imagem,
            REDE,
            &apelido,
            &[Montagem::escrita(&exec.out, "/out")],
            &comando_gerador(cfg, p),
        )?;
        println!(
            "  · {} ({}) a {} msg/s em {} thread(s)",
            p.nome,
            p.proto.texto(),
            relatorio::num(p.taxa),
            p.threads
        );
    }

    // ---- 7. Espera, drenagem e parada ----
    let limite = cfg.teste.duracao + cfg.teste.aquecimento + Duration::from_secs(60);
    println!(
        "[7/8] Rodando por {}s (aquecimento {}s)...",
        relatorio::dec(cfg.teste.duracao.as_secs_f64(), 0),
        relatorio::dec(cfg.teste.aquecimento.as_secs_f64(), 0)
    );
    aguardar_geradores(plano, limite)?;

    println!(
        "  · geradores concluídos, drenando filas por {}s...",
        relatorio::dec(cfg.teste.drenagem.as_secs_f64(), 0)
    );
    dormir_interrompivel(cfg.teste.drenagem);

    println!("  · parando relay e receptores...");
    podman::parar(&format!("{PREFIXO}relay"), 10);
    for i in 1..=cfg.receptores.quantidade {
        podman::parar(&format!("{PREFIXO}recv-{i}"), 10);
    }
    // O rsyslog grava em modo assíncrono; dá um instante para o buffer chegar ao disco.
    std::thread::sleep(Duration::from_millis(500));

    // ---- Relatório ----
    montar_relatorio(cfg, plano, exec)
}

fn comando_rsyslog(conf: &str) -> Vec<String> {
    vec![
        "rsyslogd".into(),
        "-n".into(),
        "-f".into(),
        conf.into(),
        "-i".into(),
        "NONE".into(),
    ]
}

fn comando_gerador(cfg: &Config, p: &GeradorPlano) -> Vec<String> {
    vec![
        "hugesyslogs".into(),
        "gerar".into(),
        "--nome".into(),
        p.nome.clone(),
        "--proto".into(),
        p.proto.texto().into(),
        "--alvo".into(),
        format!("relay:{PORTA_ENTRADA}"),
        "--taxa".into(),
        p.taxa.to_string(),
        "--threads".into(),
        p.threads.to_string(),
        "--tamanho".into(),
        p.tamanho_mensagem.to_string(),
        "--duracao".into(),
        format!("{:.3}", cfg.teste.duracao.as_secs_f64()),
        "--aquecimento".into(),
        format!("{:.3}", cfg.teste.aquecimento.as_secs_f64()),
        "--total".into(),
        cfg.teste.total_mensagens.to_string(),
        "--saida".into(),
        "/out".into(),
    ]
}

fn aguardar_geradores(plano: &[GeradorPlano], limite: Duration) -> Result<()> {
    let inicio = Instant::now();
    loop {
        checar_interrupcao()?;
        let ativos = plano
            .iter()
            .filter(|p| podman::esta_rodando(&p.container()))
            .count();
        if ativos == 0 {
            break;
        }
        if inicio.elapsed() > limite {
            bail!("tempo esgotado: {ativos} gerador(es) ainda rodando após o limite");
        }
        std::thread::sleep(Duration::from_millis(500));
    }

    for p in plano {
        if let Some(codigo) = podman::codigo_saida(&p.container()) {
            if codigo != 0 {
                let logs = podman::logs(&p.container());
                bail!(
                    "o gerador {} terminou com código {codigo}:\n{}",
                    p.nome,
                    logs.trim()
                );
            }
        }
    }
    Ok(())
}

fn dormir_interrompivel(total: Duration) {
    let inicio = Instant::now();
    while inicio.elapsed() < total {
        if interrompido() {
            return;
        }
        std::thread::sleep(Duration::from_millis(200));
    }
}

/// Anexa os logs do container à mensagem de erro, que sozinha diria pouco.
fn diagnosticar(erro: anyhow::Error, container: &str) -> anyhow::Error {
    let logs = podman::logs(container);
    if logs.trim().is_empty() {
        erro
    } else {
        anyhow::anyhow!("{erro}\n\n--- logs de {container} ---\n{}", logs.trim())
    }
}

fn montar_relatorio(cfg: &Config, plano: &[GeradorPlano], exec: &Execucao) -> Result<()> {
    let mut envios = Vec::new();
    let mut inicio_medicao: BTreeMap<String, i128> = BTreeMap::new();

    for p in plano {
        let caminho = exec.out.join(format!("gerador-{}.json", p.nome));
        let Ok(bruto) = std::fs::read_to_string(&caminho) else {
            eprintln!(
                "  ! o gerador {} não deixou relatório em {}",
                p.nome,
                caminho.display()
            );
            continue;
        };
        let v: serde_json::Value = serde_json::from_str(&bruto)
            .with_context(|| format!("JSON inválido em {}", caminho.display()))?;

        let numero = |c: &str| v.get(c).and_then(|x| x.as_u64()).unwrap_or(0);
        let inicio = v
            .get("inicio_medicao_ns")
            .and_then(|x| x.as_i64())
            .unwrap_or(0) as i128;
        let fim = v.get("fim_ns").and_then(|x| x.as_i64()).unwrap_or(0) as i128;

        inicio_medicao.insert(p.nome.clone(), inicio);
        envios.push(EnvioGerador {
            nome: p.nome.clone(),
            taxa_alvo: numero("taxa_alvo"),
            enviadas_uteis: numero("enviadas_uteis"),
            taxa_real: v.get("taxa_real").and_then(|x| x.as_f64()).unwrap_or(0.0),
            erros_envio: numero("erros_envio"),
            janela_s: ((fim - inicio) as f64 / 1e9).max(0.0),
        });
    }

    let ag = metricas::coletar(&exec.out, cfg.receptores.quantidade, &inicio_medicao)?;
    let conf = metricas::conferir(&exec.out);

    let tamanho_medio = if plano.is_empty() {
        cfg.teste.tamanho_mensagem
    } else {
        plano.iter().map(|p| p.tamanho_mensagem).sum::<usize>() / plano.len()
    };
    let resumo = relatorio::resumir(cfg, &envios, &ag, tamanho_medio);

    if cfg.saida.formato.quer_tabela() {
        relatorio::imprimir_tabelas(cfg, plano, &envios, &ag, &conf, &resumo);
    }
    if cfg.saida.formato.quer_json() {
        let json = relatorio::montar_json(cfg, plano, &envios, &ag, &conf, &resumo);
        let destino = exec.raiz.join("relatorio.json");
        std::fs::write(&destino, serde_json::to_string_pretty(&json)?)?;
        println!("Relatório JSON: {}", destino.display());
    }

    Ok(())
}

/// Remove containers e rede criados pela ferramenta.
pub fn limpar(remover_rede: bool) {
    for nome in podman::listar_com_prefixo(PREFIXO) {
        podman::remover(&nome);
    }
    if remover_rede && podman::rede_existe(REDE) {
        podman::rede_remover(REDE);
    }
}
