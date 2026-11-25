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

## Packed Ledger on the Linux kernel tree

The Windows x64 release build scanned an extracted upstream Linux kernel snapshot with **97,274 regular files** and **1,677,682,083 source bytes**. The extraction skipped 106 symlinks and 13 filenames that collide under Windows case-insensitive lookup. The test populated the packed Ledger with 100 and then 1,000 total direct tags per regular file using a synthetic 2,048-tag vocabulary. Tags were partly shared by groups of nearby files and partly varied per file. The optional source-text cache stayed off. [Pinned kernel snapshot](https://github.com/torvalds/linux/tree/4dce97c6c265544342e716e62fd24bbb232590c5)

- The initial scan took **66.0 seconds**. The compacted Ledger used **153.4 MB**, including filesystem tags.
- At **100 tags per file**, the compacted Ledger used **173.7 MB**. Tag storage accounted for **21.2 MB**. The complete Ledger added **10.4%** to source bytes.
- At **1,000 tags per file**, the compacted Ledger used **237.9 MB**. Tag storage accounted for **85.4 MB**. The complete Ledger added **14.2%** to source bytes.

The test includes the object registry, metadata search, both tag directions, and the scan audit log. Synthetic bulk writes use the production packing code and omit individual tag-add audit events. The figures measure the database file after compaction. Transaction logs and compaction can temporarily require more free space. Tag-vocabulary diversity, confidence and evidence, history, and optional source indexing can change the footprint.

To reproduce the storage check against an extracted kernel snapshot, set `MOSH_KERNEL_ROOT` to its root directory and run `cargo test --locked --release -p mosh-core --lib kernel_storage_budget -- --ignored --nocapture`.
