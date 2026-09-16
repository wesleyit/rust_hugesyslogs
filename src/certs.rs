//! Geração do certificado self-signed usado no modo `certvalid`.
//!
//! No modo `anon` (padrão) nenhum certificado é necessário e este módulo não é usado.

use anyhow::{bail, Context, Result};
use std::path::Path;
use std::process::Command;

/// Gera um par cert/chave self-signed em `destino`.
///
/// `openssl req -x509` já marca `basicConstraints=CA:TRUE`, então o próprio certificado
/// serve de âncora de confiança para o relay validar a cadeia.
pub fn gerar(destino: &Path) -> Result<()> {
    std::fs::create_dir_all(destino)
        .with_context(|| format!("não foi possível criar {}", destino.display()))?;

    let cert = destino.join("cert.pem");
    let chave = destino.join("key.pem");

    let saida = Command::new("openssl")
        .args([
            "req",
            "-x509",
            "-newkey",
            "rsa:2048",
            "-nodes",
            "-days",
            "365",
            "-subj",
            "/CN=hugesyslogs",
            "-keyout",
        ])
        .arg(&chave)
        .arg("-out")
        .arg(&cert)
        .output()
        .context("falha ao executar 'openssl' — ele está instalado e no PATH?")?;

    if !saida.status.success() {
        bail!(
            "openssl falhou ao gerar o certificado:\n{}",
            String::from_utf8_lossy(&saida.stderr)
        );
    }

    // A chave privada precisa ser legível pelo rsyslog dentro do container.
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&chave, std::fs::Permissions::from_mode(0o644))
            .context("não foi possível ajustar as permissões da chave")?;
    }

    Ok(())
}
