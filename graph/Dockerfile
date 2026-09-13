FROM rust:1.95-bookworm AS build
WORKDIR /src
COPY . .
RUN cargo build --release -p ohara-graph

FROM debian:bookworm-slim
COPY --from=build /src/target/release/ohara-graph /usr/local/bin/ohara-graph
USER 65532:65532
ENTRYPOINT ["/usr/local/bin/ohara-graph"]
