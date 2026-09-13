#!/usr/bin/env python3
"""Run the deterministic scrape-to-query pipeline fixture.

The runner uses only Python's standard library for local test doubles. It
starts the real Ohara scraper, cleaning, indexer, and retrieval binaries, then
checks their JSON/file boundaries with no external network or model download.
"""

from __future__ import annotations

import json
import os
from pathlib import Path
import signal
import socket
import subprocess
import sys
import tempfile
import threading
import time
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from typing import Any
from urllib.error import HTTPError, URLError
from urllib.parse import urlsplit
from urllib.request import Request, urlopen


ROOT = Path(__file__).resolve().parents[1]
POLL_INTERVAL_SECONDS = 0.1
PROCESS_START_TIMEOUT_SECONDS = 15.0
PIPELINE_TIMEOUT_SECONDS = 30.0


def free_port() -> int:
    with socket.socket(socket.AF_INET, socket.SOCK_STREAM) as listener:
        listener.bind(("127.0.0.1", 0))
        return int(listener.getsockname()[1])


class QuietHandler(BaseHTTPRequestHandler):
    def log_message(self, _format: str, *_args: Any) -> None:
        return

    def send_json(self, status: int, payload: Any) -> None:
        body = json.dumps(payload).encode()
        self.send_response(status)
        self.send_header("content-type", "application/json")
        self.send_header("content-length", str(len(body)))
        self.end_headers()
        self.wfile.write(body)


class FixtureHandler(QuietHandler):
    def do_GET(self) -> None:  # noqa: N802
        if self.path.split("?", 1)[0] == "/news":
            article_url = (
                f"http://127.0.0.1:{self.server.server_port}/article.html"
            )
            body = f"""<?xml version="1.0" encoding="UTF-8"?>
<rss version="2.0"><channel><title>Ohara fixture feed</title>
<item><title>Ohara fixture article</title><link>{article_url}</link></item>
</channel></rss>""".encode()
            self.send_response(200)
            self.send_header("content-type", "application/rss+xml")
            self.send_header("content-length", str(len(body)))
            self.end_headers()
            self.wfile.write(body)
            return

        if self.path == "/article.html":
            body = """<!doctype html>
<html><head><title>Ohara fixture article</title></head>
<body><article>
<h1>Ohara fixture article</h1>
<p>Ohara is a private local knowledge base that turns web topics into searchable evidence.</p>
<p>The scraper fetches source pages, cleaning extracts the article, and the indexer creates searchable chunks.</p>
<p>Retrieval finds the relevant evidence and returns a local grounded answer with citations.</p>
</article></body></html>""".encode()
            self.send_response(200)
            self.send_header("content-type", "text/html")
            self.send_header("content-length", str(len(body)))
            self.end_headers()
            self.wfile.write(body)
            return

        self.send_error(404)


class QdrantHandler(QuietHandler):
    def do_GET(self) -> None:  # noqa: N802
        if urlsplit(self.path).path == "/collections":
            self.send_json(200, {"result": {"collections": []}, "status": "ok"})
            return
        self.send_error(404)

    def do_PUT(self) -> None:  # noqa: N802
        path = urlsplit(self.path).path
        if path.endswith("/points"):
            payload = read_json(self)
            points = payload.get("points", [])
            if not isinstance(points, list) or any(
                not isinstance(point, dict)
                or not isinstance(point.get("vector"), list)
                or len(point["vector"]) != 384
                for point in points
            ):
                self.send_json(400, {"status": "invalid fixture vector"})
                return
            self.server.points.extend(points)
            self.send_json(200, {"result": True, "status": "ok"})
            return
        if path.startswith("/collections/"):
            self.send_json(200, {"result": True, "status": "ok"})
            return
        self.send_error(404)

    def do_POST(self) -> None:  # noqa: N802
        if urlsplit(self.path).path.endswith("/points/search"):
            payload = read_json(self)
            vector = payload.get("vector")
            if not isinstance(vector, list) or len(vector) != 384:
                self.send_json(400, {"status": "invalid fixture query vector"})
                return
            limit = int(payload.get("limit", len(self.server.points)))
            points = [
                {
                    "id": point.get("id"),
                    "score": 1.0,
                    "payload": point.get("payload", {}),
                }
                for point in self.server.points[:limit]
            ]
            self.send_json(200, {"result": points, "status": "ok"})
            return
        self.send_error(404)


class OllamaHandler(QuietHandler):
    def do_GET(self) -> None:  # noqa: N802
        if self.path == "/api/tags":
            self.send_json(200, {"models": [{"name": "fixture"}]})
            return
        self.send_error(404)

    def do_POST(self) -> None:  # noqa: N802
        if self.path == "/api/generate":
            read_json(self)
            self.send_json(
                200,
                {
                    "response": (
                        "Ohara is a private local knowledge base built from local artifacts."
                    )
                },
            )
            return
        self.send_error(404)


class ProviderServer(ThreadingHTTPServer):
    allow_reuse_address = True

    def __init__(self, handler: type[BaseHTTPRequestHandler]) -> None:
        super().__init__(("127.0.0.1", 0), handler)
        self.points: list[dict[str, Any]] = []


def read_json(handler: BaseHTTPRequestHandler) -> dict[str, Any]:
    length = int(handler.headers.get("content-length", "0"))
    value = json.loads(handler.rfile.read(length))
    if not isinstance(value, dict):
        raise ValueError("expected a JSON object")
    return value


def start_provider(handler: type[BaseHTTPRequestHandler]) -> ProviderServer:
    server = ProviderServer(handler)
    threading.Thread(target=server.serve_forever, daemon=True).start()
    return server


def request_json(
    method: str,
    url: str,
    payload: dict[str, Any] | None = None,
    headers: dict[str, str] | None = None,
) -> tuple[int, Any]:
    body = None if payload is None else json.dumps(payload).encode()
    request_headers = {
        "accept": "application/json",
        "content-type": "application/json",
    }
    if headers:
        request_headers.update(headers)
    request = Request(
        url,
        data=body,
        headers=request_headers,
        method=method,
    )
    try:
        with urlopen(request, timeout=2) as response:
            response_body = response.read()
            return response.status, json.loads(response_body) if response_body else None
    except HTTPError as error:
        response_body = error.read()
        try:
            payload_value = json.loads(response_body) if response_body else None
        except json.JSONDecodeError:
            payload_value = response_body.decode(errors="replace")
        return error.code, payload_value


def wait_for_http(url: str, expected_status: int | None = None) -> None:
    deadline = time.monotonic() + PROCESS_START_TIMEOUT_SECONDS
    while time.monotonic() < deadline:
        try:
            status, _payload = request_json("GET", url)
            if expected_status is None or status == expected_status:
                return
        except (OSError, URLError, ValueError):
            pass
        time.sleep(POLL_INTERVAL_SECONDS)
    raise RuntimeError(f"timed out waiting for {url}")


def wait_for_file(path: Path, processes: list["ManagedProcess"]) -> None:
    deadline = time.monotonic() + PIPELINE_TIMEOUT_SECONDS
    while time.monotonic() < deadline:
        if path.is_file():
            return
        for process in processes:
            if process.process.poll() is not None:
                raise RuntimeError(
                    f"{process.name} exited with status {process.process.returncode}"
                )
        time.sleep(POLL_INTERVAL_SECONDS)
    raise RuntimeError(f"timed out waiting for {path}")


class ManagedProcess:
    def __init__(self, name: str, binary: str, environment: dict[str, str], log_dir: Path):
        self.name = name
        self.log_path = log_dir / f"{name}.log"
        self.log_file = self.log_path.open("w", encoding="utf-8")
        self.process = subprocess.Popen(
            [str(ROOT / "target" / "debug" / binary)],
            cwd=ROOT,
            env=environment,
            stdout=self.log_file,
            stderr=subprocess.STDOUT,
            text=True,
        )

    def close(self) -> None:
        self.log_file.close()


def check(condition: bool, message: str) -> None:
    if not condition:
        raise RuntimeError(message)


def run_pipeline() -> None:
    binaries = {
        "scraper": "ohara-scraper",
        "cleaning": "ohara-cleaning",
        "indexer": "ohara-indexer",
        "retrieval": "ohara-retrieval",
    }
    with tempfile.TemporaryDirectory(prefix="ohara-pipeline-") as temporary:
        data_dir = Path(temporary) / "data"
        log_dir = Path(temporary) / "logs"
        data_dir.mkdir()
        log_dir.mkdir()
        fixture_server = start_provider(FixtureHandler)
        qdrant_server = start_provider(QdrantHandler)
        ollama_server = start_provider(OllamaHandler)
        processes: list[ManagedProcess] = []
        try:
            scraper_port = free_port()
            retrieval_port = free_port()
            base_environment = os.environ.copy()
            base_environment.update(
                {
                    "OHARA_DATA_DIR": str(data_dir),
                    "OHARA_QDRANT_URL": f"http://127.0.0.1:{qdrant_server.server_port}",
                    "OHARA_LLM_URL": f"http://127.0.0.1:{ollama_server.server_port}",
                    "OHARA_LLM_MODEL": "fixture",
                    "OHARA_EMBEDDING_MODE": "deterministic",
                    "OHARA_PROCESS_AUTH_TOKEN": "fixture-process-token",
                }
            )

            scraper_environment = base_environment | {
                "OHARA_SCRAPER_BIND": f"127.0.0.1:{scraper_port}",
                "OHARA_SCRAPER_SEARCH_URL": (
                    f"http://127.0.0.1:{fixture_server.server_port}/news"
                ),
            }
            retrieval_environment = base_environment | {
                "OHARA_RETRIEVAL_BIND": f"127.0.0.1:{retrieval_port}",
                "OHARA_SCRAPER_URL": f"http://127.0.0.1:{scraper_port}",
            }
            processes.extend(
                [
                    ManagedProcess("scraper", binaries["scraper"], scraper_environment, log_dir),
                    ManagedProcess("cleaning", binaries["cleaning"], base_environment, log_dir),
                    ManagedProcess("indexer", binaries["indexer"], base_environment, log_dir),
                    ManagedProcess(
                        "retrieval", binaries["retrieval"], retrieval_environment, log_dir
                    ),
                ]
            )

            wait_for_http(f"http://127.0.0.1:{scraper_port}/health", expected_status=204)
            wait_for_http(f"http://127.0.0.1:{retrieval_port}/api/health")
            status, auth_payload = request_json(
                "POST",
                f"http://127.0.0.1:{scraper_port}/scrape",
                {"topic": "deterministic fixture topic", "limit": 1},
            )
            check(
                status == 401,
                f"unauthenticated scrape returned HTTP {status}: {auth_payload}",
            )
            status, scrape_payload = request_json(
                "POST",
                f"http://127.0.0.1:{retrieval_port}/api/topics/scrape",
                {"topic": "deterministic fixture topic", "limit": 1},
            )
            check(status == 200, f"scrape returned HTTP {status}: {scrape_payload}")
            check(isinstance(scrape_payload, dict), "scrape did not return an object")
            check(scrape_payload.get("discovered") == 1, "fixture result was not discovered")
            check(scrape_payload.get("enqueued") == 1, "fixture result was not enqueued")
            documents = scrape_payload.get("documents")
            check(isinstance(documents, list) and len(documents) == 1, "missing fixture document")
            document_id = documents[0]["documentId"]

            raw_artifact = data_dir / "raw" / f"{document_id}.json"
            clean_artifact = data_dir / "clean" / f"{document_id}.json"
            indexed_artifact = data_dir / "indexed" / f"{document_id}.json"
            wait_for_file(raw_artifact, processes)
            wait_for_file(clean_artifact, processes)
            wait_for_file(indexed_artifact, processes)

            clean_payload = json.loads(clean_artifact.read_text(encoding="utf-8"))
            indexed_payload = json.loads(indexed_artifact.read_text(encoding="utf-8"))
            check(
                "Ohara is a private local knowledge base" in clean_payload["markdown"],
                "clean artifact did not contain the fixture article",
            )
            check(
                indexed_payload["chunks"]
                and indexed_payload["chunks"][0]["document_id"] == document_id,
                "indexed artifact did not contain the fixture chunk",
            )

            check(not (data_dir / "inbox/cleaning" / f"{document_id}.json").exists(), "cleaning inbox item remained")
            check(not (data_dir / "inbox/indexer" / f"{document_id}.json").exists(), "indexer inbox item remained")
            check((data_dir / "inbox/graph" / f"{document_id}.json").is_file(), "graph handoff was not published")

            status, query_payload = request_json(
                "POST",
                f"http://127.0.0.1:{retrieval_port}/api/query",
                {"query": "What is Ohara?", "top_k": 1},
            )
            check(status == 200, f"query returned HTTP {status}: {query_payload}")
            check(isinstance(query_payload, dict), "query did not return an object")
            check(query_payload.get("grounding") == "grounded", "query was not grounded")
            check(
                isinstance(query_payload.get("answer"), str)
                and query_payload["answer"].strip(),
                "query answer was empty",
            )
            chunks = query_payload.get("chunks")
            citations = query_payload.get("citations")
            check(isinstance(chunks, list) and chunks, "query returned no chunks")
            check(isinstance(citations, list) and citations, "query returned no citations")
            check(citations[0] == chunks[0]["chunkId"], "citation did not reference evidence")
            print(f"pipeline fixture passed: {document_id}")
        finally:
            for process in reversed(processes):
                if process.process.poll() is None:
                    process.process.send_signal(signal.SIGTERM)
            for process in reversed(processes):
                try:
                    process.process.wait(timeout=5)
                except subprocess.TimeoutExpired:
                    process.process.kill()
                    process.process.wait(timeout=5)
                process.close()
            fixture_server.shutdown()
            qdrant_server.shutdown()
            ollama_server.shutdown()
            if any(process.process.returncode not in (0, -signal.SIGTERM) for process in processes):
                for process in processes:
                    output = process.log_path.read_text(encoding="utf-8")
                    if output:
                        print(f"--- {process.name} log ---\n{output}", file=sys.stderr)


def main() -> int:
    try:
        run_pipeline()
    except (OSError, RuntimeError, ValueError) as error:
        print(f"pipeline fixture failed: {error}", file=sys.stderr)
        return 1
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
