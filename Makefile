SHELL := /bin/sh

RUSTUP ?= rustup
TOOLCHAIN ?= 1.95.0
DATA_DIR ?= $(CURDIR)/data
RETRIEVAL_BIND ?= 127.0.0.1:3000
RETRIEVAL_URL ?= http://$(RETRIEVAL_BIND)
FRONTEND_HOST ?= 127.0.0.1
FRONTEND_PORT ?= 5173
QDRANT_URL ?= http://127.0.0.1:6335
QDRANT_COLLECTION ?= ohara_chunks
QDRANT_SEARCH_MODE ?= exact
QDRANT_HNSW_EF ?= 64
FALKORDB_URL ?= redis://127.0.0.1:6380
FALKORDB_GRAPH ?= ohara
LLM_URL ?= http://127.0.0.1:11434
LLM_MODEL ?= phi4-mini:latest
OHARA_PROCESS_AUTH_TOKEN ?=
CONTAINER_USER ?= $(shell id -u):$(shell id -g)
STAGE ?= cleaning
LIMIT ?= 10

CARGO = OHARA_DATA_DIR="$(DATA_DIR)" OHARA_QDRANT_URL="$(QDRANT_URL)" OHARA_QDRANT_COLLECTION="$(QDRANT_COLLECTION)" OHARA_QDRANT_SEARCH_MODE="$(QDRANT_SEARCH_MODE)" OHARA_QDRANT_HNSW_EF="$(QDRANT_HNSW_EF)" OHARA_FALKORDB_URL="$(FALKORDB_URL)" OHARA_FALKORDB_GRAPH="$(FALKORDB_GRAPH)" OHARA_LLM_URL="$(LLM_URL)" OHARA_LLM_MODEL="$(LLM_MODEL)" OHARA_PROCESS_AUTH_TOKEN="$(OHARA_PROCESS_AUTH_TOKEN)" OHARA_RETRIEVAL_BIND="$(RETRIEVAL_BIND)" $(RUSTUP) run $(TOOLCHAIN) cargo

.DEFAULT_GOAL := help
.PHONY: help toolchain fmt fmt-check clippy test doc verify build \
        providers providers-down providers-logs docker-up docker-down docker-logs \
        scraper cleaning indexer graph retrieval frontend replay rebuild \
        pipeline-fixture pipeline-benchmark pipeline-resource-benchmark retrieval-quality \
        entity-resolution-quality qdrant-hnsw-benchmark dev clean

help:
	@printf '%s\n' \
		'Ohara commands:' \
		'' \
		'  make providers      Start Qdrant and FalkorDB' \
		'  make scraper        Start topic discovery and raw ingestion' \
		'  make cleaning       Start the cleaning process' \
		'  make indexer        Start chunking and Qdrant indexing' \
		'  make graph          Start FalkorDB graph extraction' \
		'  make retrieval      Start the frontend-facing HTTP interface' \
		'  make frontend       Start the local Vite frontend' \
		'  edit scraper/config.yaml                          Configure discovery and fetching' \
		'  OHARA_SCRAPER_BRAVE_API_KEY=... make scraper      Supply the Brave secret' \
		'  make replay         Retry dead-letter artifacts (STAGE=cleaning LIMIT=10)' \
		'  make rebuild        Recreate derived stores and replay durable artifacts' \
		'  make pipeline-fixture Run the deterministic scrape-to-query harness' \
		'  make pipeline-benchmark Measure cold/warm latency with p50/p95 gates' \
		'  make pipeline-resource-benchmark Measure peak RSS for all processes' \
		'  make retrieval-quality Evaluate golden retrieval metrics' \
		'  make entity-resolution-quality Measure entity merge thresholds' \
		'  make qdrant-hnsw-benchmark Compare exact and HNSW Qdrant search' \
		'  make dev            Start providers and all local processes' \
		'' \
		'  make docker-up      Build and run Rust processes in Compose' \
		'  make docker-down    Stop all Compose containers' \
		'  make docker-logs    Follow Compose logs' \
		'' \
		'  make verify         Run Rust formatting, lint, test, and docs' \
		'  make toolchain      Install the pinned Rust toolchain' \
		'  make clean          Remove Cargo build artifacts'

toolchain:
	$(RUSTUP) toolchain install $(TOOLCHAIN) --profile minimal --component rustfmt --component clippy

fmt:
	$(CARGO) fmt --all

fmt-check:
	$(CARGO) fmt --all -- --check

clippy:
	$(CARGO) clippy --workspace --all-targets -- -D warnings

test:
	$(CARGO) test --workspace

doc:
	$(CARGO) doc --workspace --no-deps

verify: fmt-check clippy test doc entity-resolution-quality

build:
	$(CARGO) build --workspace

providers:
	docker compose up -d --wait qdrant falkordb

providers-down:
	docker compose stop qdrant falkordb

providers-logs:
	docker compose logs -f --tail=100 qdrant falkordb

docker-up:
	OHARA_CONTAINER_USER="$(CONTAINER_USER)" docker compose up -d --build

docker-down:
	docker compose down

docker-logs:
	docker compose logs -f --tail=100

scraper:
	@$(CARGO) run -p ohara-scraper

cleaning:
	@$(CARGO) run -p ohara-cleaning

indexer:
	@$(CARGO) run -p ohara-indexer

graph:
	@$(CARGO) run -p ohara-graph

retrieval:
	@$(CARGO) run -p ohara-retrieval

frontend:
	@cd frontend && OHARA_API_PROXY_TARGET="$(RETRIEVAL_URL)" VITE_OHARA_API_MODE=http npm run dev -- --host $(FRONTEND_HOST) --port $(FRONTEND_PORT) --strictPort

replay:
	@set -eu; \
		case "$(STAGE)" in \
			cleaning|indexer|graph) ;; \
			*) echo 'STAGE must be cleaning, indexer, or graph' >&2; exit 2 ;; \
		esac; \
		case "$(LIMIT)" in \
			''|*[!0-9]*) echo 'LIMIT must be a non-negative integer' >&2; exit 2 ;; \
		esac; \
		inbox="$(DATA_DIR)/inbox/$(STAGE)"; \
		dead_letter="$(DATA_DIR)/dead-letter/$(STAGE)"; \
		mkdir -p "$$inbox" "$$dead_letter"; \
		count=0; \
		for path in "$$dead_letter"/*.json; do \
			[ -f "$$path" ] || continue; \
			[ "$$count" -lt "$(LIMIT)" ] || break; \
			mv "$$path" "$$inbox/$$(basename "$$path")"; \
			count=$$((count + 1)); \
		done; \
		echo "Replayed $$count $(STAGE) dead-letter artifact(s)"

rebuild:
	@set -eu; \
		command -v curl >/dev/null 2>&1 || { echo 'rebuild requires curl' >&2; exit 127; }; \
		qdrant_url="$(QDRANT_URL)"; \
		qdrant_url="$${qdrant_url%/}"; \
		mkdir -p "$(DATA_DIR)/inbox/indexer" "$(DATA_DIR)/inbox/graph"; \
		for path in "$(DATA_DIR)"/clean/*.json; do \
			[ -f "$$path" ] || continue; \
			name="$$(basename "$$path")"; \
			[ ! -e "$(DATA_DIR)/inbox/indexer/$$name" ] || { echo "rebuild refused: pending indexer inbox artifact exists: $$name" >&2; exit 2; }; \
		done; \
		for path in "$(DATA_DIR)"/indexed/*.json; do \
			[ -f "$$path" ] || continue; \
			name="$$(basename "$$path")"; \
			[ ! -e "$(DATA_DIR)/inbox/graph/$$name" ] || { echo "rebuild refused: pending graph inbox artifact exists: $$name" >&2; exit 2; }; \
		done; \
		printf '%s\n' 'Rebuilding Qdrant collection $(QDRANT_COLLECTION)...'; \
		delete_status="$$(curl --silent --show-error --output /dev/null --write-out '%{http_code}' --request DELETE "$$qdrant_url/collections/$(QDRANT_COLLECTION)")"; \
		case "$$delete_status" in 200|404) ;; *) echo "Qdrant collection delete returned HTTP $$delete_status" >&2; exit 1 ;; esac; \
		curl --fail --silent --show-error --request PUT "$$qdrant_url/collections/$(QDRANT_COLLECTION)" \
			--header 'content-type: application/json' \
			--data '{"vectors":{"size":384,"distance":"Cosine"},"hnsw_config":{"m":16,"ef_construct":100,"full_scan_threshold":$(if $(filter hnsw,$(QDRANT_SEARCH_MODE)),10,1000000000)}}' >/dev/null; \
		printf '%s\n' 'Rebuilding FalkorDB graph $(FALKORDB_GRAPH)...'; \
		if [ "$(FALKORDB_URL)" = 'redis://127.0.0.1:6380' ]; then \
			graph_delete="$$(docker compose exec -T falkordb redis-cli --raw GRAPH.DELETE "$(FALKORDB_GRAPH)" 2>&1)" || true; \
		else \
			command -v redis-cli >/dev/null 2>&1 || { echo 'rebuild requires redis-cli for a non-default FalkorDB URL' >&2; exit 127; }; \
			graph_delete="$$(redis-cli --raw -u "$(FALKORDB_URL)" GRAPH.DELETE "$(FALKORDB_GRAPH)" 2>&1)" || true; \
		fi; \
		case "$$graph_delete" in \
			*'does not exist'*|*'not found'*|'') ;; \
			*) echo "FalkorDB graph delete failed: $$graph_delete" >&2; exit 1 ;; \
		esac; \
		clean_count=0; \
		for path in "$(DATA_DIR)"/clean/*.json; do \
			[ -f "$$path" ] || continue; \
			name="$$(basename "$$path")"; \
			temporary="$(DATA_DIR)/inbox/indexer/.rebuild-$$name-$$$$"; \
			cp "$$path" "$$temporary"; \
			mv "$$temporary" "$(DATA_DIR)/inbox/indexer/$$name"; \
			clean_count=$$((clean_count + 1)); \
		done; \
		indexed_count=0; \
		for path in "$(DATA_DIR)"/indexed/*.json; do \
			[ -f "$$path" ] || continue; \
			name="$$(basename "$$path")"; \
			temporary="$(DATA_DIR)/inbox/graph/.rebuild-$$name-$$$$"; \
			cp "$$path" "$$temporary"; \
			mv "$$temporary" "$(DATA_DIR)/inbox/graph/$$name"; \
			indexed_count=$$((indexed_count + 1)); \
		done; \
		echo "Queued $$clean_count clean artifact(s) for Qdrant and $$indexed_count indexed artifact(s) for FalkorDB"; \
		echo 'Restart the five processes after this command if they were stopped.'

pipeline-fixture:
	$(CARGO) build --workspace
	OHARA_PIPELINE_QDRANT_SEARCH_MODE="$(QDRANT_SEARCH_MODE)" $(CARGO) run -p ohara-tools --bin pipeline_fixture

pipeline-benchmark:
	$(CARGO) build --workspace
	$(CARGO) run -p ohara-tools --bin pipeline_benchmark

pipeline-resource-benchmark:
	$(CARGO) build --workspace
	$(CARGO) run -p ohara-tools --bin pipeline_resource_benchmark

retrieval-quality:
	$(CARGO) build --workspace
	$(CARGO) run -p ohara-tools --bin retrieval_quality_benchmark

entity-resolution-quality:
	$(CARGO) run -p ohara-graph -- --entity-resolution-benchmark

qdrant-hnsw-benchmark: providers
	$(CARGO) run -p ohara-tools --bin qdrant_hnsw_benchmark

dev:
	@set -eu; \
		pids=; \
		cleanup() { \
			status=$$?; \
			trap - INT TERM EXIT; \
			for pid in $$pids; do kill -TERM "$$pid" 2>/dev/null || true; done; \
			for pid in $$pids; do wait "$$pid" 2>/dev/null || true; done; \
			$(MAKE) --no-print-directory providers-down >/dev/null 2>&1 || true; \
			exit "$$status"; \
		}; \
		trap cleanup INT TERM EXIT; \
		$(MAKE) --no-print-directory providers; \
		$(CARGO) run -p ohara-scraper & pids="$$pids $$!"; \
		$(CARGO) run -p ohara-cleaning & pids="$$pids $$!"; \
		$(CARGO) run -p ohara-indexer & pids="$$pids $$!"; \
		$(CARGO) run -p ohara-graph & pids="$$pids $$!"; \
		$(CARGO) run -p ohara-retrieval & pids="$$pids $$!"; \
		sleep 2; \
		cd frontend && OHARA_API_PROXY_TARGET="$(RETRIEVAL_URL)" VITE_OHARA_API_MODE=http npm run dev -- --host $(FRONTEND_HOST) --port $(FRONTEND_PORT) --strictPort

clean:
	$(CARGO) clean
