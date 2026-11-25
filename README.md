# Mosh

Mosh is a local-first file explorer and searchable Ledger for Windows. It indexes existing directories in place and lets one file appear in several virtual collections. Filesystem operations, direct search, and deterministic indexing work offline.

## Install and start

Download the Windows x64 installer or CLI ZIP from [GitHub Releases](https://github.com/ConnorBache/Mosh/releases). The desktop application uses Microsoft Edge WebView2. The initial installer is unsigned. Compare its SHA-256 hash with the release checksum before running it.

The 0.2.0 preview uses Ledger format 7 with compact file identifiers, packed tags, nested Mosh boundaries, and intelligence features. Back up your Ledger before opening it with the newer build. See the [preview release notes](docs/release-preview.md).

1. Open **Mosh** and choose **Index folder**. Select an ordinary directory.
2. Browse the indexed folder. Double-click a folder to enter it or a file to open it with its Windows association. Select a row to inspect metadata, a text preview, tags, Paint, and available history.
3. Search using a filename fragment or a query such as `tag:source-code AND modified:<30d`. Open **Tags** to build a filter or **Collections** to create a virtual Coll or Marriage.
4. Use **Cleave** to preview a selection. Physical operations require confirmation. Quarantine and undo retain recoverable file bytes.

The desktop and CLI share the Ledger at `%LOCALAPPDATA%\Mosh\ledger.sqlite3`. The `MOSH_DB` environment variable selects another Ledger. This is useful for a disposable workspace.

## CLI

Extract `mosh.exe` and run it from PowerShell. Commands produce JSON.

~~~powershell
.\mosh.exe init
.\mosh.exe scan C:\Work
.\mosh.exe search 'tag:source-code AND NOT tag:archived'
.\mosh.exe tag add C:\Work\Notes.md planning
.\mosh.exe paint C:\Work\Notes.md --level minimal --body 'Planning decisions from this week.'
.\mosh.exe inspect C:\Work\Notes.md
.\mosh.exe watch C:\Work
.\mosh.exe cleave --query 'kind:file AND ext:log' --dry-run
.\mosh.exe doctor
~~~

For directory watching, keep the watch command running and use Ctrl+C to stop. Manual desktop rescans and the CLI scan reconcile missed changes. Use `mosh --help` and `mosh <command> --help` for command arguments.

## Information tiers and Ask Mosh

Files have separate importance, information, and hotness scores. The default direct-tag minimums are **1 for T1, 5 for T2, 10 for T3, and 100 for T4**. Change these under **Settings → Information tiers**. T5 through T10 are inactive until configured. Inherited and suggested tags do not count. A file keeps its valid tags when its effective tier falls below its target. Inspect a file to see the reason, override the target, or request reassessment.

**Settings → AI providers and agents** configures Ollama, OpenAI, or an OpenAI-compatible endpoint. Ollama defaults to loopback. Choose models by role or use **Use detected models** to fill empty roles from the installed model list. Agent background work is opt-in. To run a local Ollama server with cloud features disabled, set `OLLAMA_NO_CLOUD=1` before starting it. Remote providers read keys from the process environment and require explicit opt-in, pricing, spending limits, and a source Coll permission before reading source content.

Use **Agents** to queue indexing, run jobs, and inspect their status and model usage. Ask Mosh can return scoped citations, and you can point it to a relevant file if a search misses. It replays that search after indexing and reports whether the confirmed file moved up.

When a planned tag intersection misses, Ask Mosh retries with one fewer inferred tag at a time, starting with tags that entered the registry most recently. It retains positive tag filters you supplied explicitly and labels retry results as approximate. Each retry still needs a search constraint.

~~~powershell
.mosh.exe intelligence tier_settings
.mosh.exe intelligence model_status
.mosh.exe intelligence queue_index --json '{"query":"ext:txt","limit":20}'
.mosh.exe intelligence ai_run --json '{"limit":20}'
.mosh.exe ask --answer 'What did John assign Matt last week?'
~~~

## Search syntax

Queries support uppercase `AND`, `OR`, and `NOT`, standalone parentheses, and an implicit `AND` between expressions. `NOT` binds first, followed by `AND` and then `OR`. Use double quotes inside the expression for phrases and spaces.

- `design` searches source text, Paint, names, and paths, and matches partial filenames. Use `name-contains:design` to search names alone without reading file contents.
- `name:"Design Notes.md"`, `name-contains:design`, `path:archive`, `ext:md`, `kind:file`, and `virtual:true` filter entries.
- `tag:planning` searches direct and inherited tags. `direct-tag:planning` and `inherited-tag:planning` distinguish the two.
- `content:"release notes"` searches source text. `paint:handoff` searches Paint.
- `modified:<30d` means an age under 30 days. Relative times support `h`, `d`, and `w`. Absolute values use UTC `YYYY-MM-DD` or RFC3339. Use `modified:2026-01-01..2026-02-01` for an inclusive range.
- `size:>500MB`, `allocated:>=20MiB`, and `duplicate:true` filter by indexed size, allocation, or the last duplicate analysis.
- `repo:Mosh`, `branch:main`, `git-status:modified`, and `git-author:"Example Author"` filter Git metadata. Author search covers the scan's recent first-parent history.

## Ledger storage

The Ledger stores packed tag sets and reverse lookups. Identical tag sets share a single record. Search and previews read uncached file contents on demand, so broad content queries may take longer. Names, paths, and Paint stay searchable through the metadata index.

~~~powershell
.\mosh.exe ledger text-index --max-bytes 25000000
.\mosh.exe ledger text-status
.\mosh.exe ledger text-index --max-bytes 0
.\mosh.exe ledger compact
~~~

The optional content cache stores searchable terms and positions up to the requested number of SQLite page bytes. It invalidates entries after Mosh observes file changes. Run the same command to rebuild it or pass zero to clear it. `doctor` reports the database size, reusable pages, tag storage, and cache sizes. The write-ahead log and temporary SQLite work files can use additional disk space during writes.

Older Ledgers migrate on open. The migration keeps their tags, aliases, confidence, evidence, Paint, relationships, permissions, and history. Existing UUIDs resolve as aliases for compact IDs. Format 1's extracted-text cache is rebuilt on demand. Run `mosh ledger compact` to return reusable pages to the filesystem after an upgrade or cache clear. Inspect `mosh doctor` for per-table storage, reusable pages, and journal sizes.

## Individuality and vocabulary

The **Individuality** tab and `mosh individuality <file>` compare distinct confirmed direct tags against a bounded set of permitted neighbors. Lower values indicate closer matches. Exact duplicates found by verified duplicate analysis appear separately. Scores use the current tag index when requested. Review quoted evidence before approving a proposed assignment. Tag merge and deletion require `--confirm`.

Search feedback evaluates a trial against the confirmed miss and up to 12 other labeled searches. The trial commits when the primary rank improves and the evaluated controls reproduce their baselines without losing rank. Its report in **Agents** includes rejected trials, metrics, and the proposed tags.

## Nested Moshes

Index each child in its own Ledger. In the parent, attach that Ledger and its physical root. The parent can suggest permitted routing metadata. Searching a child requires a separate request and a grant issued in the child Ledger.

~~~powershell
mosh --db parent.sqlite3 scope info
mosh --db parent.sqlite3 scope attach child.sqlite3 C:\Work\Child 'Child project' --tag engineering
mosh --db child.sqlite3 scope allow PARENT_MOSH_ID local
mosh --db parent.sqlite3 scope route 'engineering'
mosh --db parent.sqlite3 scope handoff CHILD_MOSH_ID 'ext:rs'
~~~

The **Nested Moshes** page exposes the same actions. Other actors need a parent discovery grant and their own child handoff grant. A handoff result includes the child's qualified file ID. A moved or unavailable attachment retains its boundary until the registration is updated or detached.

## Safety and recovery

Scanning stores local paths, metadata, and Mosh annotations in SQLite. Treat the Ledger and its backups as sensitive. The application writes files only after a user action. Reparse points are indexed as records and their contents are skipped. Moves and quarantine operate on ordinary files on the same volume. Copy verifies the main data stream and preserves basic times and permissions where supported. Extended attributes and alternate streams can differ on copies.

The following workflow prepares a recoverable quarantine. Substitute the plan ID from the preview. The original parent and filename must still be available when restoring.

~~~powershell
.\mosh.exe cleave --dry-run --action quarantine --query 'kind:file AND ext:log'
.\mosh.exe cleave --execute PLAN-ID --confirm
.\mosh.exe cleave --status PLAN-ID
.\mosh.exe cleave --undo PLAN-ID --confirm
~~~

If an operation is interrupted, inspect `cleave --status PLAN-ID` and `doctor`. An intent state means a filesystem change may have occurred. Keep both paths and compare their contents with the journal digest before manual recovery. Quarantine files live in `.mosh-quarantine` under a registered root. Mosh excludes this directory from scans.

Use `mosh backup C:\Backups\mosh.sqlite3` for a consistent metadata snapshot. Use `mosh restore C:\Backups\mosh.sqlite3 C:\Recovered\mosh.sqlite3` to restore into a new Ledger. Check it with `mosh --db C:\Recovered\mosh.sqlite3 doctor`. `mosh ledger rebuild` regenerates the metadata search index and packed reverse tag lookups. Back up original files separately. A fresh scan can rediscover files, while annotations, virtual organization, identities, and previously observed activity require the Ledger backup.

## Local integration API

`mosh serve` exposes a read-only API on IPv4 loopback. Set `MOSH_API_TOKEN` to a random bearer token of at least 32 ASCII characters. Use `mosh serve --actor PERSON` for a restricted Mosh identity. The default `local` actor has access to the entire Ledger. Keep the token and the database local.

Send JSON via `POST /v1/search` with a body such as `{"query":"tag:planning","options":{"limit":25}}`. Additional commands are `roots`, `inspect`, `preview`, `history`, `tag_catalog`, `tag_coverage`, `graph`, and `saved`. Object endpoints accept a `reference` string with an indexed path or UUID. Audit history is available to the local administrator because it can contain previous locations. Coll permissions govern what Mosh serves. Windows permissions govern direct filesystem access.

## Build from source

Use a Windows x64 Developer PowerShell with Rust stable, Node.js 24, the Visual Studio C++ tools, Windows SDK, and WebView2. Git is needed for Git integration and its tests. See the [official Tauri Windows prerequisites](https://v2.tauri.app/start/prerequisites/#windows).

~~~powershell
cargo test --locked
cargo build --locked --release -p mosh
Set-Location desktop
npm ci
npm run tauri build -- --bundles nsis --ci
~~~

The CLI binary is `target/release/mosh.exe` and the installer is in `target/release/bundle/nsis`. Set `CARGO_TARGET_DIR` to an unsynced local directory if building from a synced folder, and make several gigabytes available for the desktop toolchain.

See [architecture](docs/architecture.md), [implementation status](docs/implementation.md), and [verification](docs/verification.md).
