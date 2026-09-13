FROM rust:1.95-trixie AS build
WORKDIR /src
COPY . .
RUN cargo build --release -p ohara-indexer

FROM debian:trixie-slim
RUN apt-get update \
    && apt-get install --yes --no-install-recommends ca-certificates libgomp1 \
    && rm -rf /var/lib/apt/lists/*
COPY --from=build /src/target/release/ohara-indexer /usr/local/bin/ohara-indexer
USER 65532:65532
ENTRYPOINT ["/usr/local/bin/ohara-indexer"]
