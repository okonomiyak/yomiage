# マルチステージ + cargo-chef（PLAN §10.3）。
# cargo-chef で依存だけを先にビルドしておくと、src を触っただけの再ビルドが数十秒で済む。
# LXC 上でのフルビルドは 3 vCPU で数分かかるので効果が大きい。

# cargo-chef 入りの公式イメージ。rust のバージョンはタグで固定する。
FROM lukemathwalker/cargo-chef:latest-rust-1.92-bookworm AS chef
# songbird の Opus は opus2 -> libopus_sys で、ビルドスクリプトが cmake を呼ぶ。
# 入れないと "failed to execute command: No such file or directory" で落ちる。
RUN apt-get update \
    && apt-get install -y --no-install-recommends cmake pkg-config \
    && rm -rf /var/lib/apt/lists/*
WORKDIR /app

FROM chef AS planner
COPY Cargo.toml Cargo.lock ./
COPY src ./src
RUN cargo chef prepare --recipe-path recipe.json

FROM chef AS builder
COPY --from=planner /app/recipe.json recipe.json
# ここが依存のビルド。recipe.json が変わらない限りキャッシュが効く。
RUN cargo chef cook --release --recipe-path recipe.json
COPY Cargo.toml Cargo.lock ./
COPY src ./src
# sqlx::migrate! はビルド時にこのディレクトリを読む。忘れるとコンパイルが落ちる。
COPY migrations ./migrations
RUN cargo build --release --locked

FROM debian:bookworm-slim AS runtime
# TLS は rustls（webpki-roots 同梱）なので openssl は不要。
# ca-certificates だけ入れておく。songbird の Opus は opus2（Rust 実装）なので libopus も不要。
RUN apt-get update \
    && apt-get install -y --no-install-recommends ca-certificates \
    && rm -rf /var/lib/apt/lists/*
COPY --from=builder /app/target/release/yomiage-bot /usr/local/bin/yomiage-bot
ENTRYPOINT ["/usr/local/bin/yomiage-bot"]
