#!/usr/bin/env python3
"""Measure deterministic retrieval cold and warm latency.

The benchmark uses the real retrieval binary with local standard-library test
doubles. It measures the time from process launch through the first successful
query (cold) and repeated queries after startup (warm). No external service,
model download, or runtime data directory is used.
"""

from __future__ import annotations

import importlib.util
import math
import os
from pathlib import Path
import signal
import subprocess
import sys
import tempfile
import time
from typing import Any


FIXTURE_PATH = Path(__file__).with_name("pipeline_fixture.py")


def load_fixture_module() -> Any:
    specification = importlib.util.spec_from_file_location(
        "ohara_pipeline_fixture", FIXTURE_PATH
    )
    if specification is None or specification.loader is None:
        raise RuntimeError(f"could not load {FIXTURE_PATH}")
    module = importlib.util.module_from_spec(specification)
    specification.loader.exec_module(module)
    return module


def positive_int(name: str, default: int) -> int:
    value = int(os.environ.get(name, default))
    if value < 1:
        raise ValueError(f"{name} must be at least 1")
    return value


def positive_float(name: str, default: float) -> float:
    value = float(os.environ.get(name, default))
    if value <= 0:
        raise ValueError(f"{name} must be greater than zero")
    return value


def percentile(samples: list[float], percentage: float) -> float:
    """Return the nearest-rank percentile in milliseconds."""
    if not samples:
        raise ValueError("cannot calculate a percentile without samples")
    ordered = sorted(samples)
    rank = max(1, math.ceil((percentage / 100) * len(ordered)))
    return ordered[rank - 1]


def stop_process(process: Any) -> None:
    if process.process.poll() is None:
        process.process.send_signal(signal.SIGTERM)
    try:
        process.process.wait(timeout=5)
    except subprocess.TimeoutExpired:
        process.process.kill()
        process.process.wait(timeout=5)
    process.close()


def query_once(fixture: Any, url: str) -> float:
    started = time.perf_counter()
    status, payload = fixture.request_json(
        "POST", f"{url}/api/query", {"query": "What is Ohara?", "top_k": 5}
    )
    elapsed = (time.perf_counter() - started) * 1_000
    if status != 200:
        raise RuntimeError(f"query returned HTTP {status}: {payload}")
    if not isinstance(payload, dict) or payload.get("grounding") != "grounded":
        raise RuntimeError(f"query was not grounded: {payload}")
    if not isinstance(payload.get("answer"), str) or not payload["answer"].strip():
        raise RuntimeError(f"query returned an empty answer: {payload}")
    return elapsed


def run_benchmark() -> None:
    fixture = load_fixture_module()
    cold_runs = positive_int("OHARA_BENCHMARK_COLD_RUNS", 5)
    warm_runs = positive_int("OHARA_BENCHMARK_WARM_RUNS", 20)
    cold_p50_limit = positive_float("OHARA_BENCHMARK_COLD_P50_MS", 1_000)
    cold_p95_limit = positive_float("OHARA_BENCHMARK_COLD_P95_MS", 3_000)
    warm_p50_limit = positive_float("OHARA_BENCHMARK_WARM_P50_MS", 250)
    warm_p95_limit = positive_float("OHARA_BENCHMARK_WARM_P95_MS", 500)

    with tempfile.TemporaryDirectory(prefix="ohara-benchmark-") as temporary:
        data_dir = Path(temporary) / "data"
        log_dir = Path(temporary) / "logs"
        data_dir.mkdir()
        log_dir.mkdir()
        qdrant_server = fixture.start_provider(fixture.QdrantHandler)
        ollama_server = fixture.start_provider(fixture.OllamaHandler)
        qdrant_server.points.extend(
            {
                "id": f"fixture-point-{index}",
                "vector": [0.0] * 384,
                "payload": {
                    "chunkId": f"fixture-chunk-{index}",
                    "text": (
                        "Ohara is a private local knowledge base that turns web "
                        "topics into searchable evidence."
                    ),
                },
            }
            for index in range(32)
        )
        cold_totals: list[float] = []
        cold_starts: list[float] = []
        cold_queries: list[float] = []
        warm_queries: list[float] = []
        base_environment = os.environ.copy()
        base_environment.update(
            {
                "OHARA_DATA_DIR": str(data_dir),
                "OHARA_QDRANT_URL": (
                    f"http://127.0.0.1:{qdrant_server.server_port}"
                ),
                "OHARA_FALKORDB_URL": "redis://127.0.0.1:1",
                "OHARA_LLM_URL": f"http://127.0.0.1:{ollama_server.server_port}",
                "OHARA_LLM_MODEL": "fixture",
                "OHARA_EMBEDDING_MODE": "deterministic",
            }
        )
        processes: list[Any] = []
        try:
            for run in range(cold_runs):
                retrieval_port = fixture.free_port()
                environment = base_environment | {
                    "OHARA_RETRIEVAL_BIND": f"127.0.0.1:{retrieval_port}"
                }
                started = time.perf_counter()
                process = fixture.ManagedProcess(
                    f"retrieval-cold-{run}",
                    "ohara-retrieval",
                    environment,
                    log_dir,
                )
                processes.append(process)
                retrieval_url = f"http://127.0.0.1:{retrieval_port}"
                fixture.wait_for_http(f"{retrieval_url}/api/health")
                startup_ms = (time.perf_counter() - started) * 1_000
                query_ms = query_once(fixture, retrieval_url)
                cold_starts.append(startup_ms)
                cold_queries.append(query_ms)
                cold_totals.append(startup_ms + query_ms)

                if run == 0:
                    warm_queries.extend(
                        query_once(fixture, retrieval_url) for _ in range(warm_runs)
                    )
                stop_process(process)

            cold_p50 = percentile(cold_totals, 50)
            cold_p95 = percentile(cold_totals, 95)
            warm_p50 = percentile(warm_queries, 50)
            warm_p95 = percentile(warm_queries, 95)
            print(
                "latency benchmark: "
                f"cold total p50={cold_p50:.1f}ms p95={cold_p95:.1f}ms; "
                f"warm query p50={warm_p50:.1f}ms p95={warm_p95:.1f}ms"
            )
            print(
                "latency detail: "
                f"cold startup p50={percentile(cold_starts, 50):.1f}ms "
                f"p95={percentile(cold_starts, 95):.1f}ms; "
                f"cold query p50={percentile(cold_queries, 50):.1f}ms "
                f"p95={percentile(cold_queries, 95):.1f}ms"
            )
            checks = [
                (cold_p50, cold_p50_limit, "cold p50"),
                (cold_p95, cold_p95_limit, "cold p95"),
                (warm_p50, warm_p50_limit, "warm p50"),
                (warm_p95, warm_p95_limit, "warm p95"),
            ]
            failures = [
                f"{name} {actual:.1f}ms exceeds {limit:.1f}ms"
                for actual, limit, name in checks
                if actual > limit
            ]
            if failures:
                raise RuntimeError("; ".join(failures))
            print("latency benchmark passed: all p50/p95 thresholds satisfied")
        finally:
            for process in reversed(processes):
                if process.process.poll() is None:
                    stop_process(process)
            qdrant_server.shutdown()
            ollama_server.shutdown()
            failed = [
                process
                for process in processes
                if process.process.returncode not in (0, -signal.SIGTERM)
            ]
            if failed:
                for process in failed:
                    output = process.log_path.read_text(encoding="utf-8")
                    if output:
                        print(
                            f"--- {process.name} log ---\n{output}",
                            file=sys.stderr,
                        )


def main() -> int:
    try:
        run_benchmark()
    except (OSError, RuntimeError, ValueError) as error:
        print(f"latency benchmark failed: {error}", file=sys.stderr)
        return 1
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
