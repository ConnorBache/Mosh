# Implementation checklist

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

## Intelligent Ledger

- [x] Migrate from Ledger formats 1 and 2 while carrying forward file identities, existing tags and aliases, Paint, permissions, and virtual relationships.
- [x] Compute target and effective tiers separately. Enforce configurable minimums of distinct confirmed direct tags, persist score components, and report unmet floors. Optional tiers can be configured through T10.
- [x] Configure model roles for Ollama, OpenAI, and compatible endpoints. Record usage and gate remote inference on Ledger budgets and source permissions.
- [x] Queue revision-scoped enrichment, retain evidence, enforce Paint locks, and invalidate source-derived tags after changes.
- [x] Track registry IDs, aliases, lifecycle decisions, and retrospective coverage. Expose operations through the CLI and Tag Registry.
- [x] Index scoped dialogue events with verifiable citations and keep compact file and passage vectors in SQLite.
- [x] Compile validated search branches to parameterized queries. Search with positive intersections, scoped relationships, cached text, name, Paint, and vector similarity where applicable.
- [x] Capture search feedback and replay a confirmed miss. Expose tier, model, agent, and search settings in desktop and CLI.
- [x] Measure the million-record migration and retrieval paths. Exercise real local embeddings and evaluate a small generation model.
- [ ] Verify real-model semantic tagging and planning quality across tiers. Verify adaptive depth, multi-perspective high-tier evaluation, and a larger annotated retrieval dataset.
- [ ] Add automatic Ollama installation, an OS credential-vault UI, and end-to-end OpenAI verification with real account credentials. Existing local models can already be detected and selected from Settings.
- [ ] Add a scalable approximate/partitioned vector index and compare alternatives. The current permission-filtered int8 similarity pass scans the configured model's eligible vectors. Add virtual-collection and Marriage embedding refresh on membership changes.
- [ ] Add independent fragment tiers, fragment Paint, and general-purpose adaptive fragmentation. Current promotion applies to existing cited passages and never changes a file.
- [ ] Add automatic follow-through and quality gating for more types of improvement. Current replay measures a known result's rank after evidence-based enrichment, but does not roll back an entire enrichment pass when rank stays unchanged.
- [ ] Expand background scheduling, advisor coverage, tag usefulness statistics, and automated retention. Current admission controls cover idle time, free memory, concurrent leases, retries, context, request timeout and API spending. Per-process CPU/GPU quotas and configurable schedules remain open.
- [ ] Gate production use of remotes on provider-specific billing verification. Local Windows permissions remain independent of Ledger permissions.

## Other deferred work

- [ ] Full-dataset dense AI enrichment measurements, parallel scan workers, and profiling across hardware.
- [ ] Explicit cross-volume moves, directory copy and move, Explorer drag and drop, recovery automation, and richer stream and attribute preservation.
- [ ] NTFS USN integration, version snapshots for non-Git files, PDF and image extractors, richer Git provenance, and portable metadata interchange beyond SQLite snapshots.
- [ ] Graph pan and zoom, disk treemaps, richer facets, organizational administration UI, and write-facing integration endpoints.
