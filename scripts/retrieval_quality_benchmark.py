#!/usr/bin/env python3
"""Measure retrieval quality against a deterministic golden dataset.

The harness starts the real retrieval binary with standard-library Qdrant and
Ollama doubles. Qdrant ranks points by cosine similarity, and the points use
the same deterministic embedding formula as the Rust retrieval process. The
dataset uses exact query/document matches so this contract test is stable
without downloading a model. It validates ranking response handling and the
metric implementation; it is not a claim about production semantic quality.
"""

from __future__ import annotations

import hashlib
import importlib.util
import json
import math
import os
from http.server import BaseHTTPRequestHandler
from pathlib import Path
import signal
import subprocess
import sys
from tempfile import TemporaryDirectory
from typing import Any
from urllib.parse import urlsplit


ROOT = Path(__file__).resolve().parents[1]
DATASET_PATH = ROOT / "docs" / "architecture" / "fixtures" / "retrieval-golden-v1.json"
FIXTURE_PATH = Path(__file__).with_name("pipeline_fixture.py")
EMBEDDING_DIMENSION = 384
DEFAULT_TOP_K = 5


def load_fixture_module() -> Any:
    specification = importlib.util.spec_from_file_location(
        "ohara_pipeline_fixture", FIXTURE_PATH
    )
    if specification is None or specification.loader is None:
        raise RuntimeError(f"could not load {FIXTURE_PATH}")
    module = importlib.util.module_from_spec(specification)
    specification.loader.exec_module(module)
    return module


def read_dataset() -> dict[str, Any]:
    dataset = json.loads(DATASET_PATH.read_text(encoding="utf-8"))
    if not isinstance(dataset, dict) or dataset.get("schemaVersion") != 1:
        raise ValueError("retrieval golden dataset must use schema version 1")
    cutoffs = dataset.get("cutoffs")
    documents = dataset.get("documents")
    queries = dataset.get("queries")
    if not (
        isinstance(cutoffs, list)
        and cutoffs
        and all(isinstance(value, int) and value > 0 for value in cutoffs)
    ):
        raise ValueError("retrieval golden dataset must define positive cutoffs")
    if not isinstance(documents, list) or not documents:
        raise ValueError("retrieval golden dataset must define documents")
    if not isinstance(queries, list) or not queries:
        raise ValueError("retrieval golden dataset must define queries")

    document_ids: set[str] = set()
    for document in documents:
        if not isinstance(document, dict):
            raise ValueError("golden documents must be objects")
        chunk_id = document.get("chunkId")
        text = document.get("text")
        if (
            not isinstance(chunk_id, str)
            or not chunk_id
            or chunk_id in document_ids
            or not isinstance(text, str)
            or not text.strip()
        ):
            raise ValueError("golden documents need unique ids and non-empty text")
        document_ids.add(chunk_id)

    query_ids: set[str] = set()
    for query in queries:
        if not isinstance(query, dict):
            raise ValueError("golden queries must be objects")
        query_id = query.get("id")
        query_text = query.get("query")
        relevance = query.get("relevance")
        if (
            not isinstance(query_id, str)
            or not query_id
            or query_id in query_ids
            or not isinstance(query_text, str)
            or not query_text.strip()
            or not isinstance(relevance, dict)
            or not relevance
        ):
            raise ValueError("golden queries need unique ids, text, and relevance")
        query_ids.add(query_id)
        if any(
            chunk_id not in document_ids
            or not isinstance(grade, int)
            or not 0 <= grade <= 3
            for chunk_id, grade in relevance.items()
        ):
            raise ValueError("golden relevance must reference documents with grades 0..3")
        if not any(grade > 0 for grade in relevance.values()):
            raise ValueError("every golden query needs a relevant document")
    return dataset


def deterministic_embedding(text: str) -> list[float]:
    """Mirror the test-only deterministic embedding in the Rust processes."""
    output = []
    for index in range(EMBEDDING_DIMENSION):
        digest = hashlib.sha256()
        digest.update(text.encode())
        digest.update(index.to_bytes(8, "little"))
        value = int.from_bytes(digest.digest()[:2], "little")
        output.append((value / 65535) * 2 - 1)
    return output


def cosine(left: list[float], right: list[float]) -> float:
    left_norm = math.sqrt(sum(value * value for value in left))
    right_norm = math.sqrt(sum(value * value for value in right))
    if left_norm == 0 or right_norm == 0:
        return 0.0
    return sum(a * b for a, b in zip(left, right, strict=True)) / (left_norm * right_norm)


def recall_at_k(ranked_ids: list[str], relevance: dict[str, int], cutoff: int) -> float:
    relevant = {chunk_id for chunk_id, grade in relevance.items() if grade > 0}
    if not relevant:
        return 0.0
    retrieved = set(ranked_ids[:cutoff])
    return len(relevant & retrieved) / len(relevant)


def mean_reciprocal_rank(ranked_ids: list[str], relevance: dict[str, int]) -> float:
    for rank, chunk_id in enumerate(ranked_ids, start=1):
        if relevance.get(chunk_id, 0) > 0:
            return 1 / rank
    return 0.0


def ndcg_at_k(ranked_ids: list[str], relevance: dict[str, int], cutoff: int) -> float:
    def gain(grade: int) -> float:
        return (2**grade) - 1

    def discounted_gain(grades: list[int]) -> float:
        return sum(
            gain(grade) / math.log2(rank + 2)
            for rank, grade in enumerate(grades[:cutoff])
        )

    actual = discounted_gain([relevance.get(chunk_id, 0) for chunk_id in ranked_ids])
    ideal = discounted_gain(sorted(relevance.values(), reverse=True))
    return actual / ideal if ideal else 0.0


def metric_regression_tests() -> None:
    relevance = {"best": 3, "good": 2, "noise": 0}
    ranked = ["noise", "good", "best"]
    if not math.isclose(recall_at_k(ranked, relevance, 1), 0.0):
        raise RuntimeError("recall@1 regression test failed")
    if not math.isclose(recall_at_k(ranked, relevance, 3), 1.0):
        raise RuntimeError("recall@3 regression test failed")
    if not math.isclose(mean_reciprocal_rank(ranked, relevance), 0.5):
        raise RuntimeError("MRR regression test failed")
    expected_ndcg = ((2**2 - 1) / math.log2(3) + (2**3 - 1) / math.log2(4)) / (
        (2**3 - 1) / math.log2(2) + (2**2 - 1) / math.log2(3)
    )
    if not math.isclose(ndcg_at_k(ranked, relevance, 3), expected_ndcg):
        raise RuntimeError("nDCG regression test failed")


def read_json(handler: BaseHTTPRequestHandler) -> dict[str, Any]:
    length = int(handler.headers.get("content-length", "0"))
    value = json.loads(handler.rfile.read(length))
    if not isinstance(value, dict):
        raise ValueError("expected a JSON object")
    return value


class QualityQdrantHandler(BaseHTTPRequestHandler):
    """Qdrant search double that performs real cosine ranking."""

    def do_GET(self) -> None:  # noqa: N802
        if urlsplit(self.path).path == "/collections":
            self.send_json(200, {"result": {"collections": []}, "status": "ok"})
            return
        self.send_error(404)

    def do_POST(self) -> None:  # noqa: N802
        if not urlsplit(self.path).path.endswith("/points/search"):
            self.send_error(404)
            return
        payload = read_json(self)
        vector = payload.get("vector")
        if not isinstance(vector, list) or len(vector) != EMBEDDING_DIMENSION:
            self.send_json(400, {"status": "invalid fixture query vector"})
            return
        limit = int(payload.get("limit", DEFAULT_TOP_K))
        ranked = sorted(
            self.server.points,  # type: ignore[attr-defined]
            key=lambda point: (-cosine(vector, point["vector"]), str(point["id"])),
        )[:limit]
        result = [
            {
                "id": point["id"],
                "score": cosine(vector, point["vector"]),
                "payload": point["payload"],
            }
            for point in ranked
        ]
        self.send_json(200, {"result": result, "status": "ok"})

    def send_json(self, status: int, payload: Any) -> None:
        body = json.dumps(payload).encode()
        self.send_response(status)
        self.send_header("content-type", "application/json")
        self.send_header("content-length", str(len(body)))
        self.end_headers()
        self.wfile.write(body)

    def log_message(self, _format: str, *_args: Any) -> None:
        return


def stop_process(fixture: Any, process: Any) -> None:
    if process.process.poll() is None:
        process.process.send_signal(signal.SIGTERM)
    try:
        process.process.wait(timeout=5)
    except subprocess.TimeoutExpired:
        process.process.kill()
        process.process.wait(timeout=5)
    process.close()


def evaluate(
    results: list[list[str]], dataset: dict[str, Any]
) -> dict[str, float]:
    cutoffs = dataset["cutoffs"]
    recalls = {
        cutoff: [
            recall_at_k(ranked, query["relevance"], cutoff)
            for ranked, query in zip(results, dataset["queries"], strict=True)
        ]
        for cutoff in cutoffs
    }
    ndcgs = {
        cutoff: [
            ndcg_at_k(ranked, query["relevance"], cutoff)
            for ranked, query in zip(results, dataset["queries"], strict=True)
        ]
        for cutoff in cutoffs
    }
    return {
        **{
            f"recall@{cutoff}": sum(values) / len(values)
            for cutoff, values in recalls.items()
        },
        "mrr": sum(
            mean_reciprocal_rank(ranked, query["relevance"])
            for ranked, query in zip(results, dataset["queries"], strict=True)
        )
        / len(results),
        **{
            f"ndcg@{cutoff}": sum(values) / len(values)
            for cutoff, values in ndcgs.items()
        },
    }


def run_benchmark() -> dict[str, float]:
    fixture = load_fixture_module()
    dataset = read_dataset()
    metric_regression_tests()
    qdrant_server = fixture.start_provider(QualityQdrantHandler)
    ollama_server = fixture.start_provider(fixture.OllamaHandler)
    for document in dataset["documents"]:
        qdrant_server.points.append(
            {
                "id": document["chunkId"],
                "vector": deterministic_embedding(document["text"]),
                "payload": {
                    "chunkId": document["chunkId"],
                    "text": document["text"],
                },
            }
        )

    processes: list[Any] = []
    try:
        with TemporaryDirectory(prefix="ohara-retrieval-quality-") as temporary:
            data_dir = Path(temporary) / "data"
            log_dir = Path(temporary) / "logs"
            data_dir.mkdir()
            log_dir.mkdir()
            retrieval_port = fixture.free_port()
            environment = os.environ.copy()
            environment.update(
                {
                    "OHARA_DATA_DIR": str(data_dir),
                    "OHARA_QDRANT_URL": f"http://127.0.0.1:{qdrant_server.server_port}",
                    "OHARA_FALKORDB_URL": "redis://127.0.0.1:1",
                    "OHARA_LLM_URL": f"http://127.0.0.1:{ollama_server.server_port}",
                    "OHARA_LLM_MODEL": "fixture",
                    "OHARA_EMBEDDING_MODE": "deterministic",
                    "OHARA_RETRIEVAL_BIND": f"127.0.0.1:{retrieval_port}",
                }
            )
            process = fixture.ManagedProcess(
                "retrieval-quality", "ohara-retrieval", environment, log_dir
            )
            processes.append(process)
            retrieval_url = f"http://127.0.0.1:{retrieval_port}"
            fixture.wait_for_http(f"{retrieval_url}/api/health")
            results = []
            for query in dataset["queries"]:
                status, payload = fixture.request_json(
                    "POST",
                    f"{retrieval_url}/api/query",
                    {"query": query["query"], "top_k": max(dataset["cutoffs"])},
                )
                if status != 200 or not isinstance(payload, dict):
                    raise RuntimeError(
                        f"query {query['id']} returned HTTP {status}: {payload}"
                    )
                if payload.get("grounding") != "grounded":
                    raise RuntimeError(f"query {query['id']} was not grounded: {payload}")
                answer = payload.get("answer")
                if not isinstance(answer, str) or not answer.strip():
                    raise RuntimeError(f"query {query['id']} returned an empty answer")
                chunk_ids = [
                    chunk.get("chunkId")
                    for chunk in payload.get("chunks", [])
                    if isinstance(chunk, dict)
                ]
                if any(not isinstance(chunk_id, str) for chunk_id in chunk_ids):
                    raise RuntimeError(f"query {query['id']} returned an invalid chunk")
                citation_ids = payload.get("citations")
                if not isinstance(citation_ids, list) or set(citation_ids) != set(chunk_ids):
                    raise RuntimeError(f"query {query['id']} returned invalid citations")
                results.append(chunk_ids)
            metrics = evaluate(results, dataset)
            minimums = {
                name: float(
                    os.environ.get(
                        f"OHARA_QUALITY_MIN_{name.upper().replace('@', '_AT_')}",
                        1.0,
                    )
                )
                for name in metrics
            }
            failures = [
                f"{name}={value:.3f} is below {minimums[name]:.3f}"
                for name, value in metrics.items()
                if value < minimums[name]
            ]
            if failures:
                raise RuntimeError("; ".join(failures))
            print(f"retrieval quality: {len(results)} golden queries")
            for name, value in metrics.items():
                print(f"  {name}={value:.3f}")
            print("retrieval quality passed: all configured minimums satisfied")
            return metrics
    finally:
        for process in reversed(processes):
            if process.process.poll() is None:
                stop_process(fixture, process)
        qdrant_server.shutdown()
        ollama_server.shutdown()


def main() -> int:
    try:
        run_benchmark()
    except (OSError, RuntimeError, ValueError) as error:
        print(f"retrieval quality failed: {error}", file=sys.stderr)
        return 1
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
