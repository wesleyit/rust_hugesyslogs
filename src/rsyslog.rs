//! Geração das configurações rsyslog do relay e dos receptores.
//!
//! Tudo que é gerado aqui passa por `rsyslogd -N1` antes de qualquer container subir.

use crate::config::{Config, ModoTls};

/// Porta que os geradores usam para falar com o relay (TCP e UDP).
pub const PORTA_ENTRADA: u16 = 5514;
/// Porta TLS que o relay usa para falar com os receptores.
pub const PORTA_TLS: u16 = 6514;

const DIR_CERTS: &str = "/etc/hsb/certs";

/// Configuração de um receptor do pool.
///
/// `indice` é 1-based e casa com o nome DNS `recv-N` dentro da rede podman.
pub fn conf_receptor(cfg: &Config, indice: usize) -> String {
    let mut s = String::new();

    s.push_str("# Receptor gerado pelo HugeSyslogs.\n");
    s.push_str("global(\n");
    s.push_str("    workDirectory=\"/tmp\"\n");
    if cfg.tls.modo == ModoTls::Certvalid {
        s.push_str("    DefaultNetstreamDriver=\"gtls\"\n");
        s.push_str(&format!(
            "    DefaultNetstreamDriverCertFile=\"{DIR_CERTS}/cert.pem\"\n"
        ));
        s.push_str(&format!(
            "    DefaultNetstreamDriverKeyFile=\"{DIR_CERTS}/key.pem\"\n"
        ));
    }
    s.push_str(")\n\n");

    // AuthMode "anon" no receptor significa: não exigir certificado do cliente.
    s.push_str("module(load=\"imtcp\"\n");
    s.push_str("       StreamDriver.Name=\"gtls\"\n");
    s.push_str("       StreamDriver.Mode=\"1\"\n");
    s.push_str("       StreamDriver.AuthMode=\"anon\")\n\n");

    s.push_str(&format!("input(type=\"imtcp\" port=\"{PORTA_TLS}\")\n\n"));

    // Só os primeiros 90 caracteres do corpo: descarta o padding e mantém a linha
    // com tamanho fixo, independente de teste.tamanho_mensagem.
    s.push_str("template(name=\"hsb\" type=\"string\"\n");
    s.push_str("         string=\"%timegenerated:::date-rfc3339% %msg:1:90%\\n\")\n\n");

    s.push_str(&format!(
        "action(type=\"omfile\" file=\"/out/recv-{indice}.log\" template=\"hsb\"\n\
         \x20      asyncWriting=\"on\" ioBufferSize=\"64k\" flushOnTXEnd=\"off\")\n"
    ));

    s
}

/// Configuração do relay: recebe TCP e UDP, encaminha por TLS em round-robin.
pub fn conf_relay(cfg: &Config) -> String {
    let alvos = cfg
        .nomes_receptores()
        .iter()
        .map(|n| format!("\"{n}\""))
        .collect::<Vec<_>>()
        .join(", ");

    let mut s = String::new();

    s.push_str("# Relay gerado pelo HugeSyslogs.\n");
    s.push_str("global(\n");
    s.push_str("    workDirectory=\"/tmp\"\n");
    if cfg.tls.modo == ModoTls::Certvalid {
        s.push_str("    DefaultNetstreamDriver=\"gtls\"\n");
        s.push_str(&format!(
            "    DefaultNetstreamDriverCAFile=\"{DIR_CERTS}/cert.pem\"\n"
        ));
    }
    s.push_str(")\n\n");

    s.push_str("module(load=\"impstats\" interval=\"5\" format=\"json\"\n");
    s.push_str("       log.file=\"/out/relay-stats.log\" log.syslog=\"off\" resetCounters=\"off\")\n\n");

    s.push_str("module(load=\"imudp\")\n");
    s.push_str(&format!(
        "input(type=\"imudp\" port=\"{PORTA_ENTRADA}\" rcvbufSize=\"16m\")\n\n"
    ));

    s.push_str("module(load=\"imtcp\")\n");
    s.push_str(&format!(
        "input(type=\"imtcp\" port=\"{PORTA_ENTRADA}\")\n\n"
    ));

    let auth = match cfg.tls.modo {
        ModoTls::Anon => "anon",
        ModoTls::Certvalid => "x509/certvalid",
    };

    s.push_str("action(\n");
    s.push_str("    type=\"omfwd\"\n");
    s.push_str(&format!("    target=[{alvos}]\n"));
    s.push_str(&format!("    port=\"{PORTA_TLS}\"\n"));
    s.push_str("    protocol=\"tcp\"\n");
    s.push_str("    StreamDriver=\"gtls\"\n");
    s.push_str("    StreamDriverMode=\"1\"\n");
    s.push_str(&format!("    StreamDriverAuthMode=\"{auth}\"\n"));
    s.push_str("    TCP_Framing=\"octet-counted\"\n");
    s.push_str("    queue.type=\"linkedList\"\n");
    s.push_str(&format!("    queue.size=\"{}\"\n", cfg.relay.tamanho_fila));
    s.push_str(&format!(
        "    queue.workerThreads=\"{}\"\n",
        cfg.relay.workers_fila
    ));
    if cfg.relay.rebind_interval > 0 {
        s.push_str(&format!(
            "    rebindInterval=\"{}\"\n",
            cfg.relay.rebind_interval
        ));
    }
    s.push_str(")\n");

    s
}
