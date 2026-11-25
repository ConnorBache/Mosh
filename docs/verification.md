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

## Intelligent Ledger measurements

The Windows x64 release benchmark created **1,000,000 simulated file metadata records** in one disposable SQLite database. The same database was measured before and after migration. It included a common packed direct tag and a rare tag on one file. It used no physical source files or vectors.

- Building and checkpointing the format 2 fixture took 51.88 seconds. Migrating and checkpointing format 3 took 8.80 seconds.
- The primary database grew from 438,087,680 to 493,289,472 bytes. The increase was 55,201,792 bytes, about **55 bytes per file**, or 12.6% of that metadata-only database. The figures exclude transient SQLite journals.
- The mean of 30 exact-name FTS engine lookups was 0.0300 ms before and 0.0298 ms after migration. A single public-API search intersecting a common tag with a rare tag and an extension returned the only match in 274.98 ms.
- The earlier kernel percentages above used packed tags before format 3. Natural-language history, evidence, and semantic indexes were disabled there.

On 125 generated files, including 120 distractors and separate assignment, related-topic, reversed-role, and deadline conversations, scoped search returned all five target sources first. There were five relevant results across five returned results and a mean reported latency of 1.00 ms in the final test run. A broad keyword search returned 123 candidates in the same fixture. One confirmed miss advanced from unranked to first after enrichment and replay. These are fixture results and do not estimate precision on arbitrary documents.

Real local inference ran through Ollama using SmolLM2 135M Instruct Q4_0 and All-MiniLM 22M in a temporary model store. The models occupied about 138 MB combined. The final embedding search found the relevant document first for a paraphrase across three files in 1.24 seconds, including a failed chat-planning attempt and fallback. Across three effective T1 files, indexing used 568 reported tokens per file on average across the two model roles. The warm indexing run took 5.91 seconds and used $0 in API charges. It produced zero accepted AI assessments and zero verified AI tags. An earlier run with model startup took 58.36 seconds. No token averages are available for T2 through T10. A controlled-endpoint test separately checks accepted and rejected tag evidence, tier downgrade, scoped events, and Paint.

~~~powershell
cargo test --locked --release -- --nocapture
cargo test --locked --release million_record_metadata -- --ignored --nocapture
$env:MOSH_OLLAMA_TEST_URL = 'http://127.0.0.1:11434'
cargo test --locked --release real_ollama_integration -- --ignored --nocapture
~~~

## Ledger format 4 storage

The same 10,000 generated C files on Windows NTFS were scanned with the preceding format 3 build and the format 4 build. Both measurements use the primary SQLite file after compaction, including directory records and indexes. The denominator is the number of regular source files.

- The format 3 baseline used 15,339,520 bytes, or **1,534 bytes per file**. A fresh format 4 Ledger used 6,656,000 bytes, or **666 bytes per file**, including the default filesystem tags and file tiers. The reduction was 56.6%.
- A fresh format 4 Ledger with **100 total direct tags** per file used 8,806,400 bytes, or **881 bytes per file**. With **1,000 total direct tags**, it used 15,622,144 bytes, or **1,562 bytes per file**. These runs use the same synthetic vocabulary and packed tag writer as the kernel test.
- The test upgrade from format 3 used 9,895,936 bytes after compaction, or **990 bytes per file**. It retained the existing audit log plus 270,336 bytes of legacy ID aliases. The test verified the old UUID lookup, a user tag, discovery history, and Ledger integrity.
- The audit table and its index fell from 3,723,264 bytes to 8,192 bytes on the fresh scan. Object records and their lookup indexes fell from 9,744,384 to 4,780,032 bytes. Filename and path search used 974,848 bytes in each fresh scan.
- The separate one-million-record metadata fixture migrated from format 2 through format 4 in 47.90 seconds and preserved the expected packed-tag intersection and legacy ID lookup.

Source text caching, embedding, and AI enrichment were disabled. The benchmark used small synthetic contents to exercise normal filesystem scanning and measures bytes per file. Its ratio to source bytes depends on typical file size. The first scan took 5.38 seconds with format 3 and 10.18 seconds with format 4 in this run. Allocated-space queries now read live filesystem details and may take longer across broad selections.

To reproduce, build the release CLI at commit `fcabd41`, set `MOSH_FORMAT_3_CLI` to that executable, and run `cargo test --locked --release supplemental_storage_budget -- --ignored --nocapture`. The test removes its generated files and Ledgers.
