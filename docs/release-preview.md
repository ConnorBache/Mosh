# Mosh 0.2.0 preview 1

- Repair legacy path spellings during migration so live content search can read the current source file.
- Add Individuality scores and evidence-backed vocabulary review.
- Attach nested Moshes with separate discovery and search grants. Searching a child requires an explicit handoff.
- Evaluate feedback changes against confirmed results before committing them.
- Store a compact exact-path lookup key and report Ledger bytes by table and index.

Back up an existing Ledger before opening it with the preview. Existing installations can test the preview against a copied Ledger by setting `MOSH_DB` to the copy before starting the app. Prior UUID references still resolve. Run `mosh ledger compact` after migration to reclaim reusable pages.

Check Windows package hashes against the published `SHA256SUMS.txt`. The preview installer is unsigned and uses Microsoft Edge WebView2.
