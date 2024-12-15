FROM rust:1.81-slim
WORKDIR /app
RUN apt-get update && \
    apt-get install -y dnsutils && \
    rm -rf /var/lib/apt/lists/*
COPY . .
RUN cargo build --release
CMD ["./target/release/p2p-service"]
