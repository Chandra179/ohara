#!/usr/bin/env python3
"""Measure peak RSS for every Ohara process with a representative corpus.

The benchmark starts each real Ohara binary in isolation and exercises its
process boundary with a deterministic corpus. Qdrant, Ollama, the scraper
feed, and FalkorDB are small Python standard-library test doubles. Peak RSS is
sampled from Linux ``/proc/<pid>/status`` while each process handles the
workload, so this command does not need psutil or a running Compose stack.

The default indexer mode is deterministic to keep CI reproducible. Set
``OHARA_RESOURCE_BENCHMARK_EMBEDDING_MODE`` to another configured mode when a
local model-memory measurement is needed. Model-backed runs use the model
cache from ``OHARA_RESOURCE_BENCHMARK_MODEL_CACHE`` or ``data/models``.
"""

from __future__ import annotations

from dataclasses import dataclass
import html
import importlib.util
import json
import os
from pathlib import Path
import signal
import shutil
import socket
import subprocess
import sys
import tempfile
import threading
import time
from typing import Any, Callable
from urllib.error import URLError
from urllib.parse import urlsplit
from http.server import BaseHTTPRequestHandler


FIXTURE_PATH = Path(__file__).with_name("pipeline_fixture.py")
ARTIFACT_VERSION = 1
EMBEDDING_DIMENSION = 384
DEFAULT_DOCUMENTS = 8
MAX_DOCUMENTS = 10
SAMPLE_INTERVAL_SECONDS = 0.01
WORKLOAD_TIMEOUT_SECONDS = 60.0


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


def check(condition: bool, message: str) -> None:
    if not condition:
        raise RuntimeError(message)


def atomic_write(path: Path, content: bytes) -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    temporary = path.with_name(f".{path.name}.{os.getpid()}.tmp")
    temporary.write_bytes(content)
    os.replace(temporary, path)


def write_json(path: Path, value: Any) -> None:
    atomic_write(path, json.dumps(value, indent=2).encode())


def json_files(path: Path) -> list[Path]:
    return [item for item in path.glob("*.json") if item.is_file()]


def read_peak_rss_kib(pid: int) -> int | None:
    """Read Linux's high-water resident set size for a live process."""
    try:
        status = Path(f"/proc/{pid}/status").read_text(encoding="utf-8")
    except (FileNotFoundError, OSError):
        return None
    for line in status.splitlines():
        if line.startswith("VmHWM:"):
            fields = line.split()
            if len(fields) >= 2:
                try:
                    return int(fields[1])
                except ValueError:
                    return None
    return None


@dataclass(frozen=True)
class CorpusDocument:
    document_id: str
    title: str
    source_url: str
    html: str
    markdown: str


@dataclass(frozen=True)
class ResourceMeasurement:
    process: str
    peak_rss_kib: int
    workload: str

    @property
    def peak_rss_mib(self) -> float:
        return self.peak_rss_kib / 1024

    def as_json(self) -> dict[str, Any]:
        return {
            "peakRssBytes": self.peak_rss_kib * 1024,
            "peakRssKiB": self.peak_rss_kib,
            "peakRssMiB": round(self.peak_rss_mib, 2),
            "workload": self.workload,
        }


def build_corpus(count: int) -> list[CorpusDocument]:
    paragraph = (
        "Ohara is a private local knowledge base for turning web topics into "
        "searchable evidence. The scraper discovers source pages, cleaning "
        "extracts readable article text, and the indexer creates bounded "
        "chunks with deterministic identities. Retrieval ranks those chunks "
        "and returns a grounded answer with citations. This representative "
        "paragraph contains enough natural language to exercise extraction, "
        "normalization, chunking, vector publication, graph mentions, and "
        "query synthesis."
    )
    documents = []
    for index in range(count):
        document_id = f"resource-document-{index:02d}"
        title = f"Ohara Resource Benchmark Article {index:02d}"
        paragraphs = [
            f"{paragraph} Corpus record {index:02d}, section {part}."
            for part in range(12)
        ]
        markdown = f"# {title}\n\n" + "\n\n".join(paragraphs)
        body = "".join(f"<p>{html.escape(item)}</p>" for item in paragraphs)
        documents.append(
            CorpusDocument(
                document_id=document_id,
                title=title,
                source_url=f"https://example.com/resource/{index}",
                html=(
                    "<!doctype html><html><head>"
                    f"<title>{html.escape(title)}</title></head><body><article>"
                    f"<h1>{html.escape(title)}</h1>{body}</article></body></html>"
                ),
                markdown=markdown,
            )
        )
    return documents


class BenchmarkFeedHandler(BaseHTTPRequestHandler):
    """RSS and HTML fixture with one result per representative document."""

    def do_GET(self) -> None:  # noqa: N802
        path = urlsplit(self.path).path
        documents: list[CorpusDocument] = self.server.documents  # type: ignore[attr-defined]
        if path == "/news":
            items = "".join(
                "<item>"
                f"<title>{html.escape(document.title)}</title>"
                f"<link>http://127.0.0.1:{self.server.server_port}/article/"
                f"{index}.html</link>"
                "</item>"
                for index, document in enumerate(documents)
            )
            body = (
                '<?xml version="1.0" encoding="UTF-8"?>'
                f"<rss version=\"2.0\"><channel>{items}</channel></rss>"
            ).encode()
            self._send_bytes(200, "application/rss+xml", body)
            return
        if path.startswith("/article/") and path.endswith(".html"):
            try:
                index = int(path.removeprefix("/article/").removesuffix(".html"))
                document = documents[index]
            except (IndexError, ValueError):
                self.send_error(404)
                return
            self._send_bytes(200, "text/html", document.html.encode())
            return
        self.send_error(404)

    def _send_bytes(self, status: int, content_type: str, body: bytes) -> None:
        self.send_response(status)
        self.send_header("content-type", content_type)
        self.send_header("content-length", str(len(body)))
        self.end_headers()
        self.wfile.write(body)

    def log_message(self, _format: str, *_args: Any) -> None:
        return


class BenchmarkFeedServer:
    def __init__(self, fixture: Any, documents: list[CorpusDocument]) -> None:
        self.server = fixture.ProviderServer(BenchmarkFeedHandler)
        self.server.documents = documents
        self.thread = threading.Thread(
            target=self.server.serve_forever, name="benchmark-feed", daemon=True
        )
        self.thread.start()

    @property
    def server_port(self) -> int:
        return int(self.server.server_port)

    def shutdown(self) -> None:
        self.server.shutdown()
        self.server.server_close()
        self.thread.join(timeout=2)


class FakeRedisServer:
    """Small RESP2/RESP3 server sufficient for graph and health checks."""

    def __init__(self) -> None:
        self.listener = socket.socket(socket.AF_INET, socket.SOCK_STREAM)
        self.listener.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)
        self.listener.bind(("127.0.0.1", 0))
        self.listener.listen()
        self.listener.settimeout(0.2)
        self.port = int(self.listener.getsockname()[1])
        self.stop_event = threading.Event()
        self.connections: set[socket.socket] = set()
        self.connections_lock = threading.Lock()
        self.query_count = 0
        self.query_count_lock = threading.Lock()
        self.thread = threading.Thread(
            target=self._serve, name="fake-falkordb", daemon=True
        )
        self.thread.start()

    def _serve(self) -> None:
        while not self.stop_event.is_set():
            try:
                connection, _address = self.listener.accept()
            except socket.timeout:
                continue
            except OSError:
                return
            with self.connections_lock:
                self.connections.add(connection)
            threading.Thread(
                target=self._handle, args=(connection,), daemon=True
            ).start()

    def _handle(self, connection: socket.socket) -> None:
        try:
            reader = connection.makefile("rb")
            while not self.stop_event.is_set():
                command = self._read_command(reader)
                if command is None:
                    return
                name = command[0].upper()
                if name == b"HELLO":
                    response = (
                        b"%2\r\n$6\r\nserver\r\n$5\r\nredis\r\n"
                        b"$7\r\nversion\r\n$3\r\n7.2\r\n"
                    )
                elif name == b"PING":
                    response = b"+PONG\r\n"
                elif name == b"GRAPH.QUERY":
                    with self.query_count_lock:
                        self.query_count += 1
                    response = b"+OK\r\n"
                else:
                    response = b"+OK\r\n"
                connection.sendall(response)
        except (ConnectionError, OSError, ValueError):
            return
        finally:
            try:
                reader.close()
            except (UnboundLocalError, OSError):
                pass
            with self.connections_lock:
                self.connections.discard(connection)
            connection.close()

    @staticmethod
    def _read_command(reader: Any) -> list[bytes] | None:
        header = reader.readline()
        if not header:
            return None
        if not header.startswith(b"*"):
            raise ValueError("fake redis expected an array command")
        count = int(header[1:-2])
        command = []
        for _ in range(count):
            length_header = reader.readline()
            if not length_header.startswith(b"$"):
                raise ValueError("fake redis expected bulk command arguments")
            length = int(length_header[1:-2])
            value = reader.read(length)
            if len(value) != length or reader.read(2) != b"\r\n":
                raise ValueError("fake redis received a truncated command")
            command.append(value)
        return command

    def shutdown(self) -> None:
        self.stop_event.set()
        try:
            self.listener.close()
        except OSError:
            pass
        with self.connections_lock:
            connections = list(self.connections)
        for connection in connections:
            try:
                connection.shutdown(socket.SHUT_RDWR)
            except OSError:
                pass
            connection.close()
        self.thread.join(timeout=2)


class MeasuredProcess:
    def __init__(
        self, name: str, binary: str, environment: dict[str, str], log_dir: Path, root: Path
    ) -> None:
        self.name = name
        self.log_path = log_dir / f"{name}.log"
        self.log_file = self.log_path.open("w", encoding="utf-8")
        self.process = subprocess.Popen(
            [str(root / "target" / "debug" / binary)],
            cwd=root,
            env=environment,
            stdout=self.log_file,
            stderr=subprocess.STDOUT,
            text=True,
        )
        self.peak_rss_kib = read_peak_rss_kib(self.process.pid) or 0
        self.stop_event = threading.Event()
        self.sampler = threading.Thread(
            target=self._sample_loop, name=f"rss-{name}", daemon=True
        )
        self.sampler.start()

    def _sample_loop(self) -> None:
        while not self.stop_event.is_set():
            self.sample()
            if self.process.poll() is not None:
                return
            self.stop_event.wait(SAMPLE_INTERVAL_SECONDS)

    def sample(self) -> None:
        current = read_peak_rss_kib(self.process.pid)
        if current is not None:
            self.peak_rss_kib = max(self.peak_rss_kib, current)

    def wait_until(self, predicate: Callable[[], bool], description: str) -> None:
        deadline = time.monotonic() + WORKLOAD_TIMEOUT_SECONDS
        while time.monotonic() < deadline:
            self.sample()
            if predicate():
                self.sample()
                return
            returncode = self.process.poll()
            if returncode is not None:
                output = self.log_path.read_text(encoding="utf-8")
                raise RuntimeError(
                    f"{self.name} exited with status {returncode} while waiting for "
                    f"{description}\n{output}"
                )
            time.sleep(0.05)
        raise RuntimeError(f"timed out waiting for {self.name} {description}")

    def stop(self) -> None:
        self.sample()
        if self.process.poll() is None:
            self.process.send_signal(signal.SIGTERM)
        try:
            self.process.wait(timeout=5)
        except subprocess.TimeoutExpired:
            self.process.kill()
            self.process.wait(timeout=5)
        self.sample()
        self.stop_event.set()
        self.sampler.join(timeout=2)
        self.log_file.close()

    def measurement(self, workload: str) -> ResourceMeasurement:
        return ResourceMeasurement(self.name, self.peak_rss_kib, workload)


def base_environment(data_dir: Path, qdrant_url: str, falkordb_url: str, llm_url: str) -> dict[str, str]:
    environment = os.environ.copy()
    environment.update(
        {
            "OHARA_DATA_DIR": str(data_dir),
            "OHARA_QDRANT_URL": qdrant_url,
            "OHARA_FALKORDB_URL": falkordb_url,
            "OHARA_FALKORDB_GRAPH": "ohara-resource-benchmark",
            "OHARA_LLM_URL": llm_url,
            "OHARA_LLM_MODEL": "fixture",
        }
    )
    embedding_mode = os.environ.get(
        "OHARA_RESOURCE_BENCHMARK_EMBEDDING_MODE", "deterministic"
    )
    if embedding_mode:
        environment["OHARA_EMBEDDING_MODE"] = embedding_mode
    return environment


def seed_directories(data_dir: Path) -> None:
    for relative in [
        "raw",
        "clean",
        "indexed",
        "catalog",
        "inbox/cleaning",
        "inbox/indexer",
        "inbox/graph",
        "dead-letter/cleaning",
        "dead-letter/indexer",
        "dead-letter/graph",
        "state",
        "models",
    ]:
        (data_dir / relative).mkdir(parents=True, exist_ok=True)


def seed_model_cache(data_dir: Path) -> None:
    """Expose the configured local model cache to the isolated indexer run."""
    source = Path(
        os.environ.get("OHARA_RESOURCE_BENCHMARK_MODEL_CACHE", "data/models")
    ).expanduser()
    if not source.is_dir():
        raise RuntimeError(
            "model-backed resource benchmark requires a model cache directory at "
            f"{source}; set OHARA_RESOURCE_BENCHMARK_MODEL_CACHE to an existing cache"
        )
    target = data_dir / "models"
    target.rmdir()
    try:
        target.symlink_to(source.resolve(), target_is_directory=True)
    except OSError:
        shutil.copytree(source, target)


def catalog_value(document: CorpusDocument) -> dict[str, Any]:
    return {
        "id": document.document_id,
        "sourceUrl": document.source_url,
        "title": document.title,
        "status": "NEW",
        "chunkCount": 0,
        "createdAt": "resource-benchmark",
        "lastProcessedAt": None,
        "error": None,
    }


def raw_artifact(document: CorpusDocument) -> dict[str, Any]:
    return {
        "schema_version": ARTIFACT_VERSION,
        "document_id": document.document_id,
        "source_url": document.source_url,
        "title": document.title,
        "raw_path": f"raw/{document.document_id}.html",
    }


def clean_artifact(document: CorpusDocument) -> dict[str, Any]:
    return {
        "schema_version": ARTIFACT_VERSION,
        "document_id": document.document_id,
        "source_url": document.source_url,
        "title": document.title,
        "markdown": document.markdown,
    }


def indexed_artifact(document: CorpusDocument) -> dict[str, Any]:
    chunks = []
    paragraphs = document.markdown.split("\n\n")
    for sequence, text in enumerate(paragraphs):
        chunks.append(
            {
                "chunk_id": f"{document.document_id}-chunk-{sequence}",
                "document_id": document.document_id,
                "title": document.title,
                "source_url": document.source_url,
                "text": text,
                "sequence": sequence,
            }
        )
    return {
        "schema_version": ARTIFACT_VERSION,
        "document_id": document.document_id,
        "source_url": document.source_url,
        "title": document.title,
        "chunks": chunks,
        "indexed_at": "resource-benchmark",
    }


def seed_cleaning(data_dir: Path, documents: list[CorpusDocument]) -> None:
    seed_directories(data_dir)
    for document in documents:
        atomic_write(
            data_dir / "raw" / f"{document.document_id}.html", document.html.encode()
        )
        write_json(data_dir / "catalog" / f"{document.document_id}.json", catalog_value(document))
        write_json(
            data_dir / "inbox/cleaning" / f"{document.document_id}.json",
            raw_artifact(document),
        )


def seed_indexer(data_dir: Path, documents: list[CorpusDocument]) -> None:
    seed_directories(data_dir)
    for document in documents:
        write_json(data_dir / "catalog" / f"{document.document_id}.json", catalog_value(document))
        write_json(
            data_dir / "inbox/indexer" / f"{document.document_id}.json",
            clean_artifact(document),
        )


def seed_graph(data_dir: Path, documents: list[CorpusDocument]) -> None:
    seed_directories(data_dir)
    for document in documents:
        write_json(
            data_dir / "inbox/graph" / f"{document.document_id}.json",
            indexed_artifact(document),
        )


def start_measured(
    fixture: Any,
    name: str,
    binary: str,
    environment: dict[str, str],
    log_dir: Path,
) -> MeasuredProcess:
    return MeasuredProcess(name, binary, environment, log_dir, fixture.ROOT)


def run_scraper(
    fixture: Any, documents: list[CorpusDocument], log_dir: Path
) -> ResourceMeasurement:
    with tempfile.TemporaryDirectory(prefix="ohara-resource-scraper-") as temporary:
        data_dir = Path(temporary) / "data"
        feed = BenchmarkFeedServer(fixture, documents)
        scraper_bind = f"127.0.0.1:{fixture.free_port()}"
        scraper_config = Path(temporary) / "scraper-config.yaml"
        scraper_config.write_text(
            f"""bind: {scraper_bind}
search:
  provider: rss
  url: http://127.0.0.1:{feed.server_port}/news
fetch:
  kind: http
""",
            encoding="utf-8",
        )
        process = start_measured(
            fixture,
            "scraper",
            "ohara-scraper",
            base_environment(
                data_dir, "http://127.0.0.1:1", "redis://127.0.0.1:1", "http://127.0.0.1:1"
            )
            | {
                "OHARA_SCRAPER_CONFIG": str(scraper_config),
            },
            log_dir,
        )
        try:
            fixture.wait_for_http(
                f"http://{scraper_bind}/health", expected_status=204
            )
            status, payload = fixture.request_json(
                "POST",
                f"http://{scraper_bind}/scrape",
                {"topic": "representative resource corpus", "limit": len(documents)},
            )
            check(status == 200, f"scraper returned HTTP {status}: {payload}")
            check(
                isinstance(payload, dict)
                and payload.get("discovered") == len(documents)
                and payload.get("enqueued") == len(documents),
                f"scraper did not enqueue the representative corpus: {payload}",
            )
            process.wait_until(
                lambda: len(json_files(data_dir / "raw")) >= len(documents),
                "raw artifacts",
            )
        finally:
            process.stop()
            feed.shutdown()
        return process.measurement("topic scrape and raw publication")


def run_cleaning(fixture: Any, documents: list[CorpusDocument], log_dir: Path) -> ResourceMeasurement:
    with tempfile.TemporaryDirectory(prefix="ohara-resource-cleaning-") as temporary:
        data_dir = Path(temporary) / "data"
        seed_cleaning(data_dir, documents)
        process = start_measured(
            fixture,
            "cleaning",
            "ohara-cleaning",
            base_environment(
                data_dir, "http://127.0.0.1:1", "redis://127.0.0.1:1", "http://127.0.0.1:1"
            ),
            log_dir,
        )
        try:
            process.wait_until(
                lambda: len(json_files(data_dir / "clean")) >= len(documents),
                "clean artifacts",
            )
        finally:
            process.stop()
        return process.measurement("representative raw corpus extraction")


def run_indexer(
    fixture: Any, documents: list[CorpusDocument], log_dir: Path
) -> ResourceMeasurement:
    with tempfile.TemporaryDirectory(prefix="ohara-resource-indexer-") as temporary:
        data_dir = Path(temporary) / "data"
        seed_indexer(data_dir, documents)
        embedding_mode = os.environ.get(
            "OHARA_RESOURCE_BENCHMARK_EMBEDDING_MODE", "deterministic"
        )
        if embedding_mode != "deterministic":
            seed_model_cache(data_dir)
        qdrant = fixture.start_provider(fixture.QdrantHandler)
        process = start_measured(
            fixture,
            "indexer",
            "ohara-indexer",
            base_environment(
                data_dir,
                f"http://127.0.0.1:{qdrant.server_port}",
                "redis://127.0.0.1:1",
                "http://127.0.0.1:1",
            ),
            log_dir,
        )
        try:
            process.wait_until(
                lambda: len(json_files(data_dir / "indexed")) >= len(documents),
                "indexed artifacts",
            )
            check(
                len(qdrant.points) >= len(documents),
                "indexer did not publish representative vectors",
            )
        finally:
            process.stop()
            qdrant.shutdown()
        return process.measurement("representative clean corpus chunking and indexing")


def run_graph(
    fixture: Any, documents: list[CorpusDocument], log_dir: Path
) -> ResourceMeasurement:
    with tempfile.TemporaryDirectory(prefix="ohara-resource-graph-") as temporary:
        data_dir = Path(temporary) / "data"
        seed_graph(data_dir, documents)
        falkordb = FakeRedisServer()
        process = start_measured(
            fixture,
            "graph",
            "ohara-graph",
            base_environment(
                data_dir,
                "http://127.0.0.1:1",
                f"redis://127.0.0.1:{falkordb.port}",
                "http://127.0.0.1:1",
            ),
            log_dir,
        )
        try:
            process.wait_until(
                lambda: not json_files(data_dir / "inbox/graph")
                and falkordb.query_count >= len(documents),
                "graph writes",
            )
        finally:
            process.stop()
            falkordb.shutdown()
        return process.measurement("representative indexed corpus graph publication")


def run_retrieval(
    fixture: Any, documents: list[CorpusDocument], log_dir: Path
) -> ResourceMeasurement:
    with tempfile.TemporaryDirectory(prefix="ohara-resource-retrieval-") as temporary:
        data_dir = Path(temporary) / "data"
        seed_directories(data_dir)
        embedding_mode = os.environ.get(
            "OHARA_RESOURCE_BENCHMARK_EMBEDDING_MODE", "deterministic"
        )
        if embedding_mode != "deterministic":
            seed_model_cache(data_dir)
        qdrant = fixture.start_provider(fixture.QdrantHandler)
        ollama = fixture.start_provider(fixture.OllamaHandler)
        falkordb = FakeRedisServer()
        for document in documents:
            qdrant.points.append(
                {
                    "id": f"{document.document_id}-point",
                    "vector": [0.0] * EMBEDDING_DIMENSION,
                    "payload": {
                        "chunkId": f"{document.document_id}-chunk-0",
                        "documentId": document.document_id,
                        "title": document.title,
                        "sourceUrl": document.source_url,
                        "text": document.markdown,
                    },
                }
            )
        port = fixture.free_port()
        environment = base_environment(
            data_dir,
            f"http://127.0.0.1:{qdrant.server_port}",
            f"redis://127.0.0.1:{falkordb.port}",
            f"http://127.0.0.1:{ollama.server_port}",
        ) | {"OHARA_RETRIEVAL_BIND": f"127.0.0.1:{port}"}
        process = start_measured(
            fixture, "retrieval", "ohara-retrieval", environment, log_dir
        )
        try:
            fixture.wait_for_http(f"http://127.0.0.1:{port}/api/health")
            status, payload = fixture.request_json(
                "POST",
                f"http://127.0.0.1:{port}/api/query",
                {"query": "What is Ohara?", "top_k": len(documents)},
            )
            check(status == 200, f"retrieval returned HTTP {status}: {payload}")
            check(
                isinstance(payload, dict)
                and payload.get("grounding") == "grounded"
                and isinstance(payload.get("answer"), str)
                and payload["answer"].strip(),
                f"retrieval did not return a grounded answer: {payload}",
            )
        finally:
            process.stop()
            falkordb.shutdown()
            qdrant.shutdown()
            ollama.shutdown()
        return process.measurement("representative ranked query and synthesis")


def run_benchmark() -> list[ResourceMeasurement]:
    if not Path("/proc/self/status").is_file():
        raise RuntimeError("peak RSS benchmark requires Linux /proc process statistics")
    fixture = load_fixture_module()
    document_count = positive_int("OHARA_RESOURCE_BENCHMARK_DOCUMENTS", DEFAULT_DOCUMENTS)
    if document_count > MAX_DOCUMENTS:
        raise ValueError(
            f"OHARA_RESOURCE_BENCHMARK_DOCUMENTS must be at most {MAX_DOCUMENTS}"
        )
    documents = build_corpus(document_count)
    with tempfile.TemporaryDirectory(prefix="ohara-resource-logs-") as temporary:
        log_dir = Path(temporary)
        measurements = [
            run_scraper(fixture, documents, log_dir),
            run_cleaning(fixture, documents, log_dir),
            run_indexer(fixture, documents, log_dir),
            run_graph(fixture, documents, log_dir),
            run_retrieval(fixture, documents, log_dir),
        ]
    output = os.environ.get("OHARA_RESOURCE_BENCHMARK_OUTPUT")
    if output:
        output_path = Path(output)
        write_json(
            output_path,
            {
                "documents": document_count,
                "embeddingMode": os.environ.get(
                    "OHARA_RESOURCE_BENCHMARK_EMBEDDING_MODE", "deterministic"
                ),
                "processes": {
                    item.process: item.as_json() for item in measurements
                },
            },
        )
    return measurements


def main() -> int:
    try:
        measurements = run_benchmark()
        for item in measurements:
            print(
                f"resource benchmark: {item.process:<9} peak RSS="
                f"{item.peak_rss_mib:.1f} MiB ({item.peak_rss_kib} KiB) — {item.workload}"
            )
        print("resource benchmark complete: all five process workloads passed")
    except (OSError, RuntimeError, ValueError, URLError) as error:
        print(f"resource benchmark failed: {error}", file=sys.stderr)
        return 1
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
