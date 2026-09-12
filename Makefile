SHELL := /bin/sh

RUSTUP ?= rustup
TOOLCHAIN ?= 1.95.0
API_BIND ?= 127.0.0.1:3000
API_PORT := $(lastword $(subst :, ,$(API_BIND)))
FRONTEND_HOST ?= 127.0.0.1
FRONTEND_PORT ?= 5173
API_PROXY_TARGET ?= http://$(API_BIND)
BACKEND_READY_ATTEMPTS ?= 120
BACKEND_READY_INTERVAL ?= 0.25
CONFIG_ARGS ?=
BACKEND_ARGS ?= serve --bind $(API_BIND) $(CONFIG_ARGS)
WORKER_ARGS ?= $(CONFIG_ARGS)
RUSTFLAGS ?=
RUSTDOCFLAGS ?=

CARGO_CMD = RUSTFLAGS="$(RUSTFLAGS)" RUSTDOCFLAGS="$(RUSTDOCFLAGS)" $(RUSTUP) run $(TOOLCHAIN) cargo
CARGO_FMT_CMD = $(CARGO_CMD) fmt
CARGO_CLIPPY_CMD = $(CARGO_CMD) clippy
OHARA_BIN ?= $(CURDIR)/target/debug/ohara
COMPOSE_SERVICES := qdrant falkordb

.DEFAULT_GOAL := help

.PHONY: help toolchain fmt fmt-check check check-minimal clippy test test-lib test-integration test-eval test-real-eval doc verify build build-minimal run worker kill-port kill-api-port kill-frontend-port wait-api backend frontend services services-down services-logs dev clean

help:
	@printf '%s\n' \
		'Ohara commands:' \
		'' \
		'  make dev              Start backend, worker, and frontend (recommended)' \
		'  make backend          Start only the Rust API' \
		'  make worker           Start only the ingestion worker' \
		'  make frontend         Start only the Vite frontend' \
		'  make services         Start Qdrant and FalkorDB with Docker Compose' \
		'  make services-down    Stop the Compose knowledge services' \
		'  make services-logs    Follow the Compose knowledge-service logs' \
		'' \
		'  make run ARGS=...     Run one CLI/operator command' \
		'' \
		'  make verify           Run all Rust quality gates' \
		'  make build            Build the default Rust feature set' \
		'  make toolchain        Install the pinned Rust toolchain' \
		'  make clean            Remove Cargo build artifacts' \
		'' \
		'make dev starts Docker services and all Ohara processes in one terminal.' \
		'Run make services once before the separate backend/worker/frontend targets.' \
		'Use make dev CONFIG_ARGS="--config /path/to/ohara.toml" for a custom config.'

toolchain:
	$(RUSTUP) toolchain install $(TOOLCHAIN) --profile minimal --component rustfmt --component clippy

fmt:
	$(CARGO_FMT_CMD) --all

fmt-check:
	$(CARGO_FMT_CMD) --all -- --check

check:
	$(CARGO_CMD) check --workspace --all-targets

check-minimal:
	$(CARGO_CMD) check --workspace --lib --bins --no-default-features

clippy:
	$(CARGO_CLIPPY_CMD) --workspace --all-targets -- -D warnings

test:
	$(CARGO_CMD) test --workspace

test-lib:
	$(CARGO_CMD) test --lib

test-integration:
	$(CARGO_CMD) test --test integration

test-eval:
	$(CARGO_CMD) test --test integration 'eval::' -- --nocapture

test-real-eval:
	$(CARGO_CMD) test --test integration eval_retrieval_baseline_real_models -- --ignored --nocapture

doc:
	$(CARGO_CMD) doc --workspace --no-deps

verify: fmt-check clippy test doc

build:
	$(CARGO_CMD) build

build-minimal:
	$(CARGO_CMD) build --no-default-features

services:
	docker compose up -d --wait $(COMPOSE_SERVICES)

services-down:
	docker compose down

services-logs:
	docker compose logs -f --tail=100 qdrant falkordb

ifeq ($(strip $(ARGS)),)
run:
	@echo "make: ARGS is required; use 'make worker' to run the ingestion worker" >&2
	@exit 2
else
run:
	$(CARGO_CMD) run -- $(ARGS)
endif

# Service targets build once, then exec the real process so signals reach it.
worker:
	@$(CARGO_CMD) build; \
	exec "$(OHARA_BIN)" $(WORKER_ARGS)

kill-port:
	@set -eu; \
	if [ -z "$(PORT)" ]; then echo "make: PORT is required" >&2; exit 2; fi; \
	command -v fuser >/dev/null 2>&1 || { echo "make: fuser is required to free TCP ports" >&2; exit 1; }; \
	if fuser -s "$(PORT)/tcp" 2>/dev/null; then \
		echo "make: stopping process on TCP port $(PORT)" >&2; \
		fuser -k -TERM "$(PORT)/tcp" >/dev/null 2>&1 || true; \
		attempt=0; \
		while fuser -s "$(PORT)/tcp" 2>/dev/null; do \
			attempt=$$((attempt + 1)); \
			if [ "$$attempt" -ge 20 ]; then fuser -k -KILL "$(PORT)/tcp" >/dev/null 2>&1 || true; break; fi; \
			sleep 0.1; \
		done; \
	fi

kill-api-port:
	@$(MAKE) --no-print-directory kill-port PORT=$(API_PORT)

kill-frontend-port:
	@$(MAKE) --no-print-directory kill-port PORT=$(FRONTEND_PORT)

wait-api:
	@set -eu; \
	command -v curl >/dev/null 2>&1 || { echo "make: curl is required to wait for the API" >&2; exit 1; }; \
	attempt=0; \
	until curl --fail --silent --show-error --max-time 1 "$(API_PROXY_TARGET)/api/health" >/dev/null 2>&1; do \
		attempt=$$((attempt + 1)); \
		if [ "$$attempt" -ge "$(BACKEND_READY_ATTEMPTS)" ]; then \
			echo "make: Rust API did not become ready at $(API_PROXY_TARGET)" >&2; \
			exit 1; \
		fi; \
		sleep "$(BACKEND_READY_INTERVAL)"; \
	done

backend: kill-api-port
	@$(CARGO_CMD) build; \
	exec "$(OHARA_BIN)" $(BACKEND_ARGS)

frontend: kill-frontend-port wait-api
	@cd frontend && OHARA_API_PROXY_TARGET="$(API_PROXY_TARGET)" VITE_OHARA_API_MODE=http npm run dev -- --host $(FRONTEND_HOST) --port $(FRONTEND_PORT) --strictPort

dev:
	@set -eu; \
	backend_pid=; \
	worker_pid=; \
	cleanup() { \
		status=$$?; \
		trap - INT TERM EXIT; \
		if [ -n "$$backend_pid" ]; then kill -INT "$$backend_pid" 2>/dev/null || true; fi; \
		if [ -n "$$worker_pid" ]; then kill -INT "$$worker_pid" 2>/dev/null || true; fi; \
		if [ -n "$$backend_pid" ]; then wait "$$backend_pid" 2>/dev/null || true; fi; \
		if [ -n "$$worker_pid" ]; then wait "$$worker_pid" 2>/dev/null || true; fi; \
		$(MAKE) --no-print-directory kill-api-port || true; \
		$(MAKE) --no-print-directory kill-frontend-port || true; \
		$(MAKE) --no-print-directory services-down || true; \
		exit "$$status"; \
	}; \
	trap cleanup INT TERM EXIT; \
	$(MAKE) --no-print-directory services; \
	$(MAKE) --no-print-directory kill-api-port; \
	$(MAKE) --no-print-directory kill-frontend-port; \
	$(CARGO_CMD) build; \
	printf '%s\n' 'ohara: starting backend'; \
	"$(OHARA_BIN)" $(BACKEND_ARGS) & \
	backend_pid=$$!; \
	printf '%s\n' 'ohara: starting worker'; \
	"$(OHARA_BIN)" $(WORKER_ARGS) & \
	worker_pid=$$!; \
	$(MAKE) --no-print-directory wait-api; \
	printf '%s\n' 'ohara: starting frontend'; \
	(cd frontend && OHARA_API_PROXY_TARGET="$(API_PROXY_TARGET)" VITE_OHARA_API_MODE=http npm run dev -- --host $(FRONTEND_HOST) --port $(FRONTEND_PORT) --strictPort)

clean:
	$(CARGO_CMD) clean
