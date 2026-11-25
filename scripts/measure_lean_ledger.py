"""Compare two release CLIs on one disposable 10,000-file fixture."""

import argparse
import json
from pathlib import Path
import statistics
import subprocess
import tempfile
import time


def call(cli, db, *args):
    start = time.perf_counter()
    output = subprocess.check_output([cli, "--db", str(db), *args], encoding="utf-8")
    return (time.perf_counter() - start) * 1000, json.loads(output)


def make_tree(root):
    target = None
    for folder in range(100):
        directory = root / f"component-{folder:03}" / "subsystem"
        directory.mkdir(parents=True)
        for index in range(100):
            path = directory / f"unit-{folder * 100 + index:05}.c"
            text = b"int generated_unit;\n"
            if folder == 42 and index == 17:
                target = path
                text = b"// mosh fixture needle\n" + text
            path.write_bytes(text.ljust(4096, b" "))
    return target


def measure(cli, db, root, target):
    call(cli, db, "ledger", "compact")
    _, empty = call(cli, db, "doctor")
    scan_ms, scan = call(cli, db, "scan", str(root))
    assert scan["errors"] == 0, scan
    call(cli, db, "ledger", "compact")
    _, populated = call(cli, db, "doctor")
    before, after = empty["storage"], populated["storage"]
    assert populated["ok"] and after["source_bytes"] == 40_960_000 and after["files"] == 10_000
    tables = {
        key: after["tables"].get(key, 0) - before["tables"].get(key, 0)
        for key in sorted(set(before["tables"]) | set(after["tables"]))
    }
    growth = after["database_bytes"] - before["database_bytes"]
    structural_growth = after["structural_bytes"] - before["structural_bytes"]
    reusable_growth = after["reusable_bytes"] - before["reusable_bytes"]
    assert growth == sum(tables.values()) + structural_growth + reusable_growth
    _, hits = call(cli, db, "search", f"name:{target.name}")
    file_id = hits[0]["id"]
    latency = {}
    queries = {
        "id": f"id:{file_id}",
        "exact_name": f"name:{target.name}",
        "partial_filename": 'name-contains:4217',
        "content_one_folder": f'in:"{target.parent}" AND content:needle',
        "content_entire_scope": "content:needle",
    }
    for name, query in queries.items():
        samples = []
        for _ in range(3):
            duration, hits = call(cli, db, "search", query)
            assert [item["id"] for item in hits] == [file_id], (name, hits)
            samples.append(duration)
        latency[name] = round(statistics.median(samples), 2)
    return {
        "empty_bytes": before["database_bytes"],
        "compacted_bytes": after["database_bytes"],
        "files": after["files"],
        "source_bytes": after["source_bytes"],
        "growth_bytes": growth,
        "growth_bytes_per_file": growth / after["files"],
        "structural_growth_bytes": structural_growth,
        "reusable_growth_bytes": reusable_growth,
        "table_growth_bytes": {key: value for key, value in tables.items() if value},
        "scan_elapsed_ms": scan["elapsed_ms"],
        "scan_wall_ms": round(scan_ms, 2),
        "query_median_wall_ms": latency,
    }


def check_upgrade(before, after, home):
    directory = home / "upgrade-source"
    directory.mkdir()
    source = directory / "current.txt"
    source.write_text("obsolete phrase", encoding="utf-8")
    old_db = home / "old.sqlite3"
    saved_db = home / "backup.sqlite3"
    new_db = home / "upgraded.sqlite3"
    call(before, old_db, "scan", str(directory))
    _, old = call(before, old_db, "search", "name:current.txt")
    file_id = old[0]["id"]
    call(before, old_db, "tag", "add", file_id, "preserved-tag")
    call(before, old_db, "paint", file_id, "--body", "Preserved annotation", "--lock")
    _, cache = call(before, old_db, "ledger", "text-index", "--max-bytes", "131072")
    assert cache["cached_files"] == 1, cache
    call(before, old_db, "backup", str(saved_db))
    source.write_text("current résumé", encoding="utf-8")
    call(after, new_db, "restore", str(saved_db), str(new_db))
    call(after, new_db, "ledger", "rebuild")
    call(after, new_db, "ledger", "compact")
    _, obsolete = call(after, new_db, "search", "content:obsolete")
    _, current = call(after, new_db, "search", "content:resume AND direct-tag:preserved-tag")
    _, annotation = call(after, new_db, "paint", file_id)
    _, doctor = call(after, new_db, "doctor")
    assert not obsolete and [item["id"] for item in current] == [file_id]
    assert annotation["body"] == "Preserved annotation" and annotation["locked"]
    assert doctor["ok"]
    return {"current_content": True, "prior_cached_content_removed": True, "identity_and_annotations_preserved": True}


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("before", type=Path, help="Previous release CLI")
    parser.add_argument("after", type=Path, help="Current release CLI")
    args = parser.parse_args()
    with tempfile.TemporaryDirectory(prefix="mosh-lean-storage-") as temporary:
        path = Path(temporary)
        root = path / "source"
        target = make_tree(root)
        results = {
            "before": measure(args.before.resolve(), path / "before.sqlite3", root, target),
            "after": measure(args.after.resolve(), path / "after.sqlite3", root, target),
            "upgrade": check_upgrade(args.before.resolve(), args.after.resolve(), path),
        }
        print(json.dumps(results, indent=2))


if __name__ == "__main__":
    main()
