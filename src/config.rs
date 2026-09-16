//! Leitura, validação e normalização do arquivo `config.toml`.

use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};
use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::time::Duration;

/// Menor tamanho de mensagem que ainda comporta o cabeçalho RFC5424 e os campos de medição.
pub const TAMANHO_MINIMO: usize = 128;

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Config {
    pub teste: Teste,
    #[serde(default)]
    pub relay: Relay,
    pub receptores: Receptores,
    #[serde(default)]
    pub tls: Tls,
    #[serde(rename = "gerador")]
    pub geradores: Vec<Gerador>,
    #[serde(default)]
    pub saida: Saida,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Teste {
    #[serde(deserialize_with = "de_duracao")]
    pub duracao: Duration,
    #[serde(default, deserialize_with = "de_duracao_opcional")]
    pub aquecimento: Duration,
    #[serde(default = "drenagem_padrao", deserialize_with = "de_duracao")]
    pub drenagem: Duration,
    #[serde(default = "tamanho_padrao")]
    pub tamanho_mensagem: usize,
    pub taxa: u64,
    #[serde(default = "threads_padrao")]
    pub threads: usize,
    #[serde(default)]
    pub total_mensagens: u64,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Relay {
    #[serde(default = "fila_padrao")]
    pub tamanho_fila: u64,
    #[serde(default = "workers_padrao")]
    pub workers_fila: usize,
    #[serde(default)]
    pub rebind_interval: u64,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Receptores {
    pub quantidade: usize,
}

#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Tls {
    #[serde(default)]
    pub modo: ModoTls,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum ModoTls {
    /// Criptografa sem verificar nada. Nenhum certificado em lugar algum.
    #[default]
    Anon,
    /// Receptores apresentam certificado self-signed; o relay valida só a cadeia.
    Certvalid,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Gerador {
    pub nome: String,
    pub proto: Proto,
    #[serde(default = "peso_padrao")]
    pub peso: f64,
    pub threads: Option<usize>,
    pub tamanho_mensagem: Option<usize>,
    pub taxa: Option<u64>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Proto {
    Tcp,
    Udp,
}

impl Proto {
    pub fn texto(self) -> &'static str {
        match self {
            Proto::Tcp => "tcp",
            Proto::Udp => "udp",
        }
    }
}

impl std::str::FromStr for Proto {
    type Err = String;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.to_ascii_lowercase().as_str() {
            "tcp" => Ok(Proto::Tcp),
            "udp" => Ok(Proto::Udp),
            outro => Err(format!("protocolo inválido: {outro:?} (use tcp ou udp)")),
        }
    }
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Saida {
    #[serde(default = "diretorio_padrao")]
    pub diretorio: PathBuf,
    #[serde(default)]
    pub formato: Formato,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum Formato {
    #[default]
    Tabela,
    Json,
    Ambos,
}

impl Formato {
    pub fn quer_tabela(self) -> bool {
        matches!(self, Formato::Tabela | Formato::Ambos)
    }
    pub fn quer_json(self) -> bool {
        matches!(self, Formato::Json | Formato::Ambos)
    }
}

// ---- Padrões ----

fn drenagem_padrao() -> Duration {
    Duration::from_secs(10)
}
fn tamanho_padrao() -> usize {
    512
}
fn threads_padrao() -> usize {
    4
}
fn fila_padrao() -> u64 {
    100_000
}
fn workers_padrao() -> usize {
    1
}
fn peso_padrao() -> f64 {
    1.0
}
fn diretorio_padrao() -> PathBuf {
    PathBuf::from("./resultados")
}

impl Default for Relay {
    fn default() -> Self {
        Relay {
            tamanho_fila: fila_padrao(),
            workers_fila: workers_padrao(),
            rebind_interval: 0,
        }
    }
}

impl Default for Saida {
    fn default() -> Self {
        Saida {
            diretorio: diretorio_padrao(),
            formato: Formato::default(),
        }
    }
}

// ---- Duração ----

/// Aceita `"500ms"`, `"60s"`, `"5m"`, `"1h"` ou um número puro (segundos).
pub fn analisar_duracao(texto: &str) -> Result<Duration, String> {
    let t = texto.trim();
    if t.is_empty() {
        return Err("duração vazia".into());
    }
    // "ms" precisa ser testado antes de "s", senão o sufixo colide.
    let (numero, fator) = if let Some(v) = t.strip_suffix("ms") {
        (v, 0.001)
    } else if let Some(v) = t.strip_suffix('s') {
        (v, 1.0)
    } else if let Some(v) = t.strip_suffix('m') {
        (v, 60.0)
    } else if let Some(v) = t.strip_suffix('h') {
        (v, 3600.0)
    } else {
        (t, 1.0)
    };
    let n: f64 = numero
        .trim()
        .parse()
        .map_err(|_| format!("duração inválida: {texto:?}"))?;
    if !n.is_finite() || n < 0.0 {
        return Err(format!("duração inválida: {texto:?}"));
    }
    Ok(Duration::from_secs_f64(n * fator))
}

fn de_duracao<'de, D>(d: D) -> Result<Duration, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let s = String::deserialize(d)?;
    analisar_duracao(&s).map_err(serde::de::Error::custom)
}

fn de_duracao_opcional<'de, D>(d: D) -> Result<Duration, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let s = Option::<String>::deserialize(d)?;
    match s {
        None => Ok(Duration::ZERO),
        Some(v) => analisar_duracao(&v).map_err(serde::de::Error::custom),
    }
}

// ---- Plano resolvido ----

/// Um gerador com todos os parâmetros já resolvidos (peso convertido em taxa absoluta).
#[derive(Debug, Clone)]
pub struct GeradorPlano {
    pub nome: String,
    pub proto: Proto,
    pub peso: f64,
    pub fatia: f64,
    pub taxa: u64,
    pub threads: usize,
    pub tamanho_mensagem: usize,
    pub taxa_explicita: bool,
}

impl GeradorPlano {
    pub fn container(&self) -> String {
        format!("{}gen-{}", crate::PREFIXO, self.nome)
    }
}

impl Config {
    pub fn carregar(caminho: &Path) -> Result<Config> {
        let bruto = std::fs::read_to_string(caminho)
            .with_context(|| format!("não foi possível ler {}", caminho.display()))?;
        let cfg: Config = toml::from_str(&bruto)
            .with_context(|| format!("erro de sintaxe em {}", caminho.display()))?;
        cfg.validar()?;
        Ok(cfg)
    }

    fn validar(&self) -> Result<()> {
        if self.geradores.is_empty() {
            bail!("é preciso declarar ao menos um [[gerador]]");
        }
        if self.receptores.quantidade == 0 {
            bail!("receptores.quantidade precisa ser no mínimo 1");
        }
        if self.teste.threads == 0 {
            bail!("teste.threads precisa ser no mínimo 1");
        }
        if self.teste.tamanho_mensagem < TAMANHO_MINIMO {
            bail!(
                "teste.tamanho_mensagem = {} é menor que o mínimo de {} bytes",
                self.teste.tamanho_mensagem,
                TAMANHO_MINIMO
            );
        }
        if self.teste.total_mensagens == 0 && self.teste.duracao.is_zero() {
            bail!("com total_mensagens = 0, teste.duracao precisa ser maior que zero");
        }
        if self.teste.aquecimento >= self.teste.duracao && self.teste.total_mensagens == 0 {
            bail!("teste.aquecimento precisa ser menor que teste.duracao");
        }
        if self.relay.workers_fila == 0 {
            bail!("relay.workers_fila precisa ser no mínimo 1");
        }

        let mut vistos = HashSet::new();
        for g in &self.geradores {
            if g.nome.trim().is_empty() {
                bail!("todo [[gerador]] precisa de um nome");
            }
            if g.nome.len() > 16 {
                bail!("nome de gerador {:?} excede 16 caracteres", g.nome);
            }
            if !g
                .nome
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
            {
                bail!(
                    "nome de gerador {:?} só pode conter letras, números, '-' e '_'",
                    g.nome
                );
            }
            if !vistos.insert(g.nome.as_str()) {
                bail!("nome de gerador duplicado: {:?}", g.nome);
            }
            if let Some(t) = g.threads {
                if t == 0 {
                    bail!("gerador {:?}: threads precisa ser no mínimo 1", g.nome);
                }
            }
            if let Some(t) = g.tamanho_mensagem {
                if t < TAMANHO_MINIMO {
                    bail!(
                        "gerador {:?}: tamanho_mensagem = {t} é menor que o mínimo de {TAMANHO_MINIMO} bytes",
                        g.nome
                    );
                }
            }
            if g.taxa.is_none() && g.peso <= 0.0 {
                bail!(
                    "gerador {:?}: peso precisa ser maior que zero (ou defina uma taxa absoluta)",
                    g.nome
                );
            }
        }

        let algum_por_peso = self.geradores.iter().any(|g| g.taxa.is_none());
        if algum_por_peso && self.teste.taxa == 0 {
            bail!("teste.taxa precisa ser maior que zero para repartir entre os geradores por peso");
        }

        Ok(())
    }

    /// Converte pesos em taxas absolutas. Geradores com `taxa` explícita a mantêm;
    /// os demais repartem `teste.taxa` proporcionalmente aos seus pesos.
    pub fn plano(&self) -> Vec<GeradorPlano> {
        let soma_pesos: f64 = self
            .geradores
            .iter()
            .filter(|g| g.taxa.is_none())
            .map(|g| g.peso)
            .sum();

        self.geradores
            .iter()
            .map(|g| {
                let fatia = if g.taxa.is_some() || soma_pesos <= 0.0 {
                    0.0
                } else {
                    g.peso / soma_pesos
                };
                let taxa = match g.taxa {
                    Some(t) => t,
                    None => (self.teste.taxa as f64 * fatia).round() as u64,
                };
                GeradorPlano {
                    nome: g.nome.clone(),
                    proto: g.proto,
                    peso: g.peso,
                    fatia,
                    taxa,
                    threads: g.threads.unwrap_or(self.teste.threads),
                    tamanho_mensagem: g.tamanho_mensagem.unwrap_or(self.teste.tamanho_mensagem),
                    taxa_explicita: g.taxa.is_some(),
                }
            })
            .collect()
    }

    /// Nomes DNS dos receptores dentro da rede podman.
    pub fn nomes_receptores(&self) -> Vec<String> {
        (1..=self.receptores.quantidade)
            .map(|i| format!("recv-{i}"))
            .collect()
    }
}

#[cfg(test)]
mod testes {
    use super::*;

    #[test]
    fn duracao_aceita_sufixos() {
        assert_eq!(analisar_duracao("60s").unwrap(), Duration::from_secs(60));
        assert_eq!(analisar_duracao("5m").unwrap(), Duration::from_secs(300));
        assert_eq!(analisar_duracao("1h").unwrap(), Duration::from_secs(3600));
        assert_eq!(
            analisar_duracao("500ms").unwrap(),
            Duration::from_millis(500)
        );
        assert_eq!(analisar_duracao("30").unwrap(), Duration::from_secs(30));
    }

    #[test]
    fn duracao_rejeita_lixo() {
        assert!(analisar_duracao("abc").is_err());
        assert!(analisar_duracao("-5s").is_err());
        assert!(analisar_duracao("").is_err());
    }

    fn cfg_exemplo() -> Config {
        toml::from_str(
            r#"
            [teste]
            duracao = "60s"
            taxa = 50000
            [receptores]
            quantidade = 4
            [[gerador]]
            nome = "A"
            proto = "udp"
            peso = 90
            [[gerador]]
            nome = "B"
            proto = "tcp"
            peso = 3
            [[gerador]]
            nome = "C"
            proto = "udp"
            peso = 1
            [[gerador]]
            nome = "D"
            proto = "udp"
            peso = 6
        "#,
        )
        .unwrap()
    }

    #[test]
    fn pesos_viram_taxas_proporcionais() {
        let plano = cfg_exemplo().plano();
        assert_eq!(plano[0].taxa, 45_000);
        assert_eq!(plano[1].taxa, 1_500);
        assert_eq!(plano[2].taxa, 500);
        assert_eq!(plano[3].taxa, 3_000);
    }

    #[test]
    fn taxa_explicita_ignora_o_peso() {
        let cfg: Config = toml::from_str(
            r#"
            [teste]
            duracao = "10s"
            taxa = 1000
            [receptores]
            quantidade = 1
            [[gerador]]
            nome = "X"
            proto = "tcp"
            peso = 1
            taxa = 777
        "#,
        )
        .unwrap();
        let plano = cfg.plano();
        assert_eq!(plano[0].taxa, 777);
        assert!(plano[0].taxa_explicita);
    }

    #[test]
    fn rejeita_nomes_duplicados() {
        let cfg: Result<Config, _> = toml::from_str::<Config>(
            r#"
            [teste]
            duracao = "10s"
            taxa = 10
            [receptores]
            quantidade = 1
            [[gerador]]
            nome = "A"
            proto = "tcp"
            [[gerador]]
            nome = "A"
            proto = "udp"
        "#,
        );
        assert!(cfg.unwrap().validar().is_err());
    }
}
