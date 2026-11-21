# bilibilive

The command line, native UI, MCP server and zuse host share `app::App`.
Keep operations and input schemas in `schema.rs`; never implement a second
network path in a frontend. Keep secrets in the operating-system credential
store, and keep diagnostics off MCP stdout.

Use the latest default-branch Git source for internal Project Z dependencies.
Do not put version selectors, tags or fixed revisions on those development
dependencies. Commit the observed lock and dependency receipt. Registry
publication uses the separate staging manifest produced by
`scripts/prepare-release.py`, with verified exact registry versions.

Validate locally. Formal release workflows are manual or tag-triggered.
Never start a real broadcast, send a real chat message, or initiate a PK just
to test. Use protocol fixtures and a private desktop for UI verification.
Store generated screenshots and binary packages outside Git; release assets
belong to GitHub Releases. Document real compatibility limits in
`docs/COMPATIBILITY.md`, with the audited StartLive revision.
