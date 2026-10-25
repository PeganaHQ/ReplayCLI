# Contributing

This repo is the **verifier subset** of [PeganaHQ/Pegana](https://github.com/PeganaHQ/Pegana),
the open-source (MIT) Pegana repository. It is **mirrored automatically**: a sync pull request
copies `crates/methodology/`, `crates/pegana-replay-cli/`, `crates/common-verify/` and
`assets.toml` here. A change made only in this repo is overwritten by the next sync, so changes
land upstream first.

## What we accept

- **Issues**: open them on [PeganaHQ/Pegana](https://github.com/PeganaHQ/Pegana/issues).
  Issues filed here are read too, but the upstream tracker is where work is planned.

- **Methodology changes**: route through an ADR. Open an issue on PeganaHQ/Pegana describing
  the change; the ADR (rationale, alternatives considered, chosen design) is written under
  `docs/adr/` in that repo. Once accepted, the code change lands there and the next sync
  mirrors it into `crates/methodology/` here.

- **CLI ergonomics fixes** (replay-cli error messages, output formatting, exit-code
  handling): open a pull request against `crates/pegana-replay-cli/` in PeganaHQ/Pegana, or an
  issue describing the problem. Accepted changes reach this repo on the next sync.

- **Documentation polish** on this README or METHODOLOGY.md: these files live under
  `tools/pegana-replay-public/` in PeganaHQ/Pegana. Same workflow.

## What we do NOT accept

- Edits to `crates/methodology/` that change peg-risk math without an ADR. The methodology
  version is part of every receipt; silently changing it would break replay for any receipt
  produced under the old rules.

- Pull requests that only touch this mirror. Engine, dispatcher, indexer, API, MCP and web
  changes belong in PeganaHQ/Pegana, where that code lives.

## Versioning + receipts

The `methodology_version` field embedded in every audit receipt points at a
git tag in *this* repo of the form `methodology-vX.Y.Z`. If you're asked to
"verify against the methodology used to produce this receipt," check out the
matching tag — that's the canonical state at the time the engine emitted the
alert.

## Security

If you find a vulnerability that affects receipt validity or the on-chain commit pipeline,
please report it privately before any public disclosure: see [SECURITY.md](SECURITY.md)
(GitHub private vulnerability reporting, or email rafael@pegana.xyz).

## License

By contributing you agree your changes are released under MIT (matching
`LICENSE` at repo root).
