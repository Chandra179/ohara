SHELL := /bin/sh

RUSTUP ?= rustup
TOOLCHAIN ?= 1.95.0
TOOLCHAIN_BIN := $(dir $(shell $(RUSTUP) which cargo --toolchain $(TOOLCHAIN) 2>/dev/null))
HOST_PATH := $(PATH)
API_BIND ?= 127.0.0.1:3000
API_PORT := $(lastword $(subst :, ,$(API_BIND)))
FRONTEND_HOST ?= 127.0.0.1
FRONTEND_PORT ?= 5173
API_PROXY_TARGET ?= http://$(API_BIND)
BACKEND_READY_ATTEMPTS ?= 120
BACKEND_READY_INTERVAL ?= 0.25
BACKEND_ARGS ?= serve --bind $(API_BIND)
OPENSSL_DIR ?=
OPENSSL_FALLBACK_DIR ?= /tmp/ohara-ossl
OPENSSL_RUNTIME_LIB_DIR ?= /usr/lib/$(shell $(CC) -print-multiarch 2>/dev/null)
RUSTFLAGS ?=
RUSTDOCFLAGS ?=

OPENSSL_RUSTDOCFLAGS = $(if $(OPENSSL_DIR),-C link-arg=-L$(OPENSSL_DIR)/lib,)
OPENSSL_ENV = OPENSSL_DIR="$(OPENSSL_DIR)" OPENSSL_FALLBACK_DIR="$(OPENSSL_FALLBACK_DIR)" OPENSSL_RUNTIME_LIB_DIR="$(OPENSSL_RUNTIME_LIB_DIR)" RUSTFLAGS="$(RUSTFLAGS)" RUSTDOCFLAGS="$(RUSTDOCFLAGS) $(OPENSSL_RUSTDOCFLAGS)"
RUST_ENV = env PATH="$(TOOLCHAIN_BIN):$(HOST_PATH)" $(OPENSSL_ENV)
RAW_CARGO_CMD = $(RUSTUP) run $(TOOLCHAIN) cargo
CARGO_CMD = $(RUST_ENV) $(RAW_CARGO_CMD)
CARGO_FMT_CMD = $(RUST_ENV) $(RUSTUP) run $(TOOLCHAIN) cargo-fmt
CARGO_CLIPPY_CMD = $(RUST_ENV) $(RUSTUP) run $(TOOLCHAIN) cargo-clippy
BACKEND_CMD = $(RUST_ENV) sh scripts/run-with-openssl.sh $(RAW_CARGO_CMD) run -- $(BACKEND_ARGS)

.DEFAULT_GOAL := verify

.PHONY: help toolchain fmt fmt-check check check-minimal clippy test test-lib test-integration test-eval test-real-eval doc verify build build-minimal run kill-port kill-api-port kill-frontend-port wait-api backend frontend dev clean

help:
	@printf '%s\n' \
		'make toolchain         Install the pinned Rust toolchain' \
		'make fmt              Format Rust sources' \
		'make fmt-check        Check formatting without changing files' \
		'make check            Type-check all targets' \
		'make check-minimal    Type-check without default native features' \
		'make clippy           Run Clippy with warnings denied' \
		'make test             Run the complete test suite' \
		'make test-lib         Run library/unit tests' \
		'make test-integration Run integration tests' \
		'make test-eval        Run the hermetic retrieval evaluation suite' \
		'make test-real-eval   Run the ignored model-backed evaluator' \
		'make doc              Build documentation without dependencies' \
		'make verify           Run all repository quality gates' \
		'make build            Build the default feature set' \
		'make build-minimal    Build without default native features' \
		'make run ARGS=...     Run ohara with optional arguments' \
		'make kill-port PORT=... Stop a process on an exact TCP port' \
		'make backend          Free API port and run the Rust API' \
		'make frontend         Free frontend port and run Vite' \
		'make dev              Run the Rust API and frontend together' \
		'make clean            Remove Cargo build artifacts'

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

run:
	$(CARGO_CMD) run -- $(ARGS)

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
	@$(BACKEND_CMD)

frontend: kill-frontend-port wait-api
	@cd frontend && OHARA_API_PROXY_TARGET="$(API_PROXY_TARGET)" VITE_OHARA_API_MODE=http npm run dev -- --host $(FRONTEND_HOST) --port $(FRONTEND_PORT) --strictPort

dev:
	@set -eu; \
	$(MAKE) --no-print-directory kill-api-port; \
	$(MAKE) --no-print-directory kill-frontend-port; \
	$(BACKEND_CMD) & \
	backend_pid=$$!; \
	cleanup() { kill "$$backend_pid" 2>/dev/null || true; wait "$$backend_pid" 2>/dev/null || true; }; \
	trap cleanup INT TERM EXIT; \
	$(MAKE) --no-print-directory wait-api; \
	cd frontend; \
	OHARA_API_PROXY_TARGET="$(API_PROXY_TARGET)" VITE_OHARA_API_MODE=http npm run dev -- --host $(FRONTEND_HOST) --port $(FRONTEND_PORT) --strictPort

clean:
	$(CARGO_CMD) clean
