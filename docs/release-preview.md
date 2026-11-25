# Mosh 0.2.0 preview 1

- Repair legacy path spellings during migration so live content search can read the current source file.
- Add Individuality scores and evidence-backed vocabulary review.
- Attach nested Moshes with separate discovery and search grants. Searching a child requires an explicit handoff.
- Evaluate feedback changes against confirmed results before committing them.
- Store a compact exact-path lookup key, pack dense tag memberships into bitmaps, and report Ledger bytes by table and index.

Back up an existing Ledger before opening it with the preview. Existing installations can test the preview against a copied Ledger by setting `MOSH_DB` to the copy before starting the app. Prior UUID references still resolve. Run `mosh ledger compact` after migration to reclaim reusable pages.

Check Windows package hashes against the published `SHA256SUMS.txt`. The preview installer is unsigned and uses Microsoft Edge WebView2.

## Ledger format 12 in source builds

- Use integer identity for Ledger references and retain prior object IDs as aliases where needed.
- Share native volume identity across files. Read current size and timestamps from the filesystem and keep a compact change stamp for scan reconciliation.
- Search names, relative paths, and Paint directly and read source text on demand. Migration removes persistent text search indexes, including the optional extracted-text index.
