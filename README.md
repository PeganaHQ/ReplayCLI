# pegana-replay

Open math + CLI behind every Pegana alert.

[pegana.xyz](https://www.pegana.xyz) is a peg-risk oracle for Solana. It watches
~27 LSTs, stablecoins, yield-bearing wrappers, CDPs, and synthetic-leverage
tokens, and publishes a state transition the moment an asset moves out of
PEGGED. Every alert ships with a **content-addressed receipt** — this repo is
how anyone outside Pegana can verify one. Verification is two layers, and the
CLI runs both by default: (1) recompute the receipt's canonical SHA-256 and
compare it to the published hash — proves the receipt's fields are internally
self-consistent; (2) confirm that exact hash is anchored on Solana in an SPL
Memo signed by one of Pegana's pinned ops wallets — proves the receipt wasn't
swapped for a different, equally self-consistent one after the fact. Layer 2
is what makes the tamper-evidence claim hold; skip it with `--offline` and
you're trusting layer 1 alone.

## Verify a Pegana alert in 60 seconds

**Option A — install the CLI (crates.io):**
```sh
cargo install pegana-replay
pegana-replay --alert-id <UUID>     # an alert id from pegana.xyz or the API
# → PASS  receipt_sha256 matches (on-chain anchor + signer verified when present)
```
By default this re-derives the receipt hash and, when the receipt carries an
on-chain anchor (severe transitions), also verifies that anchor + its signer
(RPC required). A severe receipt with no anchor reports ONCHAIN_INCOMPLETE, not
a false PASS. For a hash-only check — CI, air-gapped hosts, or you just want it
fast — add
`--offline`, or verify a saved bundle entirely offline:
`pegana-replay --bundle <path-to-replay-bundle.json>`. For CI or scripting, add
`--json` for a single machine-readable verdict line on stdout (`status` +
`exit_code` + the receipt's frozen `asset` / `final_state` /
`methodology_version` / `receipt_sha256`); the exit code is unchanged. An
interactive run also colors PASS green / FAIL red (honors `NO_COLOR` and stays
plain when piped).

**Option B — no install, verify in your browser:**
Open the audit page for any alert — `https://www.pegana.xyz/audit/<alert-id>` — where the in-browser
verifier (`ReceiptVerifier`) recomputes the canonical receipt hash in pure JS (byte-identical to the
CLI), trusting nothing from our servers.

**What PASS means:** with the default on-chain check, PASS means the recomputed `receipt_sha256`
equals the published one AND that hash is anchored on-chain under a pinned Pegana signer — together,
the verdict and its inputs were not altered after the fact. With `--offline` (or `--bundle`, which is
always offline), PASS means only the hash check passed: the receipt is internally self-consistent, but
an attacker controlling the API could in principle have swapped in a different self-consistent receipt
— the on-chain check is what rules that out. For schema-v2 receipts the CLI also re-derives the verdict
from the frozen inputs (not just the hash).

## What's in this repo

| Path | What it is |
|---|---|
| `crates/methodology/` | Pure functions: spread, EWMA, hysteresis, transition. The exact code the engine runs in production. |
| `crates/pegana-replay-cli/` | The CLI binary: fetches a receipt, re-hashes it against the published hash, re-derives the verdict from the receipt's OWN frozen inputs (schema-v2 only — never against fresh oracle data), and by default cross-checks the on-chain SPL Memo anchor. Reports PASS / FAIL / ERROR / VERSION_MISMATCH / ONCHAIN_MISMATCH / ONCHAIN_INCOMPLETE. |
| `crates/common-verify/` | Config types + the AlertEvidence schema returned by `/v1/audit/:id`. |
| `assets.toml` | Canonical asset list. Symbol, mint, class, peg target, threshold bands, intrinsic + market strategies. Hashed into every receipt. |
| `docs/adr/` | Architecture Decision Records — every load-bearing methodology choice is dated and rationalised here. |
| `.github/workflows/release.yml` | `cargo-dist` pipeline that publishes signed binaries to GitHub Releases on every `v*` tag, with Sigstore attestations via GitHub-issued OIDC. |

## What's NOT in this repo

The engine, dispatcher, indexer, API server, MCP server, Telegram bot,
on-chain commit pipeline, and webhook signing primitives live in a closed
sibling repo (`lrafasouza/pegana`, private). Closed for operational reasons —
**every decision they produce, however, is reproducible from this repo**
against the alert's audit receipt URL.

This repo is automatically mirrored from the private one by a path-filtered
subtree-push action; do not file PRs against it directly (see CONTRIBUTING).

## Verify a specific alert end-to-end

```sh
# 1. Install the verifier (no Rust toolchain needed — curl|sh ships a
#    pre-built, Sigstore-attested binary for linux-x86_64 and
#    darwin-aarch64. Intel Mac and Windows build from source via
#    `cargo install --git`.)
curl --proto '=https' --tlsv1.2 -LsSf \
  https://releases.pegana.xyz/pegana-replay-installer.sh | sh

# 2. Fetch the bundle and verify BOTH layers (default as of v0.5.0):
#    re-hash the receipt's frozen inputs + recorded verdict, THEN confirm
#    that hash is anchored on-chain in an SPL Memo signed by a pinned
#    Pegana ops wallet. Needs a working Solana RPC (override --solana-rpc).
#    Does NOT re-execute the methodology.
pegana-replay --alert-id 0190ab12-3456-7890-abcd-ef0123456789

# 3. Hash-only, no RPC call (CI, air-gapped hosts):
pegana-replay --alert-id 0190ab12-3456-7890-abcd-ef0123456789 --offline

# 3b. Machine-readable verdict for CI / scripting — one JSON line to stdout,
#     exit code unchanged; combine with --offline for a no-RPC check:
pegana-replay --alert-id 0190ab12-3456-7890-abcd-ef0123456789 --json
# → {"status":"PASS","exit_code":0,"asset":"jitoSOL","previous_state":"Pegged",
#    "final_state":"Pegged","methodology_version":"0.7.0","receipt_sha256":"…","alert_id":"…"}

# 4. Verify the binary's provenance via the Sigstore attestation issued
#    by this repo's CI (linux-x86_64 / darwin-aarch64 release binaries).
gh attestation verify ~/.pegana/bin/pegana-replay \
  --repo PeganaHQ/ReplayCLI
```

## Versioning

- The CLI **embeds exactly one methodology version**, and its release tag is
  **decoupled** from it: e.g. CLI `v0.4.1` embeds methodology `0.4.0` (the CLI
  can ship fixes without a methodology bump). What matters for replay is the
  **embedded methodology version**, not the CLI tag. A receipt carries the
  `methodology_version` it was produced under, so anyone reading an old receipt
  installs the CLI build that embeds *that* methodology and replays against it
  *as it was at the time*, not as it is today. (The CLI enforces this: a receipt
  whose `methodology_version` differs from the binary's **embedded** methodology
  version exits `3` VERSION_MISMATCH with a hint to install the matching build.)
- Each `v*` tag builds two Sigstore-attested targets — `x86_64-unknown-linux-gnu`
  and `aarch64-apple-darwin`. Intel Mac and Windows build from source.

## License

MIT. See `LICENSE`.

## Links

- Site & live audit ledger: [www.pegana.xyz](https://www.pegana.xyz)
- Self-hosted install mirror: [releases.pegana.xyz](https://releases.pegana.xyz)
- Telegram for issues / feedback: [@PeganaWatchBot](https://t.me/PeganaWatchBot)
- Twitter / X: [@PeganaHQ](https://x.com/PeganaHQ)
