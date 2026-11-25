# Implementation checklist

The initial build is the stopping point for the product clarification.

## Initial scope

- [x] Create the public GitHub repository.
- [x] Build a shared Rust and SQLite core and CLI with no model dependency.
- [x] Index ordinary directories and read native Windows file IDs, allocation, timestamps, and access-control descriptors.
- [x] Exercise stable IDs, incremental notification processing, tag persistence and inheritance, selective Paint, and FTS search.
- [x] Exercise virtual Colls, Marriage references, cycle checks, and Secret Coll visibility.
- [x] Exercise Git status, author filtering, history, and diffs against a fixture repository.
- [x] Exercise duplicate detection, hardlink accounting, verified file plans, and reversible quarantine.
- [x] Exercise Ledger integrity, rebuild, and backup restoration.
- [x] Smoke-test the versioned read API with a restricted actor.
- [x] Verify the 10,000-file fixture and record measured results.
- [x] Smoke-test the packaged Windows desktop and publish the installer and CLI.

## Deferred from the full brief

- [ ] AI providers, janitors, job scheduling, suggestions, semantic search, and grounded search chat.
- [ ] Million-record and dense-tag scale validation, parallel scan workers, and performance profiling.
- [ ] Explicit cross-volume moves, directory copy and move, Explorer drag and drop, recovery automation, and richer stream and attribute preservation.
- [ ] NTFS USN integration, version snapshots for non-Git files, PDF and image extractors, richer Git provenance, and portable metadata interchange beyond SQLite snapshots.
- [ ] Graph pan and zoom, disk treemaps, richer facets, organizational administration UI, and write-facing integration endpoints.
