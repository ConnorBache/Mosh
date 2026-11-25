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

## Preview storage and churn

Run `cargo test --locked --release storage_churn_workload -- --ignored --nocapture` to reproduce the generated NTFS workload. Each of the three tag distributions starts with 1,200 files and 4,922,490 source bytes. The churn phase edits, renames and removes 60 files each, then rescans twice. It finishes with 1,140 active files and 4,726,720 source bytes.

- The same fresh records and annotations occupied **1,007 bytes per file** with the prior index layout and **867 bytes per file** with the preview layout. The two layouts were compacted and compared on the same fixture. A warm exact-path lookup averaged 0.0045 ms with the previous layout and 0.0057 ms with the preview.
- Of the **1,040,384 occupied bytes** before bulk tagging, file rows and key maps used 327,680 bytes (273 per file), other file indexes 184,320 (154 per file), and name/path search 176,128 (147 per file). Packed tags and tiers each used 49,152 bytes. Audit and vocabulary history each used 12,288 bytes, with zero routine scan events and two durable audit events. Paint used 4,096 bytes, optional intelligence 69,632, source cache 20,480, and other schema and application tables 135,168. The compacted database had zero reusable or structural bytes outside those groups. Empty feature tables and indexes still occupy SQLite pages.
- With 100 and 1,000 direct tags, compacted bytes per active file before churn were **1,007 and 1,051** for repeated sets, **1,212 and 1,683** for overlapping sets, and **1,324 and 1,693** for diverse sets. With the compact path index but the previous tag codec, the 1,000-tag overlapping and diverse cases used **2,000** and **3,417** bytes per file respectively. The 100-tag measurements were unchanged before churn.
- After churn the corresponding counts were **1,132 and 1,132**, **1,358 and 1,782**, and **1,484 and 1,793**. The denominator changes when files are removed. At 1,000 diverse tags, the previous tag codec used **3,553** bytes per active file after churn. Durable user annotations and the identity across a rename were verified.
- First scans took 1.45–1.46 s. Churn scans took 0.35–0.39 s, unchanged rescans took 0.27–0.28 s, and mean warm intersections ranged from 0.67 to 2.08 ms. The largest sampled journal and scratch footprints were 4,128,272 bytes and 278,800 bytes. The sampler observes visible files at roughly 4 ms intervals. Reusable pages and logical versus physical bytes appear separately in the report.

These are small generated workloads. The kernel benchmark remains available for source-tree comparison.

A separate fixed-cost measurement of format 8 scanned 10,000 generated 4,096-byte C files, arranged in 200 components with 50 files per component and a shared `subsystem` directory within each component. With filesystem tags, compaction left 6,193,152 Ledger bytes for 40,960,000 source bytes, or **619 bytes per file** and **15.1%** of source size. The scan took 12.76 s. File rows and key maps used 292 bytes per file. Their absolute path text alone accounted for 141 raw bytes per file.

## Format 9 file storage

The published format 8 CLI and a format 9 release build indexed the same 10,000 generated C files on Windows. Each of 100 components contained a `subsystem` folder with 100 files, each 4,096 bytes. The runs used filesystem tags, with optional inference and the source-text cache disabled.

- The compacted Ledger fell from **5,660,672 to 4,112,384 bytes**, or **566.07 to 411.24 bytes per file**, a **27.4%** reduction. Both empty Ledgers used 372,736 bytes. The format 9 import itself added **373.96 bytes per file** after subtracting the empty Ledger. The shared directory dictionary occupied 32,768 bytes in the populated Ledger.
- One representative file row fell from **212 to 85 bytes**, including SQLite record and cell framing. The old row held a 115-byte absolute path. The new row used one byte for its directory key and twelve for its basename. The sample's other value bytes were 4 for the file ID, 4 for the logical parent, 16 for the native ID, 18 for the three timestamps, and 2 for size. Record and cell framing used 28 bytes.
- Inserting one more file into an already indexed directory used **543 bytes** within existing pages, compared with **696 bytes** in format 8. Format 9 used 83 for the file row, 38 for the key map and its two indexes, 82 for the four lookup entries, 31 for tiers, 9 for the shared tag-set entry, 14 for the metadata search document record, and 286 for changes to shared full-text postings and segment metadata. Shared posting growth varies with the new terms and current segments.
- A copied format 8 Ledger retained a selected file's ID, user tag, Paint, and live accented-content search after migration. The rebuilt Ledger recognized the same ID and Paint after its parent directory was moved. Extension, size, modification, name, and path filters returned the same result IDs on both formats. Mean query times, including CLI startup, ranged from 49 to 51 ms in format 8 and 43 to 48 ms in format 9 in the final run. The unchanged rescan took 2.56 s in format 8 and 3.10 s in format 9.

## Format 10 relative directory search

A Windows release build from format 9 commit `8e923ab` and a format 10 release build scanned identical generated trees. The dense fixture had 10,000 C files in 100 `component-XXX/subsystem` directories. The sparse fixture had 600 files with one file in each directory. Each file was 4,096 bytes. The source-text cache and inference were disabled.

- On the dense fixture the compacted database fell from **4,222,976 to 3,264,512 bytes**. The **958,464-byte** saving is **95.85 bytes per file** and **22.7%** of the old database. The metadata full-text indexes fell from **1,298,432 to 335,872 bytes**. In format 10, filename and Paint search used 319,488 bytes and 100 relative directory search documents used 16,384 bytes including pages and index metadata.
- On the sparse fixture the compacted database fell from **1,306,624 to 1,187,840 bytes**, a **118,784-byte** saving. Its metadata search indexes fell from **237,568 to 114,688 bytes**. An empty Ledger grew from 372,736 to 393,216 bytes with the extra search index and schema.
- An extra file in an already indexed directory used **79 search-index bytes** within existing pages in both format 10 fixtures. Format 9 used 335 bytes in the dense fixture and 334 bytes in the sparse fixture. The directory document count stayed the same after adding the file. Shared index growth varies with the new terms and any segment merges.
- A directory-term search returned the same ten files across both formats and took 59 ms before and 51 ms after on the dense fixture. A phrase spanning the directory and filename returned the same results and took 58 ms before and 77 ms after. CLI startup is included. The copied format 9 Ledger retained its original file ID, user tag, Paint, and live accented-content match after upgrade.

## Format 11 identity and tier storage

Windows release builds for formats 10 and 11 indexed the same 10,000 generated C files, each 4,096 bytes, in the same 100 directories. Both runs used filesystem tags and left the source-text cache and inference off.

- The compacted Ledger fell from **3,264,512 to 2,674,688 bytes**, or **326.45 to 267.47 bytes per file** including the empty Ledger. Excluding the empty Ledger, indexing added **287.13 before and 228.97 bytes per file after**. Computed tiers removed 315,392 bytes of file-tier growth. Integer identity consolidation removed another 266,240 bytes of file-row and mapping growth. The empty Ledger shrank by 8,192 bytes.
- Format 11 added **88.47 bytes/file** for file rows, **11.88** for the generated public-ID index, **85.61** for the existing file lookup indexes, **30.31** for metadata text search, **9.42** for the shared tag-set lookup, and **3.28** for shared directory paths. It added **zero** ordinary tier rows. Reading a tier did not add a row.
- Initial scan time went from **13.51 to 5.92 s**. The unchanged rescan went from **3.02 to 2.98 s**. One default tier inspection took **45 to 66 ms** including CLI startup. These timings describe the generated Windows fixture.
- A real format 10 backup kept the selected file's ID, user tag, Paint, and locked override through upgrade. It retained one explicit tier row. The tier fixture verifies a two-clue shortlist that a greedy first-clue choice alone would miss, and verifies a higher Individuality floor for a larger file.

## Format 12 live search and file identity

Run `python scripts/measure_lean_ledger.py <format-11-cli> <format-12-cli>` to compare release CLIs on the same disposable Windows fixture. It creates 10,000 regular C files of 4,096 bytes each, in 100 directories. The runs use default filesystem tags. The measured primary database excludes SQLite journals and the source files.

- The empty compacted database used **385,024 bytes** in format 11 and **327,680 bytes** in format 12. The populated databases used **2,666,496** and **1,576,960 bytes**, respectively.
- The empty format 12 Ledger occupied 80 SQLite pages of 4,096 bytes. Doctor attributed 69,632 bytes to optional intelligence, 36,864 to packed tags and the registry, 24,576 to file lookup indexes, 24,576 to audit and vocabulary history, 24,576 to object rows and directory and volume dictionaries, 8,192 to tiers, 8,192 to Paint and legacy ID lookups, and 131,072 to the schema and remaining tables and indexes. These pages hold table and index roots, schema definitions, and initial configuration. The breakdown had zero reusable or unattributed bytes.
- After subtracting the empty Ledger, indexing added **2,281,472 bytes** in format 11 and **1,249,280 bytes** in format 12. This is **228.15 to 124.93 added bytes per file**, a **45.2%** reduction.
- Format 12 growth consisted of **700,416 bytes** of object rows, **163,840** for exact-path lookup, **163,840** for native-ID lookup, **102,400** for physical-parent lookup, **94,208** for tag-set ownership, and **24,576** for the shared directory table and lookup. These total **1,249,280 bytes**. The native-volume table fit in its existing pages. Persistent text indexes, the generated public-ID index, the size index, and default tier rows added **zero**. Reusable and unattributed page growth were both zero after compaction.
- The scan took **5.92 to 4.00 s**. Median CLI wall-clock times across three calls were **27.62 to 27.53 ms** by ID, **30.43 to 29.38 ms** for an exact name, **31.02 to 30.98 ms** for a partial name, **126.78 to 122.65 ms** for content in one directory, and **3.95 to 3.51 s** for content across the full fixture.
- A populated format 11 backup kept the file's ID, direct tag, and locked Paint through restore, migration, rebuild, and compaction. The migrated Ledger found the current accented text and omitted the obsolete cached phrase.

## Preview retrieval evaluation

`cargo test --locked --release --test retrieval_quality -- --nocapture` evaluates 38 generated files and tracks tuned and held-out questions separately. The one tuned miss moved to rank one. On six held-out queries, top-1 accuracy, recall@3, and MRR each rose from **0.50 to 1.00**. Mean held-out latency rose from **1.65 to 2.06 ms**. The compacted database occupied **385,024 bytes**, or **10,132 bytes per file**, before and after within this small fixture's page allocation. The trial spent zero model tokens. A separate test rejects an improvement to the primary query when it demotes a known result in a control query.

The labeled cases include paraphrasing, reversed roles, dated evidence, misleading tags, overlapping records, and exact duplicates. A separate actor exercises the inaccessible source. The benchmark treats a correctly empty answer as a correct abstention.

A separate preview trial used SmolLM2 135M Instruct Q4_0 and All-MiniLM 22M in a disposable local Ollama store. On three generated files, a paraphrase that returned no result with inference disabled returned the expected source first by embedding similarity in 823 ms. Indexing used 1,705 reported tokens and the query used 519, including the classifier's attempted plan. The classifier produced zero accepted tags, so this trial measures a semantic retrieval gain only. The models and documents were processed locally.
