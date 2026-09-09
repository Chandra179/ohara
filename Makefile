SHELL := /bin/sh

CARGO ?= cargo
TOOLCHAIN ?= 1.98.1
CARGO_CMD := $(CARGO) +$(TOOLCHAIN)

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
		'make test-eval        Run the hermetic retrieval evaluator' \
		'make test-real-eval   Run the ignored model-backed evaluator' \
		'make doc              Build documentation without dependencies' \
		'make verify           Run all repository quality gates' \
		'make build            Build the default feature set' \
		'make build-minimal    Build without default native features' \
		'make run ARGS=...     Run ohara with optional arguments' \
		'make clean            Remove Cargo build artifacts'

toolchain:
	rustup toolchain install $(TOOLCHAIN) --profile minimal --component rustfmt --component clippy

fmt:
	$(CARGO_CMD) fmt --all

fmt-check:
	$(CARGO_CMD) fmt --all -- --check

check:
	$(CARGO_CMD) check --workspace --all-targets

check-minimal:
	$(CARGO_CMD) check --workspace --lib --bins --no-default-features

clippy:
	$(CARGO_CMD) clippy --workspace --all-targets -- -D warnings

test:
	$(CARGO_CMD) test --workspace

test-lib:
	$(CARGO_CMD) test --lib

test-integration:
	$(CARGO_CMD) test --test integration

test-eval:
	$(CARGO_CMD) test --test integration eval_retrieval_baseline_machinery -- --nocapture

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
