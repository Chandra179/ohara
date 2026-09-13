FROM rust:1.95-bookworm AS build
WORKDIR /src
COPY . .
RUN cargo build --release -p ohara-scraper

FROM debian:bookworm-slim
RUN apt-get update \
    && apt-get install --yes --no-install-recommends ca-certificates \
    && rm -rf /var/lib/apt/lists/*
COPY --from=build /src/target/release/ohara-scraper /usr/local/bin/ohara-scraper
USER 65532:65532
ENTRYPOINT ["/usr/local/bin/ohara-scraper"]
