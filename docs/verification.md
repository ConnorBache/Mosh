# Verification

Mosh uses generated test directories and a dedicated temporary database. The repository contains synthetic fixture definitions only.

Run `cargo test --locked` to execute the Windows core integration suite. It exercises native file metadata, rename and identity tracking, text search, contextual tags, virtual references, permissions, Git, real Windows directory watching, exact duplicates, hardlinks, backup and restore, and confirmation and undo for physical plans.

The [Windows build workflow](https://github.com/ConnorBache/Mosh/actions/workflows/windows.yml) builds the installer and CLI and exercises the installed WebView2 interface. The smoke test covers directory browsing, metadata inspection, Paint search, Tag Explorer, Marriage references, duplicate analysis, Cleave preview and undo, and History.
