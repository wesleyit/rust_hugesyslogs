//! Leitura dos logs dos receptores e agregação das métricas.
//!
//! Cada linha gravada por um receptor tem a forma:
//! `2026-09-16T12:00:00.124312-03:00 g=A s=4211 t=1758024000123456789 AAAA...`

use anyhow::{Context, Result};
use chrono::DateTime;
use hdrhistogram::Histogram;
use std::collections::BTreeMap;
use std::fs::File;
use std::io::{BufRead, BufReader};
use std::path::Path;

/// Latência máxima registrável no histograma: 60 s em microssegundos.
const LATENCIA_MAXIMA_US: u64 = 60_000_000;

pub struct EstatReceptor {
    pub nome: String,
    pub recebidas: u64,
    pub por_gerador: BTreeMap<String, u64>,
    pub latencia: Histogram<u64>,
}

impl EstatReceptor {
    fn novo(nome: String) -> Result<EstatReceptor> {
        Ok(EstatReceptor {
            nome,
            recebidas: 0,
            por_gerador: BTreeMap::new(),
            latencia: Histogram::new_with_bounds(1, LATENCIA_MAXIMA_US, 3)
                .context("não foi possível criar o histograma de latência")?,
        })
    }

    pub fn percentil_us(&self, q: f64) -> u64 {
        self.latencia.value_at_quantile(q)
    }
    pub fn maximo_us(&self) -> u64 {
        self.latencia.max()
    }
}

#[derive(Default)]
pub struct Agregado {
    pub receptores: Vec<EstatReceptor>,
    pub total_recebidas: u64,
    pub recebidas_por_gerador: BTreeMap<String, u64>,
    /// Mensagens descartadas por caírem na janela de aquecimento.
    pub descartadas_aquecimento: u64,
    /// Linhas que não puderam ser interpretadas.
    pub linhas_invalidas: u64,
    /// Latências negativas — indicam relógio inconsistente, não devem ocorrer.
    pub anomalias_relogio: u64,
}

struct Campos<'a> {
    gerador: &'a str,
    envio_ns: i128,
}

fn extrair(corpo: &str) -> Option<Campos<'_>> {
    let mut gerador = None;
    let mut envio_ns = None;
    for token in corpo.split_ascii_whitespace() {
        if let Some(v) = token.strip_prefix("g=") {
            gerador = Some(v);
        } else if let Some(v) = token.strip_prefix("t=") {
            envio_ns = v.parse::<i128>().ok();
        }
        if gerador.is_some() && envio_ns.is_some() {
            break;
        }
    }
    Some(Campos {
        gerador: gerador?,
        envio_ns: envio_ns?,
    })
}

/// Lê todos os `recv-N.log` do diretório e agrega as métricas.
///
/// `inicio_medicao` mapeia cada gerador ao instante em que seu aquecimento terminou;
/// mensagens anteriores a isso são descartadas.
pub fn coletar(
    dir_saida: &Path,
    quantidade_receptores: usize,
    inicio_medicao: &BTreeMap<String, i128>,
) -> Result<Agregado> {
    let mut ag = Agregado::default();

    for i in 1..=quantidade_receptores {
        let nome = format!("recv-{i}");
        let caminho = dir_saida.join(format!("{nome}.log"));
        let mut est = EstatReceptor::novo(nome)?;

        let arquivo = match File::open(&caminho) {
            Ok(f) => f,
            // Um receptor que não recebeu nada não gera arquivo; conta como zero.
            Err(_) => {
                ag.receptores.push(est);
                continue;
            }
        };

        let leitor = BufReader::with_capacity(1 << 20, arquivo);
        for linha in leitor.lines() {
            let linha = match linha {
                Ok(l) => l,
                Err(_) => {
                    ag.linhas_invalidas += 1;
                    continue;
                }
            };
            let linha = linha.trim();
            if linha.is_empty() {
                continue;
            }

            let Some((carimbo, corpo)) = linha.split_once(' ') else {
                ag.linhas_invalidas += 1;
                continue;
            };

            let Ok(recebido) = DateTime::parse_from_rfc3339(carimbo) else {
                ag.linhas_invalidas += 1;
                continue;
            };
            let Some(recebido_ns) = recebido.timestamp_nanos_opt() else {
                ag.linhas_invalidas += 1;
                continue;
            };

            let Some(campos) = extrair(corpo) else {
                ag.linhas_invalidas += 1;
                continue;
            };

            if let Some(limite) = inicio_medicao.get(campos.gerador) {
                if campos.envio_ns < *limite {
                    ag.descartadas_aquecimento += 1;
                    continue;
                }
            }

            let latencia_ns = recebido_ns as i128 - campos.envio_ns;
            if latencia_ns < 0 {
                ag.anomalias_relogio += 1;
            } else {
                let us = (latencia_ns / 1000).clamp(1, LATENCIA_MAXIMA_US as i128) as u64;
                let _ = est.latencia.record(us);
            }

            est.recebidas += 1;
            *est.por_gerador
                .entry(campos.gerador.to_string())
                .or_insert(0) += 1;
            ag.total_recebidas += 1;
            *ag.recebidas_por_gerador
                .entry(campos.gerador.to_string())
                .or_insert(0) += 1;
        }

        ag.receptores.push(est);
    }

    Ok(ag)
}

/// Contadores do relay extraídos do `impstats`, para conferência cruzada.
#[derive(Default, Debug)]
pub struct Conferencia {
    pub recebidas_relay: u64,
    pub encaminhadas: u64,
    pub na_fila: u64,
    pub descartadas_fila: u64,
    pub disponivel: bool,
}

/// Lê o `relay-stats.log`. Como `resetCounters` está desligado, os contadores são
/// cumulativos e basta ficar com a última amostra de cada origem.
pub fn conferir(dir_saida: &Path) -> Conferencia {
    let mut c = Conferencia::default();
    let Ok(arquivo) = File::open(dir_saida.join("relay-stats.log")) else {
        return c;
    };

    let mut recebidas: BTreeMap<String, u64> = BTreeMap::new();
    let mut encaminhadas: BTreeMap<String, u64> = BTreeMap::new();

    for linha in BufReader::new(arquivo).lines().map_while(Result::ok) {
        // As linhas do impstats trazem um prefixo syslog antes do JSON.
        let Some(inicio) = linha.find('{') else {
            continue;
        };
        let Ok(v) = serde_json::from_str::<serde_json::Value>(&linha[inicio..]) else {
            continue;
        };

        let origem = v.get("origin").and_then(|o| o.as_str()).unwrap_or("");
        let nome = v.get("name").and_then(|o| o.as_str()).unwrap_or("");
        let numero = |chave: &str| v.get(chave).and_then(|x| x.as_u64()).unwrap_or(0);

        match origem {
            "imudp" | "imtcp" => {
                if let Some(s) = v.get("submitted").and_then(|x| x.as_u64()) {
                    recebidas.insert(format!("{origem}/{nome}"), s);
                    c.disponivel = true;
                }
            }
            "omfwd" => {
                // Um contador por alvo do pool; o total é a soma deles.
                encaminhadas.insert(nome.to_string(), numero("messages.sent"));
                c.disponivel = true;
            }
            "core.queue" if nome.starts_with("action") || nome.contains("main") => {
                c.na_fila = numero("size");
                c.descartadas_fila =
                    numero("discarded.full") + numero("discarded.nf") + numero("full");
            }
            _ => {}
        }
    }

    c.recebidas_relay = recebidas.values().sum();
    c.encaminhadas = encaminhadas.values().sum();
    c
}
