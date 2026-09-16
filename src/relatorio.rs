//! Formatação dos resultados: tabelas no terminal e JSON.

use crate::config::{Config, GeradorPlano};
use crate::metricas::{Agregado, Conferencia};
use serde_json::json;
use std::collections::BTreeMap;

/// Dados de envio de um gerador, lidos do JSON que ele grava em `/out`.
#[derive(Debug, Clone)]
pub struct EnvioGerador {
    pub nome: String,
    pub taxa_alvo: u64,
    pub enviadas_uteis: u64,
    pub taxa_real: f64,
    pub erros_envio: u64,
    pub janela_s: f64,
}

// ---- Formatação numérica em pt-BR ----

/// Separa os milhares com ponto: `3000000` -> `3.000.000`.
pub fn num(n: u64) -> String {
    let d = n.to_string();
    let bytes = d.as_bytes();
    let mut saida = String::with_capacity(d.len() + d.len() / 3);
    for (i, b) in bytes.iter().enumerate() {
        if i > 0 && (bytes.len() - i).is_multiple_of(3) {
            saida.push('.');
        }
        saida.push(*b as char);
    }
    saida
}

/// Percentual com duas casas e vírgula decimal.
pub fn pct(v: f64) -> String {
    format!("{:.2}%", v).replace('.', ",")
}

fn pct_sinal(v: f64) -> String {
    let s = format!("{:+.2}%", v).replace('.', ",");
    s
}

/// Latência em microssegundos para uma unidade legível.
pub fn lat(us: u64) -> String {
    if us == 0 {
        "-".into()
    } else if us < 1_000 {
        format!("{}µs", us)
    } else if us < 1_000_000 {
        format!("{:.1}ms", us as f64 / 1000.0).replace('.', ",")
    } else {
        format!("{:.2}s", us as f64 / 1_000_000.0).replace('.', ",")
    }
}

/// Decimal com vírgula, no padrão pt-BR.
pub fn dec(v: f64, casas: usize) -> String {
    format!("{v:.casas$}").replace('.', ",")
}

fn milhares_f(v: f64) -> String {
    if v >= 1000.0 {
        format!("{:.1}k", v / 1000.0).replace('.', ",")
    } else {
        format!("{v:.0}")
    }
}

pub enum Veredito {
    Balanceado,
    DesvioLeve,
    Desbalanceado,
}

impl Veredito {
    fn avaliar(desvio_maximo: f64) -> Veredito {
        if desvio_maximo <= 1.0 {
            Veredito::Balanceado
        } else if desvio_maximo <= 5.0 {
            Veredito::DesvioLeve
        } else {
            Veredito::Desbalanceado
        }
    }
    fn texto(&self) -> &'static str {
        match self {
            Veredito::Balanceado => "BALANCEADO",
            Veredito::DesvioLeve => "DESVIO LEVE",
            Veredito::Desbalanceado => "DESBALANCEADO",
        }
    }
}

pub struct Resumo {
    pub enviadas: u64,
    pub recebidas: u64,
    pub perdidas: i64,
    pub perda_pct: f64,
    pub vazao: f64,
    pub mb_s: f64,
    pub janela_s: f64,
    pub desvio_maximo: f64,
}

pub fn resumir(
    cfg: &Config,
    envios: &[EnvioGerador],
    ag: &Agregado,
    tamanho_medio: usize,
) -> Resumo {
    let enviadas: u64 = envios.iter().map(|e| e.enviadas_uteis).sum();
    let recebidas = ag.total_recebidas;
    let perdidas = enviadas as i64 - recebidas as i64;
    let perda_pct = if enviadas > 0 {
        perdidas as f64 / enviadas as f64 * 100.0
    } else {
        0.0
    };
    let janela_s = envios
        .iter()
        .map(|e| e.janela_s)
        .fold(0.0_f64, |a, b| a.max(b));
    let vazao = if janela_s > 0.0 {
        recebidas as f64 / janela_s
    } else {
        0.0
    };

    let n = cfg.receptores.quantidade.max(1) as f64;
    let ideal = 100.0 / n;
    let desvio_maximo = ag
        .receptores
        .iter()
        .map(|r| {
            let fatia = if recebidas > 0 {
                r.recebidas as f64 / recebidas as f64 * 100.0
            } else {
                0.0
            };
            (fatia - ideal).abs()
        })
        .fold(0.0_f64, f64::max);

    Resumo {
        enviadas,
        recebidas,
        perdidas,
        perda_pct,
        vazao,
        mb_s: vazao * tamanho_medio as f64 / 1_048_576.0,
        janela_s,
        desvio_maximo,
    }
}

pub fn imprimir_tabelas(
    cfg: &Config,
    plano: &[GeradorPlano],
    envios: &[EnvioGerador],
    ag: &Agregado,
    conf: &Conferencia,
    resumo: &Resumo,
) {
    println!();
    println!("=== RESUMO ===");
    println!(
        "duração útil: {}s   enviadas: {}   recebidas: {}",
        dec(resumo.janela_s, 1),
        num(resumo.enviadas),
        num(resumo.recebidas)
    );
    let perda_txt = if resumo.perdidas >= 0 {
        format!("{} ({})", num(resumo.perdidas as u64), pct(resumo.perda_pct))
    } else {
        format!(
            "{} duplicadas ({})",
            num(resumo.perdidas.unsigned_abs()),
            pct(-resumo.perda_pct)
        )
    };
    println!(
        "perda: {}   vazão: {} msg/s | {} MB/s",
        perda_txt,
        milhares_f(resumo.vazao),
        dec(resumo.mb_s, 1)
    );

    // ---- Balanceamento ----
    println!();
    println!("=== POR RECEPTOR (balanceamento) ===");
    println!(
        "{:<10} {:>12} {:>9} {:>9} {:>9} {:>9} {:>9}",
        "receptor", "mensagens", "fatia", "desvio", "p50", "p99", "máx"
    );
    let ideal = 100.0 / cfg.receptores.quantidade.max(1) as f64;
    for r in &ag.receptores {
        let fatia = if resumo.recebidas > 0 {
            r.recebidas as f64 / resumo.recebidas as f64 * 100.0
        } else {
            0.0
        };
        println!(
            "{:<10} {:>12} {:>9} {:>9} {:>9} {:>9} {:>9}",
            r.nome,
            num(r.recebidas),
            pct(fatia),
            pct_sinal(fatia - ideal),
            lat(r.percentil_us(0.50)),
            lat(r.percentil_us(0.99)),
            lat(r.maximo_us()),
        );
    }
    println!("{}", "-".repeat(72));
    let v = Veredito::avaliar(resumo.desvio_maximo);
    println!(
        "desvio máximo: {}   ->  {}",
        pct(resumo.desvio_maximo),
        v.texto()
    );

    // ---- Geradores ----
    println!();
    println!("=== POR GERADOR ===");
    println!(
        "{:<10} {:>6} {:>7} {:>12} {:>12} {:>20}",
        "gerador", "proto", "fatia", "enviadas", "recebidas", "perdidas"
    );
    let por_nome: BTreeMap<&str, &EnvioGerador> =
        envios.iter().map(|e| (e.nome.as_str(), e)).collect();

    for p in plano {
        let Some(e) = por_nome.get(p.nome.as_str()) else {
            println!(
                "{:<10} {:>6} {:>7} {:>12} {:>12} {:>20}",
                p.nome,
                p.proto.texto(),
                "-",
                "sem relatório",
                "-",
                "-"
            );
            continue;
        };
        let recebidas = ag
            .recebidas_por_gerador
            .get(p.nome.as_str())
            .copied()
            .unwrap_or(0);
        let perdidas = e.enviadas_uteis as i64 - recebidas as i64;
        let pp = if e.enviadas_uteis > 0 {
            perdidas as f64 / e.enviadas_uteis as f64 * 100.0
        } else {
            0.0
        };
        let fatia_txt = if p.taxa_explicita {
            "fixa".to_string()
        } else {
            pct(p.fatia * 100.0)
        };
        println!(
            "{:<10} {:>6} {:>7} {:>12} {:>12} {:>20}",
            p.nome,
            p.proto.texto(),
            fatia_txt,
            num(e.enviadas_uteis),
            num(recebidas),
            format!("{} ({})", num(perdidas.unsigned_abs()), pct(pp)),
        );
    }

    // ---- Origem e relay ----
    if !ag.origens.is_empty() {
        println!();
        println!("=== ORIGEM x RELAY (cadeia vista pelo receptor) ===");
        println!(
            "{:<20} {:<20} {:>12} {:>9}",
            "origem (emissor)", "relay (encaminhou)", "mensagens", "fatia"
        );
        for ((origem, relay), n) in &ag.origens {
            let fatia = if resumo.recebidas > 0 {
                *n as f64 / resumo.recebidas as f64 * 100.0
            } else {
                0.0
            };
            println!(
                "{:<20} {:<20} {:>12} {:>9}",
                origem,
                relay,
                num(*n),
                pct(fatia)
            );
        }
    }

    // ---- Avisos ----
    let mut avisos: Vec<String> = Vec::new();
    for e in envios {
        if e.taxa_alvo > 0 && e.taxa_real < e.taxa_alvo as f64 * 0.95 {
            avisos.push(format!(
                "gerador {} emitiu {} msg/s, abaixo dos {} configurados — o gargalo pode ser o próprio gerador",
                e.nome,
                milhares_f(e.taxa_real),
                milhares_f(e.taxa_alvo as f64)
            ));
        }
        if e.erros_envio > 0 {
            avisos.push(format!(
                "gerador {} acumulou {} erros de envio",
                e.nome,
                num(e.erros_envio)
            ));
        }
    }
    if ag.anomalias_relogio > 0 {
        avisos.push(format!(
            "{} mensagens com latência negativa (relógio inconsistente)",
            num(ag.anomalias_relogio)
        ));
    }
    if ag.linhas_invalidas > 0 {
        avisos.push(format!(
            "{} linhas ilegíveis nos logs dos receptores",
            num(ag.linhas_invalidas)
        ));
    }
    if !avisos.is_empty() {
        println!();
        println!("=== AVISOS ===");
        for a in avisos {
            println!("  ! {a}");
        }
    }

    // ---- Conferência ----
    if conf.disponivel {
        println!();
        println!("=== CONFERÊNCIA (impstats do relay, inclui o aquecimento) ===");
        println!(
            "recebidas pelo relay: {}   encaminhadas: {}   na fila ao final: {}",
            num(conf.recebidas_relay),
            num(conf.encaminhadas),
            num(conf.na_fila)
        );
        if conf.descartadas_fila > 0 {
            println!(
                "  ! o relay descartou {} mensagens por fila cheia",
                num(conf.descartadas_fila)
            );
        }
    }
    println!();
}

pub fn montar_json(
    cfg: &Config,
    plano: &[GeradorPlano],
    envios: &[EnvioGerador],
    ag: &Agregado,
    conf: &Conferencia,
    resumo: &Resumo,
) -> serde_json::Value {
    let por_nome: BTreeMap<&str, &EnvioGerador> =
        envios.iter().map(|e| (e.nome.as_str(), e)).collect();
    let ideal = 100.0 / cfg.receptores.quantidade.max(1) as f64;

    let receptores: Vec<_> = ag
        .receptores
        .iter()
        .map(|r| {
            let fatia = if resumo.recebidas > 0 {
                r.recebidas as f64 / resumo.recebidas as f64 * 100.0
            } else {
                0.0
            };
            json!({
                "nome": r.nome,
                "recebidas": r.recebidas,
                "fatia_pct": fatia,
                "desvio_pct": fatia - ideal,
                "latencia_us": {
                    "p50": r.percentil_us(0.50),
                    "p90": r.percentil_us(0.90),
                    "p99": r.percentil_us(0.99),
                    "max": r.maximo_us(),
                },
                "por_gerador": r.por_gerador,
            })
        })
        .collect();

    let geradores: Vec<_> = plano
        .iter()
        .map(|p| {
            let e = por_nome.get(p.nome.as_str());
            let recebidas = ag
                .recebidas_por_gerador
                .get(p.nome.as_str())
                .copied()
                .unwrap_or(0);
            let enviadas = e.map(|x| x.enviadas_uteis).unwrap_or(0);
            json!({
                "nome": p.nome,
                "proto": p.proto.texto(),
                "peso": p.peso,
                "fatia_pct": p.fatia * 100.0,
                "taxa_alvo": p.taxa,
                "taxa_real": e.map(|x| x.taxa_real).unwrap_or(0.0),
                "enviadas": enviadas,
                "recebidas": recebidas,
                "perdidas": enviadas as i64 - recebidas as i64,
                "erros_envio": e.map(|x| x.erros_envio).unwrap_or(0),
            })
        })
        .collect();

    let origens: Vec<_> = ag
        .origens
        .iter()
        .map(|((origem, relay), n)| {
            json!({
                "origem": origem,
                "relay": relay,
                "mensagens": n,
                "fatia_pct": if resumo.recebidas > 0 {
                    *n as f64 / resumo.recebidas as f64 * 100.0
                } else {
                    0.0
                },
            })
        })
        .collect();

    json!({
        "resumo": {
            "janela_s": resumo.janela_s,
            "enviadas": resumo.enviadas,
            "recebidas": resumo.recebidas,
            "perdidas": resumo.perdidas,
            "perda_pct": resumo.perda_pct,
            "vazao_msg_s": resumo.vazao,
            "vazao_mb_s": resumo.mb_s,
            "desvio_maximo_pct": resumo.desvio_maximo,
            "veredito": Veredito::avaliar(resumo.desvio_maximo).texto(),
        },
        "receptores": receptores,
        "geradores": geradores,
        "origens": origens,
        "conferencia": {
            "disponivel": conf.disponivel,
            "recebidas_relay": conf.recebidas_relay,
            "encaminhadas": conf.encaminhadas,
            "na_fila": conf.na_fila,
            "descartadas_fila": conf.descartadas_fila,
        },
        "diagnostico": {
            "descartadas_aquecimento": ag.descartadas_aquecimento,
            "linhas_invalidas": ag.linhas_invalidas,
            "anomalias_relogio": ag.anomalias_relogio,
        }
    })
}

#[cfg(test)]
mod testes {
    use super::*;

    #[test]
    fn formata_milhares_com_ponto() {
        assert_eq!(num(0), "0");
        assert_eq!(num(999), "999");
        assert_eq!(num(1_000), "1.000");
        assert_eq!(num(3_000_000), "3.000.000");
    }

    #[test]
    fn formata_percentual_com_virgula() {
        assert_eq!(pct(25.0), "25,00%");
        assert_eq!(pct_sinal(0.01), "+0,01%");
    }

    #[test]
    fn formata_latencia_por_faixa() {
        assert_eq!(lat(500), "500µs");
        assert_eq!(lat(1_200), "1,2ms");
    }
}
