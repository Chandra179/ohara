SHELL := /bin/sh

RUSTUP ?= rustup
TOOLCHAIN ?= stable
TOOLCHAIN_BIN := $(dir $(shell $(RUSTUP) which cargo --toolchain $(TOOLCHAIN) 2>/dev/null))
HOST_PATH := $(PATH)
RUST_ENV := env PATH="$(TOOLCHAIN_BIN):$(HOST_PATH)"
CARGO_CMD := $(RUST_ENV) $(RUSTUP) run $(TOOLCHAIN) cargo
CARGO_FMT_CMD := $(RUST_ENV) $(RUSTUP) run $(TOOLCHAIN) cargo-fmt
CARGO_CLIPPY_CMD := $(RUST_ENV) $(RUSTUP) run $(TOOLCHAIN) cargo-clippy

.DEFAULT_GOAL := verify

.PHONY: help toolchain fmt fmt-check check check-minimal clippy test test-lib test-integration test-eval test-real-eval doc verify build build-minimal run clean

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

clean:
	$(CARGO_CMD) clean
