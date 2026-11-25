# Verification

Mosh uses generated test directories and a dedicated temporary database. The repository contains synthetic fixture definitions only.

Run `cargo test --locked` to execute the Windows core integration suite. It exercises native file metadata, rename and identity tracking, text search, contextual tags, virtual references, permissions, Git, real Windows directory watching, exact duplicates, hardlinks, backup and restore, and confirmation and undo for physical plans.

The [Windows build workflow](https://github.com/ConnorBache/Mosh/actions/workflows/windows.yml) builds the installer and CLI and exercises the installed WebView2 interface. The smoke test covers directory browsing, metadata inspection, Paint search, Tag Explorer, Marriage references, duplicate analysis, Cleave preview and undo, and History.

## Initial Windows benchmark

Run `python scripts/measure.py <path-to-mosh.exe>` to measure a synthetic local fixture. The first recorded run used a Windows x64 debug CLI with 10,000 generated files across 20 folders. It verified that requests without a token fail and that the restricted API omits a file in a Secret Coll.

- Scan duration was 65.63 seconds for 10,021 objects, averaging 153 objects per second.
- The SQLite database occupied 21,753,856 bytes, excluding WAL and shared-memory files.
- Loopback requests averaged 40.72 ms for an exact filename, 30.76 ms for two inherited tag filters, and 60.28 ms for full text. Each mean uses 30 requests with a 25-row limit and includes HTTP and connection setup.
- Duplicate analysis took 2.715 seconds, fully hashed 4 MiB of candidates, and verified one duplicate group.
- A one-file quarantine took 51.08 ms and undo took 49.73 ms, including CLI startup.

The results describe this fixture and build profile. Large-scale validation, isolated engine latency, and process memory profiling remain deferred.
