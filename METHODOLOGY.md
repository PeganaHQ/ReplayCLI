# Pegana methodology — v0.4.0

How peg-risk signals are produced, why this design over the obvious
alternatives, and what every load-bearing piece of the math is doing.

This document is the long-form version of what the
`crates/methodology` Rust code does. The two should never drift —
if they do, file an issue (the code is authoritative; this doc is
the explanation).

---

## TL;DR

For each monitored asset Pegana computes a single number at every
recompute tick:

```
spread = market_price − intrinsic_value
```

where:

- **intrinsic_value** is what the asset is *supposed to be worth*
  according to a source the issuer or protocol controls — an oracle
  feed, an on-chain redemption rate, a stability-pool exchange rate,
  a CR/LP NAV.
- **market_price** is what the asset is *actually trading for* in
  the deepest executable venue right now — Jupiter for SPL routes,
  a Pyth spot feed where one exists, DexScreener as a fallback.

That spread is then classified into one of five peg states
(`PEGGED`, `DRIFT`, `DEPEG`, `CRITICAL`, `BLACK_SWAN`) using
per-asset-class thresholds, EWMA smoothing, and hysteresis — see
[State machine](#state-machine) below.

Every state transition produces a **content-addressed receipt**:
a hash over the canonical inputs the methodology saw, the values
it computed, and the methodology version that produced the decision.
Anyone can replay that receipt offline using the open-source
`pegana-replay` binary in this repo — see the [Verifiability](#verifiability)
section. State transitions also commit the receipt's sha256 to Solana
mainnet via SPL Memo, so the timing of the decision is independently
auditable from chain history.

---

## Why oracle/intrinsic + executable market quote = spread

The two obvious alternatives are:

### "Just track the price"

Most peg-monitoring tools watch a single price feed and trigger when
it deviates from the peg target by some bps. This is wrong for
~half of Pegana's universe:

- **Liquid Staking Tokens** (jitoSOL, dzSOL, vSOL): the *peg target*
  is the SOL exchange rate of the underlying stake pool, not 1 SOL.
  As staking rewards accrue, jitoSOL legitimately grows above 1 SOL
  and shouldn't trigger an alert. You need the actual `intrinsic`
  computed from on-chain validator state.
- **Yield-bearing stables** (USDY, sUSD, syrupUSDC, sUSDe):
  per-share NAV is published by the issuer via a separate oracle
  (often a Pyth `.RR` feed). The market price *will* drift around
  NAV depending on liquidity; the alert is "market << intrinsic"
  (redemption-stress signal), not "deviation from $1".
- **Synthetic leverage** (xSOL via Hylo): leverage means the
  intrinsic is `(collateral_sol − hyusd_supply_in_sol) / xsol_supply`,
  which moves with the underlying. Treating it as a $1 peg makes
  no sense.

### "Just track the oracle"

The other naive approach is to trust the issuer-controlled feed
exclusively. This fails differently:

- **Issuer feeds lag execution** — Maple publishes a daily NAV for
  syrupUSDC. If liquidity disappears intraday, the spot market
  reflects it within seconds; the feed lags 6–24 hours.
- **Issuer feeds can be wrong on purpose or by accident**. Several
  high-profile depeg incidents started with an oracle feed continuing
  to publish stale-but-confident values during a panic. The
  redundancy of having an *executable* market quote on the other
  side is what makes peg-stress visible early.

So Pegana takes both, takes the difference, and watches the
difference. When intrinsic and market agree, the spread is ~0 and
the asset is `PEGGED`. When they disagree, the spread quantifies
*how much* and *in which direction* — `intrinsic > market` is a
discount (redemption stress, illiquidity, panic); `market > intrinsic`
is a premium (squeeze, new demand, oracle lag).

The closed-form math is in `crates/methodology/src/discount.rs`.
The output is two `Decimal` values (`discount`, `bps`) per asset
per tick.

---

## State machine

Five states, transitions driven by `(discount_bps, current_state)`:

```
                  ╭──> DRIFT ──> DEPEG ──> CRITICAL ──> BLACK_SWAN
                  │     │         │           │             │
   PEGGED ◀───────┴─────┴─────────┴───────────┴─────────────╯
                       (hysteresis-gated downgrades)
```

Thresholds are per-asset (calibrated in `assets.toml`) because a 50bps
move is normal for `stable_fiat` but a screaming emergency for
`stable_cdp`. The table shows representative values; per-asset entries in
`assets.toml` are the authoritative source:

| Class | DRIFT | DEPEG | CRITICAL | BLACK_SWAN | Source |
|---|---|---|---|---|---|
| stable_fiat (USDC, USDT, PYUSD…) | ~15–20bps | ~50bps | ~200bps | ~400bps† | `assets.toml` per asset |
| stable_cdp (hyUSD) | CR<130% | CR<115% | CR<105% | CR<100% | `assets.toml` `thresholds_cr` |
| lst (jitoSOL, dzSOL…)* | ~40–140bps | ~100–280bps | ~250–560bps | ~500bps+† | same |
| stable_yield (USDY, sUSD, syrupUSDC, sUSDe…) | discount-only* | ~30bps | ~100bps | ~200bps† | same |
| synth_lev (xSOL) | ~100bps | ~300bps | ~1000bps | ~2000bps† | same |

† `black_swan` defaults to `2 × critical` when not overridden in `assets.toml`
(v0.4.0, ADR-0025). Per-asset overrides via `black_swan` / `black_swan_bps` /
`cr_black_swan` keys.

\* For yield-bearing stables **and LSTs** only the *discount* side
triggers — i.e., `market < intrinsic`. A premium (`market > intrinsic`)
normalizes to `PEGGED`. For a yield-bearing stable a premium is thin
secondary liquidity; for an LST it's demand pressure, not stress — the
risk signal is the *discount* (redemption stress, cf. stETH −7% in 2022,
ezETH's depeg). `is_direction_sensitive` covers both classes (ADR-0021).
Discount-side classification is unchanged.

**NAV-premium sanity gate (v0.4.0, ADR-0025).** The premium→PEGGED
carve-out is correct for real demand pressure but can silently mask a
*broken intrinsic anchor* — e.g. sHYUSD: market ≈ +30% above a
thin-Jupiter NAV print, yet PEGGED. If the premium exceeds
`NAV_PREMIUM_SANITY_BPS = 1000` (10%) on a direction-sensitive class the
engine concludes the NAV anchor is broken and forces the state to
**UNKNOWN** instead. Legitimate premiums never approach this bound (the
widest observed across all assets is INF ≈ 160 bps); the discount side
and symmetric classes are untouched. Code:
`crates/methodology/src/thresholds.rs` — `premium_sanity_violated` /
`NAV_PREMIUM_SANITY_BPS`.

### BLACK_SWAN

The fifth state, `BLACK_SWAN`, is reached when the spread exceeds a
threshold beyond every normal operating band. As of v0.4.0 (ADR-0025) it
is reachable via **both** classification paths:

- **Spread path (bps).** `state_for_bps_discount` has a `black_swan` band
  that defaults to `2 × critical` when not specified in `assets.toml`. A
  stable_fiat asset at ≥ 400 bps discount (e.g. USDC trading at $0.96),
  or a UST-style cascade already in CRITICAL, enters BLACK_SWAN. Before
  v0.4.0 the bps path topped at CRITICAL; only the CR path could reach
  BLACK_SWAN, leaving the advertised five-state machine unreachable for
  25/26 assets.
- **CR path (CDP stables).** `state_for_cr` enters BLACK_SWAN when CR
  drops below `cr_black_swan` (default 100%).

**BLACK_SWAN is not a terminal state.** It auto-exits via normal
hysteresis like every other band — the engine does NOT enforce an
operator-reset or "never-auto-exits" behavior. No such reset path exists
to make a permanent terminal state safe (a false or recovered black_swan
would otherwise be stuck forever). The CR-path BLACK_SWAN (hyUSD) already
behaved this way before v0.4.0; the spread-path BLACK_SWAN follows the
same rule. Any prior marketing copy implying terminal behavior was
incorrect and has been corrected per ADR-0025. Code:
`crates/methodology/src/thresholds.rs` — `state_for_bps_discount` (spread
path) and `state_for_cr` (CR path).

### EWMA smoothing

Raw spread ticks are noisy — a single 100ms Jupiter quote can spike
because of a thin route or a stale fill. The methodology runs an
**exponentially-weighted moving average** with `α = 0.3` over the
last several recompute cycles before applying thresholds. That cuts
~80% of single-tick spikes without measurably delaying real
transitions. Code: `crates/methodology/src/ewma.rs` — `apply_ewma_pure`
(formula: `α × raw + (1 − α) × prev`; seeds at `raw` on cold-start when
no prior EWMA exists).

### Hysteresis

Two independent mechanisms keep the state stable when the EWMA hovers
near a threshold.

**Time-hysteresis (asymmetric transitions).** Promoting an asset to a
*worse* state (toward `BLACK_SWAN`) happens immediately when the EWMA
crosses the threshold; *demoting* requires the EWMA to fall ≥30% below
the threshold for ≥3 consecutive ticks.

**Magnitude-hysteresis — spread path (Schmitt-trigger deadband, ADR-0021).**
Time-hysteresis alone could not stop oscillation *at* the boundary, so
`classify_with_hysteresis` was added: escalation still uses the normal
threshold, but a relaxation toward a looser state only commits once the
smoothed discount falls below `threshold × (1 − deadband_pct)` (engine
default `DEADBAND_PCT = 25`). Example: JupSOL flapped `DRIFT`↔`PEGGED`
around 60 bps (84.7 → 48.8 bps); the deadband (exit at 45 bps) holds
the state through the dead zone. Code:
`crates/methodology/src/thresholds.rs` — `classify_with_hysteresis`.

**Magnitude-hysteresis — CR path (Schmitt-trigger deadband, ADR-0023).**
CDP assets (hyUSD) are classified via the collateral-ratio path, not bps.
For CR, *lower* is worse, so the exit band is inverted: relaxation toward
a looser state only commits once CR rises above
`threshold × (1 + CR_DEADBAND_PCT)` (engine default `CR_DEADBAND_PCT = 2`,
i.e. a 2% band). The DRIFT threshold for hyUSD is **130%** (CR < 130% →
DRIFT); this is intentional — hyUSD rests at ≈ 133% (σ ≈ 2.5 pp) and
`DRIFT at CR < 130` is a deliberate risk-level choice, not a bug. The
deadband reduced calibration-window flapping from 52 transitions to ~10
without changing the threshold. Code:
`crates/methodology/src/thresholds.rs` — `classify_cr_with_hysteresis`.

The time-hysteresis constants (0.30 retreat margin, 3-tick floor) and the
deadband constants (25% spread, 2% CR) are checked by proptest in the
crate's test suite.

---

## Verifiability

Every state transition produces an **`AlertEvidence`** record (see
`crates/common-verify/src/lib.rs`) with the following load-bearing
fields:

```rust
pub struct AlertEvidence {
    pub alert_id: Uuid,
    pub methodology_version: String,    // semver of pegana-methodology
    pub methodology_git_sha: Option<String>,
    pub assets_toml_sha256: String,     // canonical hash of assets.toml
    pub inputs_frozen: serde_json::Value,
    pub computed: serde_json::Value,
    pub receipt_sha256: String,         // sha256 over a canonical encoding
    pub onchain_tx_sig: Option<String>, // SPL Memo commit signature
    pub commit_status: String,
    pub created_at: DateTime<Utc>,
}
```

`inputs_frozen` is *every* input the methodology saw at the moment
of decision: timestamp, oracle price, market quote, prior EWMA
state, asset config snapshot. `computed` is what the methodology
output: discount, state, classification. `receipt_sha256` is sha256
over a canonicalized JSON encoding of `(inputs_frozen, computed,
methodology_version, methodology_git_sha, assets_toml_sha256)`.

### Replay

```sh
pegana-replay --alert-id <UUID>
```

The CLI:

1. Fetches `AlertEvidence` from `https://api.pegana.xyz/v1/audit/<UUID>`.
2. Reads `methodology_version` from the receipt.
3. Loads the matching version of `pegana-methodology` (this repo
   pins by git tag — the release tag equals the methodology version it
   embeds, e.g. `v0.4.0` ships methodology 0.4.0).
4. Re-hashes the receipt's frozen inputs + recorded verdict using
   `canonical_receipt_hash` and compares to the on-chain-anchored
   canonical hash; checks the anchoring transaction was signed by a
   pinned Pegana ops key.
5. Exits 0 (PASS) iff both conditions hold: the hash matches and
   (unless `--offline`) the on-chain anchor is signed by an accepted
   Pegana commit wallet. On-chain verification is on by default;
   `--offline` runs the hash check alone. Add `--json` for a single
   machine-readable verdict line on stdout — the exit code is unchanged.

Note: the CLI does NOT re-execute the methodology or re-derive the
verdict from raw market data — that is ADR-0019's documented design.
It provides tamper-evidence: anyone can confirm the published history
wasn't altered.

The replay binary doesn't need network access for the math — only to
fetch the receipt and (unless `--offline`) confirm the SPL Memo
commitment on-chain.

### On-chain commitment

For receipts where `onchain_tx_sig` is non-null, the engine
committed `receipt_sha256` to Solana mainnet via the SPL Memo
program. Anyone can look up the transaction and confirm:

1. The Memo program's `Memo` instruction data equals `receipt_sha256`.
2. The transaction's signer is the engine's ops wallet
   (`7PpoyumFQMmcWzhJxDYr6iPv1fjYN41KBTA8xKKzu7R9`).
3. The block_time on chain happens AFTER the engine claims to have
   decided.

Together these prevent backdating: the engine can't claim it
decided 2 hours ago about an event that just happened, because the
Memo commit would have a block_time later than the receipt's
`detected_at`. Code: `crates/methodology/src/canonical.rs` (the
hash function) + the engine's `onchain_commit.rs` in the closed
sibling repo.

Note: the anti-backdating enforcement (`block_time > detected_at` check)
runs inside the closed sibling repo's engine and is therefore not
verifiable from this public repo. The hash itself — which you can
reproduce from this repo's `canonical_receipt_hash` — is what you verify;
the block_time ordering is confirmable directly from the Solana explorer
for any `onchain_tx_sig`.

### Retraction protocol

If a methodology version is later found to be broken (a bug in
threshold logic, a numerical edge case, an asset-class misapplied),
the engine **refuses to run** on the broken version. Old receipts
remain queryable and replayable — replay against the buggy version
will produce the buggy output, which is forensically useful. The
audit page renders a warning banner on receipts from a retracted
version.

See [`docs/adr/0003-methodology-retraction-protocol.md`](docs/adr/0003-methodology-retraction-protocol.md)
for the full state machine. Three lifecycle states:

- **active** — currently emitting receipts
- **deprecated** — superseded by a newer version, receipts still
  valid and replayable
- **broken** — bug confirmed, engine refuses to start, old receipts
  preserved with a warning banner

---

## What the methodology is NOT

A few things this design explicitly does not do, with reasons:

- **No latency promises before v1**. The calibration window
  (2026-05-29 → 2026-06-10) exists specifically so we can measure
  p95/p99 alert latency under real load before promising any
  number. See ADR-0001 / ADR-0015. Anyone shipping a competing
  methodology should run their own measurement; ours will be
  published as a calibration report.
- **No ML, no AI, no anomaly detection on top of the spread**. The
  methodology is intentionally a pure function of explicit inputs
  with checked-in thresholds. This is what makes replay tractable
  and what makes the receipt math meaningful. Anomaly layers can
  always be added on top by consumers; they shouldn't be baked into
  the methodology because they break determinism.
- **No black-box oracle fusion**. Where two sources exist (e.g.,
  Pyth NAV + secondary spot for USDY), the methodology chooses one
  per direction explicitly — issuer-NAV for `intrinsic`, market
  spot for `market`. Mixing/weighting them was considered and
  rejected; see ADR-0009 for the discussion.
- **No SOC 2 compliance, no regulatory audit, no financial advice**.
  Pegana publishes a *verifiable computation log*: anyone can prove
  which methodology produced which alert given which inputs. That's
  it. Use the receipts to verify our claims; don't substitute them
  for your own judgment.

---

## Source

| Path | What it is |
|---|---|
| [`crates/methodology/`](crates/methodology) | Pure functions implementing this document. Versioned by semver; the CLI release tag equals the methodology version it embeds (e.g. `v0.4.0`), so a receipt's `methodology_version` maps directly to a release tag. |
| [`crates/pegana-replay-cli/`](crates/pegana-replay-cli) | The CLI binary that runs the methodology against a fetched receipt. |
| [`crates/common-verify/`](crates/common-verify) | Config types + `AlertEvidence` schema returned by the API. |
| [`assets.toml`](assets.toml) | Canonical asset list. Hashed into every receipt. |
| [`docs/adr/`](docs/adr) | ADRs that decided each load-bearing methodology choice, dated and rationalised. |

The engine that runs this methodology in production (the closed
sibling repo) is **not** in scope for this document. What that engine
must do is produce, for each state transition, an `AlertEvidence`
that this methodology can verify. As long as that happens, the
engine is interchangeable — anyone could write their own
implementation, hash their receipts the same way, and consumers
could replay both indistinguishably.

That's the point of separating the math from the runtime.

---

## Links

- Site: <https://www.pegana.xyz>
- Audit ledger (live receipts post 2026-06-11 verdict gate):
  <https://www.pegana.xyz/audit>
- Install verifier: <https://releases.pegana.xyz>
- Telegram for questions: [@PeganaWatchBot](https://t.me/PeganaWatchBot)
