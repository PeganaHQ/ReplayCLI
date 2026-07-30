<h1 align="center">ReplayCLI</h1>

<p align="center">
  <strong>Don't trust the peg alert. Re-derive it.</strong><br>
  The open math and offline verifier behind every <a href="https://www.pegana.xyz">Pegana</a> peg-risk receipt.
</p>

<p align="center">
  <a href="https://crates.io/crates/pegana-replay"><img alt="crates.io" src="https://img.shields.io/crates/v/pegana-replay.svg?logo=rust"></a>
  <a href="https://github.com/PeganaHQ/ReplayCLI/actions/workflows/release.yml"><img alt="Release" src="https://github.com/PeganaHQ/ReplayCLI/actions/workflows/release.yml/badge.svg"></a>
  <a href="https://scorecard.dev/viewer/?uri=github.com/PeganaHQ/ReplayCLI"><img alt="OpenSSF Scorecard" src="https://api.scorecard.dev/projects/github.com/PeganaHQ/ReplayCLI/badge"></a>
  <a href="LICENSE"><img alt="License: MIT" src="https://img.shields.io/badge/license-MIT-blue.svg"></a>
  <img alt="MSRV" src="https://img.shields.io/badge/rustc-1.90+-orange.svg">
</p>

---

[pegana.xyz](https://www.pegana.xyz) is a peg-risk oracle for Solana. It watches ~70 stablecoins,
LSTs, yield-bearing wrappers, CDPs and synthetic-leverage tokens (live count:
[`/v1/stats`](https://api.pegana.xyz/v1/stats)) and publishes a state transition the moment an
asset leaves `PEGGED`.

Every alert ships a **content-addressed receipt**. This repo is how anyone outside Pegana proves
that receipt is real — without asking Pegana's servers to vouch for themselves.

## Why this repo exists

An oracle that says "trust me" is not an oracle. So Pegana publishes the *same* pure functions its
production engine runs, and a CLI that replays any receipt against them offline.

Verification is two independent layers, and the CLI runs **both by default**:

| Layer | What it proves | Defeats |
|---|---|---|
| **1. Canonical re-hash** | The receipt's fields are internally self-consistent — recompute its SHA-256 and compare to the published hash. | Silent edits to a receipt's numbers. |
| **2. On-chain anchor** | That exact hash is anchored on Solana in an SPL Memo signed by a **pinned** Pegana ops wallet. | Swapping in a *different*, equally self-consistent receipt after the fact. |

`--offline` skips layer 2. That's a real reduction in guarantee, not a shortcut — see
[What PASS actually means](#what-pass-actually-means).

## Quick start

**Install** — pre-built, Sigstore-attested binary (linux-x86_64, darwin-aarch64):

```sh
curl --proto '=https' --tlsv1.2 -LsSf \
  https://releases.pegana.xyz/pegana-replay-installer.sh | sh
```

<details>
<summary>Other install methods</summary>

```sh
# From crates.io (any platform with a Rust 1.90+ toolchain)
cargo install pegana-replay --locked

# From source — Intel Mac and Windows build this way
cargo install --git https://github.com/PeganaHQ/ReplayCLI pegana-replay --locked
```

`--locked` matters: without it Cargo re-resolves the dependency graph and you are no longer
building the tree this repo's `Cargo.lock` pins.
</details>

**Verify an alert** — grab any alert id from [pegana.xyz](https://www.pegana.xyz) or the API:

```sh
pegana-replay --alert-id 0190ab12-3456-7890-abcd-ef0123456789
# → PASS  receipt_sha256 matches (on-chain anchor + signer verified)
```

```sh
# Hash-only, no RPC — CI and air-gapped hosts
pegana-replay --alert-id <UUID> --offline

# A saved bundle, fully offline
pegana-replay --bundle ./replay-bundle.json

# One machine-readable line on stdout; exit code unchanged
pegana-replay --alert-id <UUID> --json
# {"status":"PASS","exit_code":0,"asset":"jitoSOL","previous_state":"Pegged",
#  "final_state":"Pegged","methodology_version":"0.7.0","receipt_sha256":"…","alert_id":"…"}
```

**No install?** Open `https://www.pegana.xyz/audit/<alert-id>` — the in-browser `ReceiptVerifier`
recomputes the canonical hash in pure JS, byte-identical to this CLI.

## Exit codes

Stable contract — safe to branch on in CI.

| Code | Status | Meaning |
|:--:|---|---|
| `0` | `PASS` | Hash matches. With `--alert-id` and no `--offline`, the on-chain anchor + pinned signer matched too. |
| `1` | `FAIL` | Receipt SHA-256 mismatch, or schema-v2 re-derivation mismatch. **Tamper or corruption.** |
| `2` | `ERROR` | Network failure, malformed bundle, unknown alert, or bad CLI usage. |
| `3` | `VERSION_MISMATCH` | The receipt's `methodology_version` ≠ this binary's embedded version. Install the matching build. |
| `4` | `ONCHAIN_MISMATCH` | The on-chain check ran and **failed** — wrong/absent signer, no memo, memo content mismatch, or anchoring permanently failed. |
| `5` | `ONCHAIN_INCOMPLETE` | The on-chain check could not be completed — RPC unreachable, anchor still inside its 24h commit window, or commit was disabled for the emitting deployment. **Absent, not tampered.** |

Note `4` vs `5`: a severe transition with no anchor reports `ONCHAIN_INCOMPLETE`, never a false `PASS`.

## What PASS actually means

- **Default (`--alert-id`, no `--offline`)** — the recomputed `receipt_sha256` equals the published
  one **and** that hash is anchored on-chain under a pinned Pegana signer. The verdict and its
  inputs were not altered after the fact.
- **`--offline` / `--bundle`** — only layer 1 passed. The receipt is internally self-consistent, but
  an attacker controlling the API could in principle have served a *different* self-consistent
  receipt. Layer 2 is what rules that out.
- **Schema-v2 receipts** — the CLI also re-derives the verdict from the receipt's **own frozen
  inputs**. Never against fresh oracle data; replay is historical, not live.

## What's in this repo

| Path | What it is |
|---|---|
| `crates/methodology/` | Pure, zero-I/O functions: discount, EWMA, hysteresis, thresholds, transitions, receipt hashing. **The exact code the production engine runs.** |
| `crates/pegana-replay-cli/` | The `pegana-replay` binary. Fetches a receipt, re-hashes it, re-derives the verdict from frozen inputs, cross-checks the on-chain SPL Memo anchor. |
| `crates/common-verify/` | Config types + the `AlertEvidence` schema returned by `/v1/audit/:id`. |
| `assets.toml` | Canonical asset registry — symbol, mint, class, peg target, threshold bands, intrinsic + market strategies. Hashed into every receipt. |
| `METHODOLOGY.md` | Long-form prose companion to `crates/methodology`. The code is authoritative; this explains it. |
| `.github/workflows/release.yml` | `cargo-dist` pipeline. Publishes attested binaries on every `v*` tag, signed via GitHub-issued OIDC (no key material). |

**Not here:** the engine, dispatcher, indexer, API, MCP server, Telegram bot, on-chain commit
pipeline and webhook signing live in a closed sibling repo. Closed for operational reasons — but
**every decision they produce is reproducible from this repo** against the alert's receipt.

## Verifying the binary itself

Release binaries carry Sigstore build provenance issued by this repo's CI:

```sh
gh attestation verify ~/.pegana/bin/pegana-replay --repo PeganaHQ/ReplayCLI
```

> **Binaries tagged `v0.4.4` and earlier** were built before this repo moved to the `PeganaHQ` org
> and are attested to the old repository identity. Verify those with
> `--repo lrafasouza/pegana-replay`, or upgrade — `v0.5.0+` attests to `PeganaHQ/ReplayCLI`.

## Versioning

The CLI **embeds exactly one methodology version**, and its release tag is **decoupled** from it —
CLI `v0.4.1` embedded methodology `0.4.0`, because the CLI can ship fixes without a methodology bump.

What matters for replay is the **embedded methodology version**, not the CLI tag. A receipt carries
the `methodology_version` it was produced under; to read an old receipt, install the CLI build that
embeds *that* methodology and replay against it **as it was at the time**. The CLI enforces this — a
mismatch exits `3` with a hint, rather than silently replaying under today's rules.

Each `v*` tag builds `x86_64-unknown-linux-gnu` and `aarch64-apple-darwin`. Intel Mac and Windows
build from source.

## Security

Reporting a vulnerability: see [SECURITY.md](SECURITY.md). Please report privately before public
disclosure — a bug here could let a tampered receipt verify as clean.

## Contributing

This repo is a **one-way mirror**, automatically generated from a closed sibling repo and
force-pushed. Direct commits are overwritten on the next sync. See
[CONTRIBUTING.md](CONTRIBUTING.md) for how to actually get a change landed, and
[CODE_OF_CONDUCT.md](CODE_OF_CONDUCT.md) for the ground rules.

Disagreeing with a verdict is not a complaint — it's the intended use. Bring the alert id and
the replay that disagrees.

## Independent review

[GeckoVision](https://www.npmjs.com/package/@geckovision/gecko) reviewed Pegana's public OpenAPI
surface across three rounds with their own OpenAPI-to-agent tooling. Their linter graded the spec
**A / 96**, and two of their findings shipped as fixes that are live in production today. Not an
endorsement — an outside pair of eyes, credited because independent review is the whole point of
this repo.

## License

MIT — see [LICENSE](LICENSE).

## Links

- **Site & live audit ledger** — [www.pegana.xyz](https://www.pegana.xyz)
- **Install mirror** — [releases.pegana.xyz](https://releases.pegana.xyz)
- **API** — [api.pegana.xyz](https://api.pegana.xyz) · [OpenAPI](https://api.pegana.xyz/openapi.json)
- **Telegram** — [@PeganaWatchBot](https://t.me/PeganaWatchBot)
- **X** — [@PeganaHQ](https://x.com/PeganaHQ)
