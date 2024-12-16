FROM rust:1.81-slim
WORKDIR /app
ENV CARGO_INCREMENTAL=1
ENV RUSTC_FORCE_INCREMENTAL=1
ENV CARGO_BUILD_JOBS=8
RUN apt-get update && \
    apt-get install -y dnsutils && \
    rm -rf /var/lib/apt/lists/*
RUN mkdir -p target
COPY Cargo.toml Cargo.lock ./
RUN mkdir src && echo "fn main() {}" > src/main.rs
RUN cargo build
COPY src ./src
RUN cargo build
CMD ["./target/debug/p2p-service"]
