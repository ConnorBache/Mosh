# Mosh

Mosh is a local-first file explorer and searchable Ledger for Windows. It indexes existing directories in place and lets one file appear in several virtual collections. Filesystem operations, direct search, and deterministic indexing work offline.

## Install and start

Download the Windows x64 installer or CLI ZIP from [GitHub Releases](https://github.com/ConnorBache/Mosh/releases). The desktop application uses Microsoft Edge WebView2. The initial installer is unsigned. Compare its SHA-256 hash with the release checksum before running it.

The published 0.2.0 preview uses Ledger format 8. Builds from this source tree use format 14 and upgrade earlier Ledgers on open. Back up your Ledger before upgrading. After upgrade, run `mosh ledger compact` to reclaim reusable pages. See the [preview release notes](docs/release-preview.md).

1. Open **Mosh**, choose **Build Mosh**, and select a directory. Mosh builds a separate local Ledger for that directory and leaves the source files in place.
2. Browse the indexed folder. Double-click a folder to enter it or a file to open it with its Windows association. Select a row to inspect metadata, a text preview, tags, Paint, and available history.
3. Search using a filename fragment or a query such as `tag:source-code AND modified:<30d`. Open **Tags** to build a filter or **Collections** to create a virtual Coll or Marriage.
4. Use **Cleave** to preview a selection. Physical operations require confirmation. Quarantine and undo retain recoverable file bytes.

Use the **Active Mosh** selector to switch between built directories and the **Default Ledger**. The active Mosh reconciles filesystem changes while open and checks for changes when reopened. Renames preserve file identity when the filesystem provides a matching native ID and creation time. Deleted entries disappear from active views, and their annotations and history stay in the Ledger.

The default desktop and CLI Ledger lives at `%LOCALAPPDATA%\Mosh\ledger.sqlite3`. The `MOSH_DB` environment variable selects a different default Ledger. Moshes built in the wizard live beside it in the `ledger.moshes` folder. Use `mosh --db <path>` to address one of those Ledgers directly.

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

Files have separate importance, information, and hotness scores. These guide the target tier. The effective tier depends on Individuality. The default floors are **0 for T1, 60 for T2, 80 for T3, and 90 for T4**. Files of at least 1, 16, and 128 MiB require an extra 10, 20, or 30 points respectively, capped at 100. Change the base floors under **Settings → Information tiers**. T5 through T10 are inactive until configured. Inspect a file to see the reason, override the target, or request reassessment. Defaults are computed on demand. Model assessments, overrides, locks, and access activity are saved.

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
- `size:>500MB` and `allocated:>=20MiB` read current filesystem metadata. `duplicate:true` uses the last duplicate analysis.
- `repo:Mosh`, `branch:main`, `git-status:modified`, and `git-author:"Example Author"` filter Git metadata. Author search covers the scan's recent first-parent history.

## Ledger storage

Each indexed file has an integer key, a filename and shared directory reference, and a filesystem file ID qualified by a shared volume reference when available. A creation time guards identity where the OS provides it. An eight-byte change stamp detects edits between scans. Mosh reads current size and timestamps from the filesystem. On Windows, a lookup by Mosh ID can recover a renamed file using its native ID when the new location is in scope and permitted. Paths support portable lookup and distinguish hard links.

Tracked files share their physical parent through the directory record. One small directory index serves both child listing and exact-name matching. Exact lookup verifies the full filename after checking its two-byte fingerprint.

The Ledger stores packed tag sets and reverse tag postings. Each shared set stores a count of its assignments. Search checks stored filenames, relative paths, and Paint directly. It reads source text on demand after applying scope and access checks. Broad content queries may take longer than scoped queries. Existing `indexed-content:` expressions resolve to live content search.

Older Ledgers migrate on open and retain their tags, aliases, confidence, evidence, Paint, relationships, permissions, and history. Existing UUIDs resolve as aliases for compact IDs. Migration drops rebuildable text indexes. Run `mosh ledger compact` to reclaim those pages. `mosh doctor` reports per-table page usage and reusable space. The write-ahead log and temporary SQLite work files can use additional disk space during writes.

## Individuality and vocabulary

The **Individuality** tab and `mosh individuality <file>` use confirmed direct tags as clues. The score estimates how many clues narrow the viewer's permitted scope to at most eight files and 32 KiB of estimated skim input. The estimate caps each file's skim at 8 KiB. A scope that already fits, or a shortlist found in one clue, gets 100. Each additional clue costs ten points. If the search cannot reach the budget with the tested clues, it scores zero. Nearest matches and verified duplicates remain visible as separate diagnostics. Review quoted evidence before approving a proposed assignment. Tag merge and deletion require `--confirm`.

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

Use `mosh backup C:\Backups\mosh.sqlite3` for a consistent metadata snapshot. Use `mosh restore C:\Backups\mosh.sqlite3 C:\Recovered\mosh.sqlite3` to restore into a new Ledger. Check it with `mosh --db C:\Recovered\mosh.sqlite3 doctor`. `mosh ledger rebuild` regenerates packed reverse tag lookups. Back up original files separately. A fresh scan can rediscover files, while annotations, virtual organization, identities, and previously observed activity require the Ledger backup.

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
