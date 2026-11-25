# Architecture

The Rust core owns indexing, search, collection rules, storage analysis, and native file operations. The CLI calls it directly. The Tauri desktop uses the same core via asynchronous commands and renders React views. The authenticated loopback service offers a read-only subset. There is no Discord dependency.

## Ledger

SQLite uses foreign keys, WAL, transactional schema initialization, a bounded page cache, and an append-only audit table. A physical path, logical UUID, native volume and file ID, and creation timestamp serve different purposes. A verified Mosh move preserves the UUID. An external rename keeps it only when a unique live candidate has the same native identity and creation timestamp and its old path is gone. Hardlinks retain separate path records with a shared native identity.

The scan walks directories without following junctions or symlinks. Metadata and deterministic tags enter a single write transaction. Readers can use the previous snapshot while scanning. Cancellation rolls the transaction back. When a walk reports filesystem errors, the scan retains old records that may simply have become inaccessible. Watch mode batches Windows directory notifications, targets changed paths, and reconciles on event loss.

FTS5 indexes names, paths, extracted content, and Paint. Structured Boolean expressions bind their values to indexed SQL predicates. Tags use a global integer dictionary. The assignment table is keyed by object, tag ID, and origin, with an inverse index for tag lookups. The engine computes inherited tags from physical parents as needed. Alias mappings point at the same integer IDs.

## Relationships and permissions

A physical folder is a Coll. Virtual Colls and Marriages carry references without copying bytes. Edges represent logical parentage, Marriage references, or related entries. SQL reachability checks reject organizational cycles. A Marriage can also evaluate saved queries as dynamic references.

Direct owners and their managers have owner authority. Guest access comes from an explicit Coll grant. Secret Coll boundaries also cover physical descendants, and references retain the source's visibility check. The Windows desktop acts as the local administrator. A restricted identity can use the local API. Mosh permissions apply to the application and API, while Windows enforces direct disk access.

## Native writes

Cleave persists a preview and requires explicit confirmation for physical changes. Steps record intent and the original BLAKE3 digest before touching files. Windows moves use a file handle and a no-replacement rename. Copy stages a new file and compares its digest before installing it without replacing an existing destination. Verified moves reconnect the original Ledger identity. Undo checks the destination digest and uses the original name or a recovery folder. Filesystem and SQLite transactions can finish independently, so an interrupted plan can require manual recovery.

Exact duplicate analysis groups by size, samples buffered regions, computes full hashes for remaining candidates, and compares bytes when reporting a group. Disk usage counts known allocated storage once per native file identity. Reclaimable allocation is reported only when the necessary allocation and hardlink information is available.
