//! Wrapper fino sobre a CLI do podman. Sem bindings — só `std::process::Command`.

use anyhow::{bail, Context, Result};
use std::path::Path;
use std::process::{Command, Stdio};

fn executar(args: &[&str]) -> Result<String> {
    let saida = Command::new("podman")
        .args(args)
        .output()
        .context("falha ao executar 'podman' — ele está instalado e no PATH?")?;
    if !saida.status.success() {
        bail!(
            "podman {} falhou:\n{}",
            args.join(" "),
            String::from_utf8_lossy(&saida.stderr).trim()
        );
    }
    Ok(String::from_utf8_lossy(&saida.stdout).trim().to_string())
}

fn executar_silencioso(args: &[&str]) -> bool {
    Command::new("podman")
        .args(args)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

pub fn disponivel() -> bool {
    executar_silencioso(&["--version"])
}

// ---- Rede ----

pub fn rede_existe(nome: &str) -> bool {
    executar_silencioso(&["network", "exists", nome])
}

pub fn rede_criar(nome: &str) -> Result<()> {
    if rede_existe(nome) {
        return Ok(());
    }
    executar(&["network", "create", nome])?;
    Ok(())
}

pub fn rede_remover(nome: &str) {
    let _ = executar_silencioso(&["network", "rm", "-f", nome]);
}

// ---- Imagem ----

pub fn imagem_existe(tag: &str) -> bool {
    executar_silencioso(&["image", "exists", tag])
}

pub fn construir_imagem(tag: &str, contexto: &Path, containerfile: &Path) -> Result<()> {
    let status = Command::new("podman")
        .arg("build")
        .arg("-t")
        .arg(tag)
        .arg("-f")
        .arg(containerfile)
        .arg(contexto)
        .status()
        .context("falha ao executar 'podman build'")?;
    if !status.success() {
        bail!("a construção da imagem falhou");
    }
    Ok(())
}

// ---- Containers ----

pub struct Montagem {
    pub origem: String,
    pub destino: String,
    pub somente_leitura: bool,
}

impl Montagem {
    pub fn leitura(origem: impl AsRef<Path>, destino: &str) -> Montagem {
        Montagem {
            origem: origem.as_ref().display().to_string(),
            destino: destino.to_string(),
            somente_leitura: true,
        }
    }
    pub fn escrita(origem: impl AsRef<Path>, destino: &str) -> Montagem {
        Montagem {
            origem: origem.as_ref().display().to_string(),
            destino: destino.to_string(),
            somente_leitura: false,
        }
    }
    fn como_arg(&self) -> String {
        // ",Z" reaplica o rótulo SELinux; inofensivo em sistemas sem SELinux.
        if self.somente_leitura {
            format!("{}:{}:ro,Z", self.origem, self.destino)
        } else {
            format!("{}:{}:Z", self.origem, self.destino)
        }
    }
}

pub fn subir(
    nome: &str,
    imagem: &str,
    rede: &str,
    apelido: &str,
    montagens: &[Montagem],
    comando: &[String],
) -> Result<()> {
    let mut args: Vec<String> = vec![
        "run".into(),
        "-d".into(),
        "--name".into(),
        nome.into(),
        "--network".into(),
        rede.into(),
        "--network-alias".into(),
        apelido.into(),
        "--hostname".into(),
        apelido.into(),
    ];
    for m in montagens {
        args.push("-v".into());
        args.push(m.como_arg());
    }
    args.push(imagem.into());
    args.extend(comando.iter().cloned());

    let refs: Vec<&str> = args.iter().map(String::as_str).collect();
    executar(&refs)?;
    Ok(())
}

pub fn esta_rodando(nome: &str) -> bool {
    match executar(&["inspect", "-f", "{{.State.Running}}", nome]) {
        Ok(s) => s.trim() == "true",
        Err(_) => false,
    }
}

/// Verifica se o processo dentro do container já abriu a porta.
///
/// `ListenPortFileName` do rsyslog não grava o arquivo quando a porta é fixa,
/// então a prontidão é sondada direto em /proc/net.
pub fn porta_escutando(nome: &str, porta: u16, udp: bool) -> bool {
    let hex = format!("{porta:04X}");
    // Em /proc/net/tcp o estado 0A é LISTEN; o UDP não tem estado de escuta.
    let script = if udp {
        format!("grep -qi ':{hex} ' /proc/net/udp /proc/net/udp6 2>/dev/null")
    } else {
        format!("grep -qi ':{hex} [0-9A-F]*:0000 0A' /proc/net/tcp /proc/net/tcp6 2>/dev/null")
    };
    executar_silencioso(&["exec", nome, "sh", "-c", &script])
}

pub fn codigo_saida(nome: &str) -> Option<i64> {
    executar(&["inspect", "-f", "{{.State.ExitCode}}", nome])
        .ok()
        .and_then(|s| s.trim().parse().ok())
}

pub fn logs(nome: &str) -> String {
    let saida = Command::new("podman").args(["logs", nome]).output();
    match saida {
        Ok(s) => {
            let mut txt = String::from_utf8_lossy(&s.stdout).to_string();
            txt.push_str(&String::from_utf8_lossy(&s.stderr));
            txt
        }
        Err(_) => String::new(),
    }
}

pub fn parar(nome: &str, segundos: u32) {
    let t = segundos.to_string();
    let _ = executar_silencioso(&["stop", "-t", &t, nome]);
}

pub fn remover(nome: &str) {
    let _ = executar_silencioso(&["rm", "-f", nome]);
}

/// Nomes de todos os containers criados pela ferramenta, rodando ou não.
pub fn listar_com_prefixo(prefixo: &str) -> Vec<String> {
    executar(&[
        "ps",
        "-a",
        "--filter",
        &format!("name=^{prefixo}"),
        "--format",
        "{{.Names}}",
    ])
    .map(|s| {
        s.lines()
            .map(str::trim)
            .filter(|l| !l.is_empty())
            .map(String::from)
            .collect()
    })
    .unwrap_or_default()
}
