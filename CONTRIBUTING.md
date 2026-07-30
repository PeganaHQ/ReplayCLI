# Contributing

This repo is **mirrored automatically** from a closed sibling repo by a
path-filtered subtree-push action. Direct edits to `pegana-replay/main` are
overwritten on the next sync; please do not open PRs against it unless you've
talked to us first.

## What we accept

- **Issues**: open one on Telegram at [@PeganaWatchBot](https://t.me/PeganaWatchBot)
  rather than in this tracker. The Telegram channel routes to a human; the
  GitHub issue tracker on this repo is not actively monitored yet.

- **Methodology changes**: route through an ADR. Open the discussion on
  Telegram; the ADR itself is written in the private repo (ADRs are no longer
  mirrored here) with the rationale, alternatives considered, and chosen
  design. Once accepted, the code change lands there and the next sync mirrors
  it into `crates/methodology/`.

- **CLI ergonomics fixes** (replay-cli error messages, output formatting,
  exit-code handling): we'll happily review proposals on Telegram and land
  them via the private repo.

- **Documentation polish** on this README or the ADRs: same workflow — open
  on Telegram, we'll land it.

## What we do NOT accept

- Direct edits to `crates/methodology/` that change peg-risk math without an
  ADR. The methodology version is part of every receipt — silently changing
  it would break replay for any receipt produced under the old rules.

- Engine, dispatcher, indexer, API, MCP, or bot changes — none of those
  components live here. They're operational surface in the closed repo.

## Versioning + receipts

The `methodology_version` field embedded in every audit receipt points at a
git tag in *this* repo of the form `methodology-vX.Y.Z`. If you're asked to
"verify against the methodology used to produce this receipt," check out the
matching tag — that's the canonical state at the time the engine emitted the
alert.

## Security

If you find a vulnerability that affects receipt validity or the on-chain
commit pipeline, please report it privately via Telegram before any public
disclosure.

## License

By contributing you agree your changes are released under MIT (matching
`LICENSE` at repo root).
