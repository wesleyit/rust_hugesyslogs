# Imagem única do HugeSyslogs: serve de relay, de receptor e de gerador.
# O papel é escolhido pelo comando passado no `podman run`.

# ---- Etapa 1: compila o binário Rust estaticamente ----
FROM docker.io/library/rust:1-bookworm AS construtor

RUN rustup target add x86_64-unknown-linux-musl && \
    apt-get update && \
    apt-get install -y --no-install-recommends musl-tools && \
    rm -rf /var/lib/apt/lists/*

WORKDIR /src
COPY Cargo.toml Cargo.lock ./
COPY src ./src
RUN cargo build --release --target x86_64-unknown-linux-musl

# ---- Etapa 2: runtime Ubuntu 26.04 LTS com rsyslog e nginx ----
FROM docker.io/library/ubuntu:26.04

# nginx entra como balanceador L4 opcional; o modulo stream e dinamico.
RUN apt-get update && \
    DEBIAN_FRONTEND=noninteractive apt-get install -y --no-install-recommends \
    rsyslog rsyslog-gnutls nginx libnginx-mod-stream && \
    rm -rf /var/lib/apt/lists/* /etc/rsyslog.d/* /etc/rsyslog.conf \
    /etc/nginx/sites-enabled/* /etc/nginx/conf.d/*

# musl estático: nenhuma dependência de glibc entre as duas etapas.
COPY --from=construtor \
    /src/target/x86_64-unknown-linux-musl/release/hugesyslogs /usr/local/bin/hugesyslogs

RUN mkdir -p /etc/hsb /out
