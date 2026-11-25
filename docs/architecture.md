# Architecture

The Rust core owns indexing, search, collection rules, storage analysis, and native file operations. The CLI calls it directly. The Tauri desktop uses the same core via asynchronous commands and renders React views. The authenticated loopback service offers a read-only subset. There is no Discord dependency.

## Ledger

SQLite uses foreign keys, WAL, transactional schema initialization, a bounded page cache, and an append-only audit table. The filesystem manages the actual files. Mosh stores a compact identifier, a searchable path, and the change stamp used to refresh tags and text. It stores Windows volume and file identity in a 16- or 24-byte binary record. A verified Mosh move preserves the Ledger identifier. An external rename preserves it when a unique live candidate has the same native identity and creation timestamp and its old path is gone. Hardlinks retain separate path records with a shared native identity.

The scan walks directories without following junctions or symlinks. The change stamp and deterministic tags enter a single write transaction. A temporary seen set reconciles missing paths. Ordinary discoveries and content edits update the index directly. Moves, missing paths, and Mosh actions can still leave history. Readers can use the previous snapshot while scanning. Cancellation rolls the transaction back. When a walk reports filesystem errors, the scan retains old records that may simply have become inaccessible. Watch mode batches Windows directory notifications, targets changed paths, and reconciles on event loss.

The metadata index has one FTS5 document per object's name and Paint. Since format 10, each directory containing indexed files has one relative path document. File matches join through the shared directory key. The relative path starts at the deepest registered root containing the directory. A folder without a directory entry can match its relative path directly. An explicit path lookup uses the recorded filesystem path. A phrase spanning a directory and basename matches a directory suffix and the start of a filename, using the same tokenizer on each. The parent index excludes registered child scopes. Previews open the current file on demand. Inspection and allocated-space queries read live filesystem metadata. MIME types are derived from extensions. Content searches use the optional byte-budgeted FTS5 cache and read uncached files with the same phrase tokenizer. Scans invalidate outdated cache entries.

## Packed tags

Format 11 assigns each object one durable integer primary key. The API generates its Ledger-local base-62 spelling from that key. Tagged objects refer to their packed set directly from the object row. Previously issued UUIDs remain usable through a compact alias table. Tag names and aliases resolve through the global tag dictionary.

Default tier assessments are computed from the file's indexed metadata and the current tier rules. The Ledger saves exceptions for inference and user activity. Effective tiers use the current authorized shortlist, so a new clue or changed neighbor can change eligibility without a cached score per file. The estimated skim uses eight candidates, 32 KiB total, and up to 8 KiB per candidate. The evaluator tests up to twelve confirmed clues. It checks useful pairs and uses up to eight greedy narrowing steps for longer combinations. Large files have higher thresholds.

## Storage floor and tradeoffs

NTFS already stores names, directory relationships, allocation, timestamps, and native IDs. A minimal supplemental row can be modeled as a small local ID and parent key, a 12–16 byte basename, a 16-byte native ID for rename detection, a small change stamp, and a packed tag reference. On this workload that is roughly 40–60 raw bytes before SQLite record and page overhead. That model assumes the application can obtain full paths and metadata from the filesystem and has no cross-directory search indexes, offline listings, tier scores, or history. It is a design estimate.

The previous Ledger copied the full path into its exact-path index. The current index stores an eight-byte key and still compares the full path before accepting a match, including in uniqueness checks. The directory index also drops the repeated logical ID. The Git index covers rows with Git metadata, and tier reads use their object-key index. Rebuildable name and path postings remain for phrase search. Routine scan activity and durable actions share an audit table, so the storage report attributes that table as one group and separately counts each event class.

Format 9 stores ordinary file locations as a directory key and basename. A shared dictionary keeps exact directory spellings. The `path` column reconstructs the full path for search, boundaries, and file operations. Keeping the dictionary independent of the folder object's current location means a moved folder cannot silently relocate its unscanned descendants in the Ledger. Directory rows and unusual legacy spellings can still store literal paths. Compaction reclaims unreferenced directory entries.

Kind uses an integer code and extension is derived from the basename. Allocation, access time, attributes, volume, filesystem kind, and hardlink count have no persistent columns and can be read by inspection. The scan retains native identity, creation time, logical size, modification time, and native change time for rename checks and revision invalidation. Extension and modification filters evaluate the derived extension or saved timestamp. Folder, exact-path, native-identity, and size indexes support frequent filesystem operations. Rebuildable name and path postings support metadata phrase search.

An arbitrary set of 1,000 members from a 2,048-tag vocabulary has about 2,041 bits of information, or 255 bytes, before origin and evidence. A dense forward bitmap for that range uses at most 256 membership bytes plus its header and any origin overrides. The reverse index adds another representation to support fast intersections. For 100 members from the same vocabulary the corresponding information bound is about 71 bytes, so the writer can keep smaller sparse encodings. Shared tag sets can amortize their forward representation across all files that reference them.

Identical tag sets share an immutable packed record. Mosh chooses the smallest representation among gap-encoded IDs, runs of consecutive IDs, and a bitmap over a dense range of IDs. Origin flags retain independent sources and can share a default value for the set. Confidence and evidence departures from the defaults occupy a sparse table.

Reverse lookups group 4,096 object keys at a time and choose the smallest delta list, run list, bitmap, or full-block marker. A bitmap can omit its leading and trailing zero bytes. Confirmed and suggested memberships have independent blocks. Intersections of direct tag filters combine blocks before enumerating objects. Inherited queries expand through physical parents.

Tag-set references and affected reverse blocks update in one SQLite transaction. Reverse blocks can be rebuilt from the tag sets. Bulk Cleave tagging writes one audit event and a packed list of object keys, which individual object histories can query.

## Relationships and permissions

A physical folder is a Coll. Virtual Colls and Marriages carry references without copying bytes. Edges represent logical parentage, Marriage references, or related entries. SQL reachability checks reject organizational cycles. A Marriage can also evaluate saved queries as dynamic references.

Direct owners and their managers have owner authority. Guest access comes from an explicit Coll grant. Secret Coll boundaries also cover physical descendants, and references retain the source's visibility check. The Windows desktop acts as the local administrator. A restricted identity can use the local API. Mosh permissions apply to the application and API, while Windows enforces direct disk access.

## Native writes

Cleave persists a preview and requires explicit confirmation for physical changes. Steps record intent and the original BLAKE3 digest before touching files. Windows moves use a file handle and a no-replacement rename. Copy stages a new file and compares its digest before installing it without replacing an existing destination. Verified moves reconnect the original Ledger identity. Undo checks the destination digest and uses the original name or a recovery folder. Filesystem and SQLite transactions can finish independently, so an interrupted plan can require manual recovery.

Exact duplicate analysis groups by size, samples buffered regions, computes full hashes for remaining candidates, and compares bytes when reporting a group. Disk usage counts known allocated storage once per native file identity. Reclaimable allocation is reported only when the necessary allocation and hardlink information is available.
