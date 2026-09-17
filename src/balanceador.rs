//! Geração da configuração do balanceador L4 (nginx com o módulo `stream`).
//!
//! Substitui o relay rsyslog quando a flag `--balanceador` é usada. Diferença
//! essencial de comportamento: o relay distribui **por mensagem**, enquanto o
//! balanceador decide **por conexão**, no momento do handshake, e mantém aquele
//! fluxo preso ao mesmo receptor até a conexão morrer.

use crate::config::Config;
use crate::rsyslog::{PORTA_ENTRADA, PORTA_PLAIN};

/// Caminho do módulo `stream`, que no Ubuntu vem como objeto dinâmico.
const MODULO_STREAM: &str = "/usr/lib/nginx/modules/ngx_stream_module.so";

/// Gera a configuração do nginx em modo L4 puro, sem TLS em nenhuma das pernas.
///
/// O hash usa `$remote_addr$remote_port`. Como o IP e a porta de destino e o
/// protocolo são constantes nesta topologia, três dos cinco elementos da 5-tupla
/// não variam — hashear origem e porta de origem **é** o hash de 5 tuplas aqui.
pub fn conf_nginx(cfg: &Config) -> String {
    let receptores = cfg.nomes_receptores();

    let mut s = String::new();
    s.push_str("# Balanceador L4 gerado pelo HugeSyslogs.\n");
    s.push_str(&format!("load_module {MODULO_STREAM};\n\n"));
    s.push_str("daemon off;\n");
    s.push_str("worker_processes auto;\n");
    s.push_str("error_log /out/lb-erros.log warn;\n");
    s.push_str("pid /tmp/nginx.pid;\n\n");
    s.push_str("events {\n    worker_connections 8192;\n}\n\n");

    s.push_str("stream {\n");

    // Upstreams separados para TCP e UDP: o nginx nao permite reusar o mesmo
    // bloco entre um listener de fluxo e um de datagrama.
    for (nome, comentario) in [
        ("receptores_tcp", "TCP em texto puro"),
        ("receptores_udp", "UDP em texto puro"),
    ] {
        s.push_str(&format!("    # {comentario}\n"));
        s.push_str(&format!("    upstream {nome} {{\n"));
        s.push_str("        hash $remote_addr$remote_port;\n");
        for r in &receptores {
            s.push_str(&format!("        server {r}:{PORTA_PLAIN};\n"));
        }
        s.push_str("    }\n\n");
    }

    s.push_str("    server {\n");
    s.push_str(&format!("        listen {PORTA_ENTRADA};\n"));
    s.push_str("        proxy_pass receptores_tcp;\n");
    s.push_str("        proxy_timeout 300s;\n");
    s.push_str("    }\n\n");

    s.push_str("    server {\n");
    s.push_str(&format!("        listen {PORTA_ENTRADA} udp;\n"));
    // Syslog em UDP nao responde; sem isso o nginx ficaria esperando resposta.
    s.push_str("        proxy_responses 0;\n");
    s.push_str("        proxy_pass receptores_udp;\n");
    s.push_str("        proxy_timeout 60s;\n");
    s.push_str("    }\n");

    s.push_str("}\n");
    s
}
