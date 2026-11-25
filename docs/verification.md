# Verification

Mosh uses generated test directories and a dedicated temporary database. The repository contains synthetic fixture definitions only.

Run `cargo test --locked` to execute the Windows core integration suite. It exercises native file metadata, rename and identity tracking, text search, contextual tags, virtual references, permissions, Git, real Windows directory watching, exact duplicates, hardlinks, backup and restore, and confirmation and undo for physical plans.

All seven integration tests passed on Windows x64. The optimized CLI also passed a separate 42-file smoke run with the restricted loopback API, duplicate verification, quarantine, undo, and integrity checking.

The v0.1.0 NSIS installer passed a clean local installation and an automated WebView2 smoke test. The test covered directory browsing, metadata inspection, Paint search, Tag Explorer, Marriage references, duplicate analysis, Cleave preview and undo, and History. It used a generated directory and a disposable Ledger, then removed the test installation.

The [Windows build workflow](https://github.com/ConnorBache/Mosh/actions/workflows/windows.yml) builds the installer and CLI. Hosted runner attempts could install the app, but the test harness could not attach to their WebView2 process. The workflow offers the desktop smoke test as an opt-in step for diagnosing that environment. The release uses the installer that passed the local clean-install test.

To repeat the desktop smoke test in an interactive Windows session, install Mosh into a fresh directory, run `npm ci` in `desktop`, and run `node desktop/tests/smoke.mjs <installed-mosh-desktop.exe> <mosh.exe>` from the repository root.

## Initial Windows benchmark

Run `python scripts/measure.py <path-to-mosh.exe>` to measure a synthetic local fixture. The first recorded run used a Windows x64 debug CLI with 10,000 generated files across 20 folders. It verified that requests without a token fail and that the restricted API omits a file in a Secret Coll.

- Scan duration was 65.63 seconds for 10,021 objects, averaging 153 objects per second.
- The SQLite database occupied 21,753,856 bytes, excluding WAL and shared-memory files.
- Loopback requests averaged 40.72 ms for an exact filename, 30.76 ms for two inherited tag filters, and 60.28 ms for full text. Each mean uses 30 requests with a 25-row limit and includes HTTP and connection setup.
- Duplicate analysis took 2.715 seconds, fully hashed 4 MiB of candidates, and verified one duplicate group.
- A one-file quarantine took 51.08 ms and undo took 49.73 ms, including CLI startup.

The results describe this fixture and build profile. Large-scale validation, isolated engine latency, and process memory profiling remain deferred.
