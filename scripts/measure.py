"""Measure a temporary Windows fixture with a built mosh.exe."""

import argparse
import json
import os
from pathlib import Path
import secrets
import socket
import statistics
import subprocess
import tempfile
import time
import urllib.error
import urllib.request


def timed(action):
    start = time.perf_counter()
    value = action()
    return value, round((time.perf_counter() - start) * 1000, 2)


def cli_call(cli, db, args):
    output = subprocess.check_output([cli, "--db", str(db), *args], encoding="utf-8")
    return json.loads(output)


def fixture(root, count):
    directories = [root / f"workspace-{index:02}" for index in range(20)]
    for directory in directories:
        directory.mkdir()
    for index in range(count - 2):
        (directories[index % len(directories)] / f"document-{index:05}.md").write_text(
            f"Document {index}. Route planning and orbit research for milestone {index % 149}.\n",
            encoding="utf-8",
        )
    duplicate = secrets.token_bytes(2 * 1024 * 1024)
    (directories[0] / "record.bin").write_bytes(duplicate)
    (directories[1] / "record-copy.bin").write_bytes(duplicate)
    return directories


def api(cli, db, test):
    with socket.socket() as stock:
        stock.bind(("127.0.0.1", 0))
        port = stock.getsockname()[1]
    token = secrets.token_hex(32)
    with subprocess.Popen(
        [cli, "--db", str(db), "serve", "--port", str(port), "--actor", "benchmark-reader"],
        env={**os.environ, "MOSH_API_TOKEN": token},
        stdout=subprocess.DEVNULL,
        stderr=subprocess.DEVNULL,
    ) as server:
        def request(command, payload, authorized=True):
            headers = {"Content-Type": "application/json"}
            if authorized:
                headers["Authorization"] = f"Bearer {token}"
            query = urllib.request.Request(
                f"http://127.0.0.1:{port}/v1/{command}",
                json.dumps(payload).encode("utf-8"),
                headers,
            )
            with urllib.request.urlopen(query, timeout=4) as response:
                return json.load(response)

        try:
            for _ in range(60):
                try:
                    request("roots", {})
                    break
                except urllib.error.URLError:
                    if server.poll() is not None:
                        raise RuntimeError("local API exited before serving a request")
                    time.sleep(0.1)
            else:
                raise RuntimeError("local API did not open its loopback listener")
            return test(request)
        finally:
            server.terminate()


def measure(cli, count):
    with tempfile.TemporaryDirectory(prefix="mosh-measure-") as temporary:
        home = Path(temporary)
        db = home / "ledger.sqlite3"
        root = home / "indexed"
        root.mkdir()
        directories = fixture(root, count)
        call = lambda *args: cli_call(cli, db, args)
        scan, scan_ms = timed(lambda: call("scan", str(root)))
        assert scan["errors"] == 0 and scan["scanned"] == count + len(directories) + 1
        call("tag", "add", str(directories[0]), "orbit-program")
        call("tag", "add", str(directories[1]), "orbit-program")
        call("tag", "add", str(directories[0]), "active")
        call("person", "benchmark-reader")
        call("secret", str(directories[1]))
        edit = directories[0] / "document-00000.md"
        plan = call("cleave", "--dry-run", "--action", "quarantine", "--query", "name:document-00000.md")
        applied, apply_ms = timed(lambda: call("cleave", "--execute", plan["id"], "--confirm"))
        assert applied["state"] == "done", applied
        assert not edit.exists()
        restored, undo_ms = timed(lambda: call("cleave", "--undo", plan["id"], "--confirm"))
        assert restored["state"] == "undone" and edit.exists()
        duplicates = call("duplicates", str(root))
        assert duplicates["errors"] == 0
        assert len(duplicates["groups"]) == 1 and duplicates["groups"][0]["physical_files"] == 2

        def query_tests(request):
            try:
                request("search", {}, authorized=False)
            except urllib.error.HTTPError as error:
                assert error.code == 401
            else:
                raise AssertionError("unauthenticated query succeeded")
            assert not request("search", {"query": "name:record-copy.bin"})
            queries = {
                "filename": "name:document-00000.md",
                "tag_intersection": "tag:orbit-program AND tag:active",
                "full_text": "content:milestone",
            }
            average = {}
            for name, query in queries.items():
                action = lambda: request("search", {"query": query, "options": {"limit": 25}})
                assert action()
                samples = [timed(action)[1] for _ in range(30)]
                average[name] = round(statistics.mean(samples), 2)
            return average

        query_ms = api(cli, db, query_tests)
        doctor = call("doctor")
        assert doctor["ok"]
        return {
            "physical_files": count,
            "indexed_objects": scan["scanned"],
            "scan_ms": scan_ms,
            "objects_per_second": round(scan["scanned"] * 1000 / scan_ms),
            "database_bytes": doctor["database_bytes"],
            "mean_loopback_query_ms_30_runs_25_results": query_ms,
            "duplicate_scan_ms": duplicates["elapsed_ms"],
            "duplicate_full_hash_bytes": duplicates["hashed_bytes"],
            "duplicate_verified_groups": len(duplicates["groups"]),
            "quarantine_cli_ms": apply_ms,
            "undo_cli_ms": undo_ms,
            "restricted_api_and_missing_token_checked": True,
        }


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("executable", type=Path)
    parser.add_argument("--count", type=int, default=10_000)
    arguments = parser.parse_args()
    if arguments.count < 22:
        parser.error("count must be at least 22")
    results = measure(str(arguments.executable.resolve(strict=True)), arguments.count)
    print(json.dumps(results, indent=2))
