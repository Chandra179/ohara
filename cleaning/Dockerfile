FROM rust:1.95-bookworm AS build
WORKDIR /src
COPY . .
RUN cargo build --release -p ohara-cleaning

FROM debian:bookworm-slim
COPY --from=build /src/target/release/ohara-cleaning /usr/local/bin/ohara-cleaning
USER 65532:65532
ENTRYPOINT ["/usr/local/bin/ohara-cleaning"]
