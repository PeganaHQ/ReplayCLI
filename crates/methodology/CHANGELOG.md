# pegana-methodology changelog

## 0.8.0 — 2026-10-24

MINOR. Adds, all default-OFF at the engine and byte-exact for existing receipts:

- `InputsFrozen.market_unavailable` + `OverrideReason::MarketUnavailable`: the
  dark-market gate (ADR-0046). `false` is omitted from the canonical hash.
  CR-classified assets are exempt (`market_unavailable_applies`). The override
  functions gain a `market_unavailable: bool` parameter.
- `InputsFrozen.premium_unconfirmed` + `confirm_premium_candidate` (ADR-0047):
  applied to the candidate before the time hysteresis, an unconfirmed premium
  caps a symmetric class at DRIFT and makes a direction-sensitive class keep a
  committed DEPEG-or-worse band. `false` is
  omitted from the hash; default OFF at the engine.
- `RECEIPT_EWMA_ALPHA` / `RECEIPT_CONFIRM_UP_SECS` / `RECEIPT_DECAY_DOWN_SECS`:
  the values the verifier binds receipts to. Changing them requires a bump.
- `validate_thresholds`, `uses_sol_path`; `state_for_cr` now saturates a CR above
  `u32::MAX` percent to the healthiest band instead of wrapping.

Release order: every receipt the engine emits carries methodology 0.8.0 whatever
the flags, so release a verifier embedding 0.8.0 (`pegana-replay` 0.6.0) before
or together with the engine deploy.

Version note: `intrinsic_stale` (ADR-0038) is also a hashed input that shipped in
0.7.0 without a bump; a published 0.5.2 verifier cannot hash a receipt carrying
`intrinsic_stale = true`. The bump to 0.8.0 makes it report VERSION_MISMATCH.

Unflagged correctness fix (engine, not a methodology function): spread
classification is now seeded from the FSM state the receipt freezes
(`last_state`) instead of `last_published_state`. They are identical in normal
operation; they differ only after a persistence failure or inside the BLACK_SWAN
maturing carve-out, where the old seed made an honest receipt fail re-derivation
(audit finding F07).

## 0.7.0 — 2026-07-16

Behavior change (MINOR). Pyth confidence-gate —
`pyth_confidence_gate_violated` + `PYTH_CONFIDENCE_GATE_BPS = 500`. A third,
independent override alongside `premium_sanity_violated`/
`discount_sanity_violated`: when a cached Pyth reading an asset depends on
(intrinsic and/or market leg) has a confidence/price ratio wider than 500 bps
(5%), the verdict is forced to honest-dark UNKNOWN rather than trusting a
possibly-wrong intrinsic/market leg. Conservative-only — can only push a
verdict *toward* UNKNOWN, never away from it. Class-agnostic (unlike the
direction-sensitive-gated premium/discount sanity checks): a wide confidence
interval taints trust in the reading regardless of asset class.

`oracle_snapshots.confidence` has been captured and persisted since day one
(no indexer/schema change needed); every receipt already freezes the exact
Pyth entries an asset depended on (`InputsFrozen.pyth_entries`, schema v2), so
this gate is byte-exact replayable offline with no new frozen field. Today's
existing `pyth_confidence_label` (`high`/`medium`/`low`/`unknown`) stays
display-only and unchanged — this is a separate, verdict-affecting function.

`PYTH_CONFIDENCE_GATE_BPS = 500` is a conservative STARTING default (5x the
existing "low" display bucket's own 100bps cut point), not a Pyth-mandated
number — Pyth's own best-practices docs frame confidence-interval usage as
application-dependent, not a fixed ratio. Structurally inert on the
2025-10-10 backtest (the runner's `pyth_entries` stays `HashMap::new()` by
construction — zero captured Pyth data in that fixture), so the golden
replay diff is limited to `methodology_version` + `expected_receipt_sha256`.
Calibration against the live `oracle_snapshots` universe (Query A/B) is the
founder's call before/soon after deploy. See ADR-0032.

## 0.6.1 — 2026-07-08

Bug fix (PATCH — no API signature change, unlike 0.6.0). Follow-up to 0.6.0:
`apply_ewma_pure`'s overflow guard protects the EWMA arithmetic itself, but
its output (`discount_smooth`) has no bound on magnitude beyond "didn't
overflow that specific computation." Three downstream consumers —
`premium_sanity_violated`, `discount_sanity_violated`, and
`state_for_bps_discount` — all did unchecked `discount.abs() * 10_000` (bps
scaling), so an untrusted `rederive()` input crafted to produce a
large-but-non-overflowing `discount_smooth` (a moderately large out-of-range
`alpha` is enough) still panicked one step later. Found via a systematic
grep for the same `.abs() * Decimal::from(10_000...)` pattern across the
crate after shipping 0.6.0 — same bug class, three more sites.

`premium_sanity_violated`/`discount_sanity_violated` now treat an
overflowing magnitude as violated (`true`) — consistent with their purpose:
an implausible discount forces honest-dark UNKNOWN rather than a false
verdict. `state_for_bps_discount` saturates to BlackSwan (not the 0/Pegged a
naive parse-failure default would give) — unlike a CR ratio where larger is
healthier, a larger discount is always worse, so overflow must map to the
most alarming band. Zero verdict change on the 2025-10-10 backtest golden
(123 receipts, re-baselined, no diff beyond version+hash) — unreachable in
the live engine for the same reason as 0.6.0.

## 0.6.0 — 2026-07-08

API change (MINOR), not a verdict change. `apply_ewma_pure` and `state_for_cr`
used unchecked `Decimal` arithmetic, which `rust_decimal` panics on overflow
rather than erroring — same bug class as the 0.4.0-era `discount.rs` fix,
never applied to these two functions. `apply_ewma_pure` now returns
`Option<Decimal>` (breaking signature change, hence MINOR); `state_for_cr`
saturates to the healthiest band on overflow instead of panicking (signature
unchanged). Unreachable in the live engine by construction — `discount_raw`
is always `|d| < 1` before reaching `apply_ewma_pure`, and by induction so is
every prior EWMA output, so the overflow can only be triggered by out-of-range
`alpha`/`ewma_prev`/`hyusd_cr` in an untrusted receipt fed to `rederive`
(i.e. via `pegana-replay`'s public verify CLI, not the engine's own pipeline).
Every legitimate, in-range input produces the exact same result as before —
only pathological inputs that previously crashed the process now return
`None`/saturate instead. No existing receipt's recorded verdict changes.

## 0.5.0 — 2026-07-01

Behavior change (MINOR). Intrinsic-sanity guard —
`discount_sanity_violated` + `NAV_DISCOUNT_SANITY_BPS = 3000`. Mirror of the
0.4.0 NAV premium-sanity on the DISCOUNT side: a garbage-HIGH intrinsic (>30%
discount on a direction-sensitive class) is a broken NAV anchor, published as
honest-dark UNKNOWN instead of a false CRITICAL/BLACK_SWAN (JLP 2026-06-24: RR
feed printed 21817 vs 3.41). Verdict-affecting only for >30%-discount garbage
prints; provably inert on the live universe (max real direction-sensitive
discount = 245 bps) and on the 2025-10-10 backtest. Not ADR-0024 (single-sample
magnitude, not constancy). See ADR-0030.

## 0.4.0 — 2026-06-15

Behavior change (MINOR) — the "validation follow-ups" cluster (ADR-0025).
Additive publish-time honesty guards; no existing band thresholds change.

- **NAV-sanity premium cap** — new `premium_sanity_violated(class, discount)` +
  `NAV_PREMIUM_SANITY_BPS = 1000`. A premium (negative discount) beyond 10% on a
  direction-sensitive class (LST, stable_yield) is not demand pressure — it means
  the NAV/intrinsic anchor is broken. The ADR-0021 premium→PEGGED carve-out would
  otherwise mask it as a confident PEGGED off a garbage anchor (sHYUSD: market
  ≈ +30% over a thin-Jupiter NAV print). The engine now publishes honest-dark
  UNKNOWN instead. Magnitude check on one smoothed sample — explicitly NOT the
  discount-constancy freeze detector rejected in ADR-0024. >6× margin over the
  widest legitimate premium observed across the 26 assets (INF ≈160 bps).
- **Confidence gate** — an asset with NO Pyth feed anywhere (neither a
  `pyth_spot`/`jupiter_usd`-numeraire market nor a `pyth_*` intrinsic) has no
  independent oracle cross-check, so it can no longer assert `high` confidence:
  the engine's `pyth_confidence_for_asset` caps it at `medium`. Blast radius
  today is exactly **sUSD** (DexScreener + custom rate); every other
  active asset has a Pyth dependency and is unchanged. Answers the validation
  grill "why does sUSD carry HIGH off a thin single source?". (Display-only —
  confidence is not a band threshold; no state-classification or calibration
  impact.)
- **BLACK_SWAN reachable from spread** — `state_for_bps_discount` gains a
  black_swan band, default `2 × critical` (overridable via `black_swan` /
  `black_swan_bps`). Previously the bps path topped out at CRITICAL and only the
  CR path (hyUSD) could reach BLACK_SWAN, so the publicly-advertised 5th state
  was unreachable for 25/26 assets — the validation "Four/Five states" honesty
  gap. Now a >2×-critical move (a USDC 4%+ break, a UST-style CRITICAL→BLACK_SWAN
  cascade) is labelled terminal-grade. Re-labels only the most extreme moves
  (already CRITICAL today); nothing at/below critical changes. It auto-exits via
  normal hysteresis — the engine does NOT enforce a "never auto-exits /
  operator-reset" terminal state (no reset path exists to make that safe), and
  the CR-path black_swan already behaved this way. See ADR-0025.

## 0.3.0 — 2026-06-04

Behavior change (MINOR). Activated immediately under the ADR-0009 **critical-bug
exception** ("wrong alerts firing"): during the calibration window hyUSD flapped
PEGGED↔DRIFT ~52× in two days as its collateral ratio oscillated around the
130% drift band (oracle jitter on the SOL-priced reserves clipping the
threshold). The CR classification path carried neither the EWMA smoothing nor
the magnitude deadband the spread path already had (ADR-0021).

- **CR magnitude hysteresis (Schmitt-trigger deadband)** — new
  `classify_cr_with_hysteresis`. The CR analog of `classify_with_hysteresis`
  with the band inverted (for a collateral ratio, a LOWER value is worse). A
  worsening CR escalates at the normal threshold; a relaxation toward a looser
  state only commits once CR rises above `threshold × (1 + deadband_pct)`
  (engine default 2%). **Why:** time-hysteresis (`transition.rs`
  confirm_up/decay_down) alone could not stop sustained oscillation *at* the
  boundary. Measured on the calibration window, the deadband cut hyUSD's
  transitions ~80% (52 → ~10) while keeping escalation immediate.

Receipt schema: v1 (unchanged). Verdicts change for some valid hyUSD inputs,
hence the MINOR bump; replay stays deterministic via the recorded
`methodology_git_sha`. See ADR-0023. v0.2.0 → `deprecated` (still valid for
replay), superseded by 0.3.0.

## 0.2.0 — 2026-06-01

Behavior change (MINOR). Activated immediately under the ADR-0009 **critical-bug
exception** ("wrong alerts firing"): v0.1.0's symmetric classification was
emitting two classes of false alerts during the calibration window —
DRIFT↔PEGGED *flapping* when a smoothed discount sat near a threshold, and a
🚨 DRIFT on a benign LST *premium* (market above redemption value).

- **Magnitude hysteresis (Schmitt-trigger deadband)** — new
  `classify_with_hysteresis`. Escalation still uses the normal threshold, but a
  relaxation toward a looser state only commits once the smoothed discount falls
  below `threshold × (1 − deadband_pct)` (engine default 25%). Time-hysteresis
  (`transition.rs` confirm_up/decay_down) alone could not stop oscillation *at*
  the boundary. **Why:** JupSOL flapped DRIFT↔PEGGED around 60 bps (e.g. 84.7 →
  48.8 bps); the deadband (exit 45 bps) holds the state through the dead zone.
- **LST premium carve-out** — `is_direction_sensitive` now includes
  `AssetClass::Lst`. A premium (market > intrinsic) normalizes to PEGGED, same
  as yield-bearing stables. **Why:** for an LST the risk signal is the *discount*
  (redemption stress, cf. stETH −7% in 2022, ezETH depeg); a premium is demand
  pressure, not stress. Discount-side classification is unchanged.

Receipt schema: v1 (unchanged). Verdicts change for some valid inputs, hence the
MINOR bump; replay stays deterministic via the recorded `methodology_git_sha`.
See ADR-0021. v0.1.0 → `deprecated` (still valid for replay), superseded by 0.2.0.

## 0.1.0 — 2026-05-29

- Initial public release.
- Extracted pure functions from pegana-engine v0.1.0; no behavior change.
- Provides: compute_discount, apply_ewma_pure, transition_decide,
  state_for_bps_discount*, state_for_cr, is_plausible_discount_sample,
  canonical_assets_hash, canonical_receipt_hash, methodology_version,
  methodology_git_sha.
- Receipt schema: v1.
