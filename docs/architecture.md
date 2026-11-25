# Architecture

The Rust core owns indexing, search, collection rules, storage analysis, and native file operations. The CLI calls it directly. The Tauri desktop uses the same core via asynchronous commands and renders React views. The authenticated loopback service offers a read-only subset. There is no Discord dependency.

## Ledger

SQLite uses foreign keys, WAL, transactional schema initialization, a bounded page cache, and an append-only audit table. The filesystem manages the actual files. Mosh stores a compact identifier, a searchable path, and the change stamp used to refresh tags and text. It stores Windows volume and file identity in a 16- or 24-byte binary record. A verified Mosh move preserves the Ledger identifier. An external rename preserves it when a unique live candidate has the same native identity and creation timestamp and its old path is gone. Hardlinks retain separate path records with a shared native identity.

The scan walks directories without following junctions or symlinks. The change stamp and deterministic tags enter a single write transaction. A temporary seen set reconciles missing paths. Ordinary discoveries and content edits update the index directly. Moves, missing paths, and Mosh actions can still leave history. Readers can use the previous snapshot while scanning. Cancellation rolls the transaction back. When a walk reports filesystem errors, the scan retains old records that may simply have become inaccessible. Watch mode batches Windows directory notifications, targets changed paths, and reconciles on event loss.

The Ledger indexes names, paths, and Paint without duplicating their text into the FTS5 tables. Previews open the current file on demand. Inspection and allocated-space queries read live filesystem metadata. MIME types are derived from extensions. Content searches use the optional byte-budgeted FTS5 cache and read uncached files with the same phrase tokenizer. Scans invalidate outdated cache entries.

## Packed tags

The object registry assigns durable integer keys for packed lookups. The API uses a short, Ledger-local base-62 spelling of the key. Previously issued UUIDs remain usable through a compact alias table. Tag names and aliases resolve through the global tag dictionary.

## Storage floor and tradeoffs

NTFS already stores names, directory relationships, allocation, timestamps, and native IDs. A minimal supplemental row can be modeled as a small local ID and parent key, a 12–16 byte basename, a 16-byte native ID for rename detection, a small change stamp, and a packed tag reference. On this workload that is roughly 40–60 raw bytes before SQLite record and page overhead. That model assumes the application can obtain full paths and metadata from the filesystem and has no cross-directory search indexes, offline listings, tier scores, or history. It is a design estimate.

The previous Ledger copied the full path into its exact-path index. The current index stores an eight-byte key and still compares the full path before accepting a match, including in uniqueness checks. The directory index also drops the repeated logical ID. The Git index covers rows with Git metadata, and tier reads use their object-key index. Rebuildable name and path postings remain for phrase search. Routine scan activity and durable actions share an audit table, so the storage report attributes that table as one group and separately counts each event class.

The file record still contains one full path. Reconstructing it from directory keys would need a coordinated migration of filesystem operations, boundary checks, and path filters. Dropping name and path postings would remove indexed phrase search. These are the largest remaining fixed-cost candidates. The preview retains those behaviors and reports their measured cost.

Identical tag sets share an immutable packed record. Mosh chooses among gap-encoded IDs and runs of consecutive IDs. Origin flags retain independent sources and can share a default value for the set. Confidence and evidence departures from the defaults occupy a sparse table.

Reverse lookups group 4,096 object keys at a time and choose the smallest delta list, run list, 512-byte bitmap, or full-block marker. Confirmed and suggested memberships have independent blocks. Intersections of direct tag filters combine blocks before enumerating objects. Inherited queries expand through physical parents.

Tag-set references and affected reverse blocks update in one SQLite transaction. Reverse blocks can be rebuilt from the tag sets. Bulk Cleave tagging writes one audit event and a packed list of object keys, which individual object histories can query.

## Relationships and permissions

A physical folder is a Coll. Virtual Colls and Marriages carry references without copying bytes. Edges represent logical parentage, Marriage references, or related entries. SQL reachability checks reject organizational cycles. A Marriage can also evaluate saved queries as dynamic references.

Direct owners and their managers have owner authority. Guest access comes from an explicit Coll grant. Secret Coll boundaries also cover physical descendants, and references retain the source's visibility check. The Windows desktop acts as the local administrator. A restricted identity can use the local API. Mosh permissions apply to the application and API, while Windows enforces direct disk access.

## Native writes

Cleave persists a preview and requires explicit confirmation for physical changes. Steps record intent and the original BLAKE3 digest before touching files. Windows moves use a file handle and a no-replacement rename. Copy stages a new file and compares its digest before installing it without replacing an existing destination. Verified moves reconnect the original Ledger identity. Undo checks the destination digest and uses the original name or a recovery folder. Filesystem and SQLite transactions can finish independently, so an interrupted plan can require manual recovery.

Exact duplicate analysis groups by size, samples buffered regions, computes full hashes for remaining candidates, and compares bytes when reporting a group. Disk usage counts known allocated storage once per native file identity. Reclaimable allocation is reported only when the necessary allocation and hardlink information is available.
