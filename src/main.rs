//! HugeSyslogs — testes de desempenho e balanceamento de relay rsyslog em round-robin.

mod balanceador;
mod certs;
mod config;
mod gerador;
mod metricas;
mod orquestrador;
mod podman;
mod relatorio;
mod rsyslog;

use anyhow::{Context, Result};
use clap::{Parser, Subcommand};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};

/// Prefixo dos containers, usado também para limpeza.
pub const PREFIXO: &str = "hsb-";
pub const REDE: &str = "hugesyslogs-rede";
pub const IMAGEM_PADRAO: &str = "localhost/hugesyslogs:latest";

pub static INTERROMPIDO: AtomicBool = AtomicBool::new(false);

#[derive(Parser)]
#[command(
    name = "hugesyslogs",
    about = "Testes de desempenho e balanceamento de relay rsyslog em round-robin",
    version
)]
struct Cli {
    #[command(subcommand)]
    comando: Comando,
}

#[derive(Subcommand)]
enum Comando {
    /// Valida o config.toml e as configurações rsyslog geradas.
    Validar {
        #[arg(short, long, default_value = "config.toml")]
        config: PathBuf,
        #[arg(long, default_value = IMAGEM_PADRAO)]
        imagem: String,
        /// Valida também a configuração do balanceador nginx.
        #[arg(long)]
        balanceador: bool,
    },
    /// Constrói a imagem de container usada pelos três papéis.
    Imagem {
        #[arg(long, default_value = IMAGEM_PADRAO)]
        imagem: String,
    },
    /// Executa o ciclo completo do teste.
    Executar {
        #[arg(short, long, default_value = "config.toml")]
        config: PathBuf,
        #[arg(long, default_value = IMAGEM_PADRAO)]
        imagem: String,
        /// Não remove os containers ao final, para inspeção.
        #[arg(long)]
        manter: bool,
        /// Troca o relay rsyslog por um balanceador nginx L4 com hash de 5 tuplas.
        ///
        /// A distribuição passa a ser por conexão em vez de por mensagem, e o
        /// tráfego até os receptores vai em texto puro, sem TLS.
        #[arg(long)]
        balanceador: bool,
    },
    /// Remove containers e rede deixados para trás.
    Limpar,
    /// Modo interno: injeta carga. Roda dentro do container.
    Gerar(gerador::ArgsGerador),
}

fn main() {
    if let Err(e) = executar() {
        eprintln!("\nerro: {e:#}");
        std::process::exit(1);
    }
}

fn executar() -> Result<()> {
    let cli = Cli::parse();

    // O gerador instala o próprio tratador; nos demais comandos, Ctrl-C leva à limpeza.
    if !matches!(cli.comando, Comando::Gerar(_)) {
        ctrlc::set_handler(|| {
            if INTERROMPIDO.swap(true, Ordering::Relaxed) {
                // Segundo Ctrl-C: sai na hora.
                std::process::exit(130);
            }
            eprintln!("\ninterrompendo... (Ctrl-C de novo para forçar)");
        })
        .context("não foi possível instalar o tratador de Ctrl-C")?;
    }

    match cli.comando {
        Comando::Validar {
            config,
            imagem,
            balanceador,
        } => validar(&config, &imagem, balanceador),
        Comando::Imagem { imagem } => construir_imagem(&imagem),
        Comando::Executar {
            config,
            imagem,
            manter,
            balanceador,
        } => {
            let cfg = config::Config::carregar(&config)?;
            let distribuicao = if balanceador {
                orquestrador::Distribuicao::Balanceador
            } else {
                orquestrador::Distribuicao::Rsyslog
            };
            let r = orquestrador::executar(&cfg, &imagem, manter, distribuicao);
            if INTERROMPIDO.load(Ordering::Relaxed) {
                orquestrador::limpar(true);
            }
            r
        }
        Comando::Limpar => {
            println!("Removendo containers e rede do HugeSyslogs...");
            orquestrador::limpar(true);
            println!("Pronto.");
            Ok(())
        }
        Comando::Gerar(args) => gerador::executar(args),
    }
}

fn validar(caminho: &Path, imagem: &str, balanceador: bool) -> Result<()> {
    let cfg = config::Config::carregar(caminho)?;
    let plano = cfg.plano();
    let distribuicao = if balanceador {
        orquestrador::Distribuicao::Balanceador
    } else {
        orquestrador::Distribuicao::Rsyslog
    };

    println!("Configuração válida: {}", caminho.display());
    println!("Distribuição: {}", distribuicao.texto());
    println!(
        "\nDuração {}s (aquecimento {}s, drenagem {}s)",
        relatorio::dec(cfg.teste.duracao.as_secs_f64(), 0),
        relatorio::dec(cfg.teste.aquecimento.as_secs_f64(), 0),
        relatorio::dec(cfg.teste.drenagem.as_secs_f64(), 0)
    );
    println!(
        "Receptores: {}   TLS: {:?}   Fila do relay: {} ({} worker(s))",
        cfg.receptores.quantidade,
        cfg.tls.modo,
        relatorio::num(cfg.relay.tamanho_fila),
        cfg.relay.workers_fila
    );

    println!("\n=== PLANO DE CARGA ===");
    println!(
        "{:<10} {:>6} {:>8} {:>9} {:>12} {:>9} {:>9}",
        "gerador", "proto", "peso", "fatia", "msgs/s", "threads", "bytes"
    );
    let mut total = 0u64;
    for p in &plano {
        total += p.taxa;
        let fatia = if p.taxa_explicita {
            "fixa".to_string()
        } else {
            relatorio::pct(p.fatia * 100.0)
        };
        println!(
            "{:<10} {:>6} {:>8} {:>9} {:>12} {:>9} {:>9}",
            p.nome,
            p.proto.texto(),
            format!("{:.0}", p.peso),
            fatia,
            relatorio::num(p.taxa),
            p.threads,
            p.tamanho_mensagem
        );
    }
    println!("{}", "-".repeat(70));
    println!("{:<10} {:>37}", "TOTAL", relatorio::num(total));

    // A validação do rsyslogd precisa da imagem; sem ela, só as confs são escritas.
    let exec = orquestrador::Execucao::criar(&cfg.saida.diretorio.join("validacao"))?;
    println!("\n=== CONFIGURAÇÕES ===");
    orquestrador::gerar_e_validar_confs(&cfg, &exec, imagem, distribuicao)?;
    println!("  · gravadas em {}", exec.conf.display());

    Ok(())
}

fn construir_imagem(imagem: &str) -> Result<()> {
    let raiz = std::env::current_dir()?;
    let containerfile = raiz.join("Containerfile");
    if !containerfile.exists() {
        anyhow::bail!(
            "Containerfile não encontrado em {} — rode a partir da raiz do repositório",
            raiz.display()
        );
    }
    println!("Construindo {imagem}...");
    podman::construir_imagem(imagem, &raiz, &containerfile)?;
    println!("Imagem {imagem} pronta.");
    Ok(())
}
