//! Threshold resolution per asset class.

use crate::receipt::PythEntry;
use pegana_common_verify::{AssetClass, PegState};
use rust_decimal::Decimal;
use std::collections::HashMap;

/// Schmitt-trigger magnitude deadband for the spread (bps) path.
///
/// Enter a stricter state at the normal threshold; only relax once the smoothed
/// discount falls below `threshold × (1 - DEADBAND_PCT%)`. Kills DRIFT↔PEGGED
/// flapping when the discount sits near a threshold. 25% → e.g. drift=60bps
/// enters at 60, exits at 45. See `classify_with_hysteresis` and ADR-0023.
pub const DEADBAND_PCT: u32 = 25;

/// CR-side Schmitt-trigger deadband for CR-driven assets (hyUSD).
///
/// A LOWER ratio is worse, so the exit band sits ABOVE the entry threshold:
/// relax only once CR rises above `threshold × (1 + CR_DEADBAND_PCT%)`.
/// Smaller than `DEADBAND_PCT` because CR thresholds are large (~130) — 2% ≈
/// a 2.6pp band, which collapsed ~80% of hyUSD's boundary flapping in the
/// calibration window (52 → ~10 transitions). See `classify_cr_with_hysteresis`
/// and ADR-0023.
pub const CR_DEADBAND_PCT: u32 = 2;

/// Classes where only the DISCOUNT side (market < intrinsic/NAV) carries a
/// risk signal:
///   - Yield-bearing stables (USDY/sUSD/syrupUSDC/sUSDe/ONyc/pbUSDC): market <
///     NAV is the redemption-stress signal; market > NAV is thin secondary
///     liquidity.
///   - LSTs (JupSOL/jitoSOL/mSOL/…): a *premium* (market > redemption value)
///     is demand pressure, not stress — holders can always redeem at intrinsic.
///     The dangerous deviation is the *discount* (cf. stETH −7% in 2022, ezETH
///     depeg) where sellers outrun arbitrage. So a premium normalizes to
///     PEGGED instead of burning a 🚨 DRIFT.
pub fn is_direction_sensitive(class: AssetClass) -> bool {
    matches!(class, AssetClass::StableYield | AssetClass::Lst)
}

/// Classes where a PREMIUM (market above NAV — a *negative* discount) carries
/// no risk and normalizes to PEGGED instead of firing a risk state. Superset of
/// [`is_direction_sensitive`], adding `SynthLev`: a leveraged token (xSOL)
/// trades at large, LEGITIMATE premiums BY DESIGN (leverage demand in a rally),
/// so a premium is never redemption stress here either — publishing DRIFT off a
/// healthy +20% leverage premium is a category error (the market is not
/// "drifting toward a depeg"; the token has no peg).
///
/// The deliberate distinction from `is_direction_sensitive`: `SynthLev` is NOT
/// added to the premium/discount SANITY magnitude masks
/// ([`premium_sanity_violated`] / [`discount_sanity_violated`]). For an LST or
/// yield-stable a premium beyond ~10% means a BROKEN anchor (arbitrage caps
/// real premiums in the low-hundreds of bps → UNKNOWN). A leveraged token's
/// premium is genuinely unbounded and rides an on-chain reserve/supply NAV, so
/// a large premium is REAL, not a broken anchor — masking it to UNKNOWN would
/// just trade the false DRIFT for a false UNKNOWN. The discount side is
/// deliberately left on the normal risk bands (a leveraged token trading BELOW
/// NAV is genuine exit/redemption stress).
pub fn premium_is_informational(class: AssetClass) -> bool {
    matches!(
        class,
        AssetClass::StableYield | AssetClass::Lst | AssetClass::SynthLev
    )
}

/// Maximum plausible PREMIUM (market above NAV/intrinsic — a *negative*
/// discount) for a direction-sensitive class before the intrinsic itself is
/// judged broken rather than the market merely bidding the asset up.
///
/// Direction-sensitive classes (LST, stable_yield) normalize *any* premium to
/// PEGGED (ADR-0021): a premium is demand pressure, not redemption stress, and
/// holders can always redeem at intrinsic. Correct for a real premium — but it
/// also silently masks a BROKEN INTRINSIC. If the NAV/redemption feed reads far
/// too low, the market sits far "above" it and the asset publishes a confident
/// PEGGED off a garbage anchor (sHYUSD: market ≈ +30% over a thin-Jupiter NAV
/// print, yet PEGGED — the validation "formula caveat"). No real LST/yield
/// premium approaches this: arbitrage caps live premiums in the low hundreds of
/// bps (INF ≈160 bps is the widest observed across the 26 assets). So a premium
/// beyond this bound is an intrinsic-quality failure, and the honest output is
/// UNKNOWN (bad anchor), not PEGGED.
///
/// 1000 bps (10%) leaves a >6× margin over the widest legitimate premium while
/// still catching the sHYUSD-class masking. This is NOT the discount-constancy
/// freeze detector rejected in ADR-0024 (no magnitude threshold there could
/// separate flat-healthy from frozen) — it is a magnitude check on a single
/// sample, zero-false-positive against every observed legitimate premium.
pub const NAV_PREMIUM_SANITY_BPS: u32 = 1000;

/// True when `discount` is a premium (negative) on a direction-sensitive class
/// whose magnitude exceeds [`NAV_PREMIUM_SANITY_BPS`] — i.e. the intrinsic is
/// almost certainly broken and the otherwise-masked PEGGED must become UNKNOWN.
/// The discount (positive) side and symmetric classes return false: a real
/// market-below-NAV move is classified by the normal bands, never laundered
/// into UNKNOWN.
pub fn premium_sanity_violated(class: AssetClass, discount: Decimal) -> bool {
    if !(is_direction_sensitive(class) && discount < Decimal::ZERO) {
        return false;
    }
    // checked_mul: `discount` is `discount_smooth` (EWMA output), which
    // apply_ewma_pure guards against overflowing but NOT against being an
    // unbounded magnitude (e.g. an untrusted rederive() alpha/ewma_prev can
    // produce a huge-but-not-overflowing smoothed value). A magnitude too
    // large for this multiplication to survive is itself already far beyond
    // any real premium — treat it as violated (matches the whole point of
    // this check: an implausible discount forces honest-dark UNKNOWN).
    discount
        .abs()
        .checked_mul(Decimal::from(10_000u32))
        .is_none_or(|bps| bps > Decimal::from(NAV_PREMIUM_SANITY_BPS))
}

/// Maximum plausible DISCOUNT (market below NAV/intrinsic — a *positive* discount) for a
/// direction-sensitive / NAV-anchored class before the intrinsic itself is judged broken
/// (garbage-HIGH NAV print) rather than the market having genuinely repriced the asset down.
///
/// Mirror of [`NAV_PREMIUM_SANITY_BPS`] on the discount side. Premium cap catches a garbage-LOW
/// intrinsic (market far "above" it → false PEGGED); this catches a garbage-HIGH intrinsic (market
/// far "below" it → false CRITICAL/BLACK_SWAN). Incident 2026-06-24: JLP's `JLP/USD.RR` Pyth feed
/// printed 21817.55 vs a correct 3.41 (~6400× garbage) → discount 0.9998 (9998 bps) → false
/// BLACK_SWAN to egress; it slipped `is_plausible_discount_sample` (0.9998 < 1.0) AND
/// `premium_sanity_violated` (fires only when discount < 0).
///
/// Bound = 3000 bps (30%). REAL yield/LST depegs are bounded well below this: stETH −7% (2022),
/// ezETH low-double-digits — real breaks live in the 5–25% band and STILL classify (DEPEG/CRITICAL)
/// on the normal bands. The widest legitimate critical BAND on any tracked asset is JLP's
/// critical = 1500 bps; 3000 = 2× that, i.e. at/beyond the BLACK_SWAN entry (2×critical) of even the
/// widest-banded asset, so nothing a calibrated asset reaches in a real depeg is touched. A 30–99.98%
/// "discount" is not a realizable market price for a NAV-anchored token — it is a broken anchor.
/// Magnitude check on ONE smoothed sample, NOT the discount-constancy freeze detector rejected in
/// ADR-0024.
pub const NAV_DISCOUNT_SANITY_BPS: u32 = 3000;

/// True when `discount` is a discount (positive) on a direction-sensitive / NAV-anchored class whose
/// magnitude exceeds [`NAV_DISCOUNT_SANITY_BPS`] — i.e. the intrinsic/NAV anchor is almost certainly
/// a garbage-HIGH print and the resulting false CRITICAL/BLACK_SWAN must become UNKNOWN instead. The
/// premium (negative) side and symmetric classes return false: a real market-below-NAV depeg inside
/// the band still classifies normally.
pub fn discount_sanity_violated(class: AssetClass, discount: Decimal) -> bool {
    if !(is_direction_sensitive(class) && discount > Decimal::ZERO) {
        return false;
    }
    // checked_mul: see premium_sanity_violated — same untrusted-magnitude
    // reasoning, overflow itself is proof the discount is implausible.
    discount
        .abs()
        .checked_mul(Decimal::from(10_000u32))
        .is_none_or(|bps| bps > Decimal::from(NAV_DISCOUNT_SANITY_BPS))
}

/// Widest 1-sigma Pyth confidence/price ratio (bps) treated as a trustworthy
/// oracle read before the VERDICT itself is forced to UNKNOWN, distinct from
/// the display-only `high`/`medium`/`low` bucket in
/// `EngineState::pyth_confidence_label` (engine-rs/src/state.rs:374-401,
/// cut points 10bps/100bps) which only labels a snapshot cosmetically.
///
/// Pyth deliberately does not publish one universal ratio — its own
/// best-practices guidance (docs.pyth.network/price-feeds/core/best-practices)
/// frames confidence-interval use as application-dependent (upper/lower-bound
/// risk parameters), not a fixed cutoff. This default is therefore a
/// CONSERVATIVE STARTING POINT mirroring the ADR-0025/ADR-0030 pattern (wide
/// margin over normal, even noisy, operation) — 5x the existing "low" display
/// bucket's own 100bps cut point — NOT a value Pyth prescribes. The founder
/// MUST run the calibration query in docs/adr/0032-pyth-confidence-gate.md
/// against `oracle_snapshots` before/soon after deploy to confirm near-zero
/// historical trips (the ADR-0030 "provably inert" proof), and may tighten or
/// widen this constant from that data.
pub const PYTH_CONFIDENCE_GATE_BPS: u32 = 500;

/// True when ANY cached Pyth reading this asset depends on (intrinsic AND
/// market legs — both multiply into `discount_raw`, so a wide-uncertainty
/// read on either side taints the verdict equally) has a confidence/price
/// ratio wider than `PYTH_CONFIDENCE_GATE_BPS`.
///
/// Scope: judges only entries ACTUALLY PRESENT in `entries`. An asset with no
/// Pyth dependency (empty map) or a feed whose live reading did not survive
/// upstream staleness/missing-feed gating is left alone — that is a presence/
/// absence question already owned by `pyth_confidence_label` and the
/// intrinsic/market staleness gates (hardening H1/H9/H12: missing data is not
/// confirming data, but it is also not *this* function's job to punish
/// absence — it only judges reads it actually has, exactly like
/// `premium_sanity_violated`/`discount_sanity_violated` judge only a real
/// discount sample, never a missing one).
///
/// A zero price is skipped (no computable ratio, not evidence of anything).
/// `checked_mul` overflow is treated as violated — same untrusted-magnitude
/// reasoning as `premium_sanity_violated` (0.6.1 fix): an implausibly large
/// scaled ratio is itself proof the reading cannot be trusted.
pub fn pyth_confidence_gate_violated(entries: &HashMap<String, PythEntry>) -> bool {
    entries.values().any(|e| {
        if e.price.is_zero() {
            return false;
        }
        // checked_div, NOT raw `/`: a near-zero price overflows `Decimal` on
        // divide, which PANICS (rust_decimal's `Div`, the audit F-12 class — see
        // discount.rs). Reachable with no catch_unwind via the public verifier
        // (rederive) on a crafted receipt AND via the live engine every tick, so
        // a raw divide crash-loops the whole engine / crashes `pegana-replay`.
        // An un-computable (overflowing) ratio is treated as gate-VIOLATED —
        // never a panic — mirroring premium_sanity_violated/discount_sanity_violated.
        let Some(ratio) = e.confidence.checked_div(e.price) else {
            return true;
        };
        ratio
            .abs()
            .checked_mul(Decimal::from(10_000u32))
            .is_none_or(|bps| bps > Decimal::from(PYTH_CONFIDENCE_GATE_BPS))
    })
}

/// Intrinsic-liveness gate (ADR-0038): force UNKNOWN when the receipt froze a
/// stale backing. `intrinsic_stale` is the frozen verdict — `flags.stale` for a
/// sanctum_lst (its rate stopped advancing on-chain) or a stalled
/// (`epoch_lag >= 2`) SPL stake-pool crank — the frozen-but-served backing that
/// otherwise reads as a confident peg (the 2026-04-01 dSOL failure mode).
///
/// Pure + trivial BY DESIGN: the liveness DETERMINATION happens off the verdict
/// path (the indexer sources + the `intrinsic_liveness` table), is frozen into
/// the receipt by the engine ONLY when `INTRINSIC_LIVENESS_GATE` is enabled, and
/// this gate just honours that frozen bit so `rederive` reproduces the verdict.
/// A named predicate (not an inline `inputs.intrinsic_stale`) keeps it a single
/// documented source alongside the other override gates.
pub fn intrinsic_liveness_gate_violated(intrinsic_stale: bool) -> bool {
    intrinsic_stale
}

/// SINGLE SOURCE OF TRUTH for the "force UNKNOWN" override chain + the BLACK_SWAN
/// carve-out. Given the FSM-committed `final_state`, apply the four honest-dark gates
/// (premium-sanity, discount-sanity, Pyth-confidence, intrinsic-liveness) and return the
/// state that must actually be published.
///
/// Which feed/value-quality gate forced an UNKNOWN override, for the caller's
/// diagnostics (`state_reason`) and per-gate metrics. Returned alongside the
/// state by [`override_final_state_attributed`]. The precedence encoded there
/// (premium → discount → Pyth → intrinsic) is the SINGLE source of truth for
/// attribution, so a caller that layers side effects keyed off the reason can
/// never drift from the published state it decided.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OverrideReason {
    /// Premium beyond `NAV_PREMIUM_SANITY_BPS` — a broken/suspect intrinsic anchor.
    PremiumSanity,
    /// Discount beyond `NAV_DISCOUNT_SANITY_BPS`, not (yet) a confirmed BLACK_SWAN.
    DiscountSanity,
    /// A required Pyth feed's confidence interval is too wide (ADR-0032).
    PythConfidence,
    /// The intrinsic backing is flagged stale (ADR-0038; gate opt-in).
    IntrinsicLiveness,
    /// The market source reported consecutive missing quotes; the frozen market
    /// leg is a stale last-known value.
    MarketUnavailable,
}

/// The drift band (bps) of a spread threshold table, after aliases and defaults:
/// the size above which a market premium needs a second source to be believed.
pub fn premium_confirmation_floor_bps(thresholds: &HashMap<String, u32>) -> u32 {
    resolved_bps_bands(thresholds).drift
}

/// The depeg band (bps) of a spread threshold table, after aliases and defaults.
pub fn depeg_band_bps(thresholds: &HashMap<String, u32>) -> u32 {
    resolved_bps_bands(thresholds).depeg
}

/// How closely (bps) an independent reference must match the smoothed discount to
/// CONFIRM a premium: half the drift band, capped at 10 bps. A tolerance as wide as
/// the reference-rejection bound would "confirm" premiums larger than the DEPEG band.
pub fn premium_confirmation_tolerance_bps(thresholds: &HashMap<String, u32>) -> u32 {
    (resolved_bps_bands(thresholds).drift / 2).clamp(1, 10)
}

/// Apply the premium-confirmation gate (ADR-0047) to the raw CANDIDATE, before
/// the time hysteresis, so an unconfirmed premium never enters the state machine
/// at all (a published-state clamp would leave the internal FSM escalating and
/// only delay the false band).
///
/// With `premium_unconfirmed` set on a genuine premium (`discount_smooth < 0`):
/// - a symmetric class (premium = depeg: stable_fiat, cdp, dn, fx, ...) has its
///   candidate capped at DRIFT when no second source confirms the premium (or
///   at `premium_cap`, the band the reference implies);
/// - a direction-sensitive class (lst, stable_yield; a premium normally reads
///   PEGGED) keeps `candidate >= previous_state` while a DEPEG-or-worse band is
///   committed, so an unconfirmed bid cannot start a relaxation.
///
/// The live engine, `rederive` and the backtest runner all call this one
/// function, so they agree by construction.
pub fn confirm_premium_candidate(
    class: AssetClass,
    discount_smooth: Decimal,
    previous_state: PegState,
    premium_unconfirmed: bool,
    premium_cap: Option<PegState>,
    candidate: PegState,
) -> PegState {
    if !premium_unconfirmed || discount_smooth >= Decimal::ZERO {
        return candidate;
    }
    if is_direction_sensitive(class) {
        if rank(previous_state) >= rank(PegState::Depeg) && rank(candidate) < rank(previous_state) {
            return previous_state;
        }
        return candidate;
    }
    // Symmetric: capped at DRIFT, or at the band the independent reference itself
    // implies when that is worse (a real premium depeg with a small basis between
    // the venues must not be stuck at DRIFT).
    let ceiling = match premium_cap {
        Some(cap) if rank(cap) > rank(PegState::Drift) => cap,
        _ => PegState::Drift,
    };
    if rank(candidate) > rank(ceiling) {
        return ceiling;
    }
    candidate
}

/// Whether a dark market leg forces UNKNOWN for an asset classified with
/// `threshold_kind`. A collateral-ratio asset (hyUSD) is classified from its
/// on-chain CR, not from the market leg, so a missing market quote says nothing
/// about its state and it is exempt. Callers pass
/// `market_unavailable && market_unavailable_applies(kind)` to the override,
/// and freeze that same value in the receipt, so `rederive` agrees.
pub fn market_unavailable_applies(threshold_kind: &str) -> bool {
    threshold_kind != "cr"
}

/// The honest-dark UNKNOWN override + confirmed-BLACK_SWAN carve-out, returning
/// BOTH the published state and WHICH gate fired.
///
/// A hysteresis-CONFIRMED `BlackSwan` yields NOTHING to ANY of the FOUR feed/value-quality
/// gates (premium-sanity, discount-sanity, Pyth-confidence, intrinsic-liveness): a
/// sustained, time-validated catastrophic collapse is real regardless of this tick's
/// feed quality. This extends ADR-0039 (which covered only discount-sanity) to all gates:
/// the P1 audit (2026-07-18) proved a wide Pyth confidence interval is the DOCUMENTED
/// signature of the high volatility that PRODUCES a crash, and a follow-up audit-review
/// found the LAST unconditional gate — `premium_sanity` — reopened the same bug: a single
/// garbage-LOW NAV print during a confirmed DISCOUNT collapse reads as a huge apparent
/// PREMIUM (market >> broken NAV) and re-masked the confirmed BLACK_SWAN as UNKNOWN + reset
/// the FSM. The "cannot co-occur" argument only held for the *raw* candidate (a premium
/// classifies to PEGGED, never a BlackSwan candidate) — NOT for the *committed* state,
/// which can be BlackSwan from prior ticks while this tick's smoothed discount glitches
/// negative. The market reality (crashed) is unchanged; only the NAV feed glitched, so the
/// confirmed BLACK_SWAN survives all four gates now.
///
/// The live engine, `rederive`, and the xtask backtest runner ALL derive the published
/// state from this ONE function, so the "three implementations agree by construction"
/// trust invariant cannot drift (it drifted twice before this was centralized — the
/// golden re-baseline debt and this very BLACK_SWAN carve-out gap). The engine additionally
/// layers side effects (force_unknown / metrics / state_reason) keyed off the returned
/// `OverrideReason` — never re-deriving the gate order — while `rederive`/runner take just
/// the state via the [`override_final_state`] wrapper. The returned reason is `None` both
/// when nothing fires AND when a gate fires but the confirmed-BLACK_SWAN carve-out keeps
/// the state (there is no override to attribute in that case).
pub fn override_final_state_attributed(
    class: AssetClass,
    discount_smooth: Decimal,
    pyth_entries: &HashMap<String, PythEntry>,
    intrinsic_stale: bool,
    market_unavailable: bool,
    final_state: PegState,
) -> (PegState, Option<OverrideReason>) {
    // A confirmed BLACK_SWAN is exempt from ALL FOUR gates (see doc). Premium-sanity
    // has no maturation nuance: a premium classifies to PEGGED, so it is never a
    // BlackSwan *candidate* — the guard only protects an already-committed BlackSwan.
    if final_state != PegState::BlackSwan {
        // Checked first: with a stale market leg the discount-derived gates below
        // would be attributing a last-known price.
        if market_unavailable {
            return (PegState::Unknown, Some(OverrideReason::MarketUnavailable));
        }
        if premium_sanity_violated(class, discount_smooth) {
            return (PegState::Unknown, Some(OverrideReason::PremiumSanity));
        }
        if discount_sanity_violated(class, discount_smooth) {
            return (PegState::Unknown, Some(OverrideReason::DiscountSanity));
        }
        if pyth_confidence_gate_violated(pyth_entries) {
            return (PegState::Unknown, Some(OverrideReason::PythConfidence));
        }
        if intrinsic_liveness_gate_violated(intrinsic_stale) {
            return (PegState::Unknown, Some(OverrideReason::IntrinsicLiveness));
        }
    }
    (final_state, None)
}

/// State-only wrapper over [`override_final_state_attributed`] — the form
/// `rederive` and the xtask backtest runner use (they don't attribute the
/// reason). See that function for the full carve-out rationale.
pub fn override_final_state(
    class: AssetClass,
    discount_smooth: Decimal,
    pyth_entries: &HashMap<String, PythEntry>,
    intrinsic_stale: bool,
    market_unavailable: bool,
    final_state: PegState,
) -> PegState {
    override_final_state_attributed(
        class,
        discount_smooth,
        pyth_entries,
        intrinsic_stale,
        market_unavailable,
        final_state,
    )
    .0
}

/// Direction-aware variant of `state_for_bps_discount`. For classes where a
/// premium (market above NAV) carries no risk signal — yield stables, LSTs, and
/// leveraged synths (see [`premium_is_informational`]) — a premium (negative
/// discount) normalizes to PEGGED. The discount side, and fully-symmetric
/// classes, delegate to the abs() form. NOTE: leveraged synths reach PEGGED
/// here at ANY premium magnitude — they are intentionally exempt from the
/// premium-sanity UNKNOWN mask that guards LST/yield anchors — because a large
/// leverage premium is legitimate, not a broken NAV.
pub fn state_for_bps_discount_aware(
    class: AssetClass,
    discount: Decimal,
    thresholds: &HashMap<String, u32>,
) -> PegState {
    if premium_is_informational(class) && discount < Decimal::ZERO {
        return PegState::Pegged;
    }
    state_for_bps_discount(discount, thresholds)
}

/// Strictness rank for hysteresis comparisons (mirrors the engine
/// `transition.rs` ordering). Higher = more severe.
fn rank(s: PegState) -> u8 {
    match s {
        PegState::Pegged | PegState::Unknown => 0,
        PegState::Drift => 1,
        PegState::Depeg => 2,
        PegState::Critical => 3,
        PegState::BlackSwan => 4,
    }
}

/// Lower every band threshold by `deadband_pct` percent (integer, floored).
/// Used to build the *exit* thresholds for the Schmitt-trigger band.
fn lower_thresholds(thresholds: &HashMap<String, u32>, deadband_pct: u32) -> HashMap<String, u32> {
    let keep = 100u32.saturating_sub(deadband_pct);
    thresholds
        .iter()
        .map(|(k, v)| (k.clone(), v.saturating_mul(keep) / 100))
        .collect()
}

/// Magnitude-hysteresis (Schmitt-trigger) classification layer over
/// `state_for_bps_discount_aware`. Time-based hysteresis (engine
/// `transition.rs` confirm_up/decay_down) suppresses brief spikes but NOT a
/// signal that sits and oscillates *around* a threshold — that flaps
/// (JupSOL: DRIFT↔PEGGED around 60bps). A deadband fixes it: escalate to a
/// stricter state at the normal threshold, but only relax back once the
/// discount falls below `threshold × (1 - deadband_pct%)`. `deadband_pct = 0`
/// reduces to the plain `aware` classification.
pub fn classify_with_hysteresis(
    class: AssetClass,
    discount: Decimal,
    thresholds: &HashMap<String, u32>,
    current: PegState,
    deadband_pct: u32,
) -> PegState {
    let raw = state_for_bps_discount_aware(class, discount, thresholds);
    // Escalation (or no change): react at the normal threshold — never let the
    // deadband slow down a worsening peg. Time-hysteresis (confirm_up_secs)
    // already debounces the way up.
    if rank(raw) >= rank(current) {
        return raw;
    }
    // Relaxation toward a looser state: only allow it once the discount falls
    // below the deadband-lowered (exit) thresholds. Classifying against the
    // lowered thresholds yields a state between `raw` and `current` — the band
    // "sticks" until the signal clearly exits.
    let exit_thresholds = lower_thresholds(thresholds, deadband_pct);
    state_for_bps_discount_aware(class, discount, &exit_thresholds)
}

/// Raise every band threshold by `deadband_pct` percent (integer, floored).
/// The CR analog of `lower_thresholds`: for CR-driven assets a LOWER ratio is
/// worse, so the Schmitt-trigger *exit* band sits ABOVE the entry thresholds.
fn raise_thresholds(thresholds: &HashMap<String, u32>, deadband_pct: u32) -> HashMap<String, u32> {
    thresholds
        .iter()
        .map(|(k, v)| (k.clone(), v.saturating_mul(100 + deadband_pct) / 100))
        .collect()
}

/// CR magnitude-hysteresis (Schmitt-trigger) over `state_for_cr`. A CR-driven
/// asset (hyUSD) whose collateral ratio sits a few points above its drift band
/// flaps PEGGED↔DRIFT as oracle jitter clips the threshold. Time-hysteresis
/// (engine `transition.rs` confirm_up/decay_down) debounces brief spikes but
/// NOT sustained oscillation around the band. A deadband fixes it: escalate
/// (CR dropping = worse) at the normal threshold, but only relax once CR rises
/// above `threshold × (1 + deadband_pct%)`. `deadband_pct = 0` reduces to plain
/// `state_for_cr`. Mirrors `classify_with_hysteresis` (spread) with the band
/// inverted, since for CR lower = worse.
pub fn classify_cr_with_hysteresis(
    cr: Decimal,
    thresholds: &HashMap<String, u32>,
    current: PegState,
    deadband_pct: u32,
) -> PegState {
    let raw = state_for_cr(cr, thresholds);
    // Escalation (or no change): react at the normal threshold — never let the
    // deadband slow down a worsening (dropping-CR) peg.
    if rank(raw) >= rank(current) {
        return raw;
    }
    // Relaxation (rising CR): only allow it once CR clears the deadband-raised
    // (exit) thresholds, so a CR oscillating just above its band sticks instead
    // of flapping back to PEGGED.
    let exit_thresholds = raise_thresholds(thresholds, deadband_pct);
    state_for_cr(cr, &exit_thresholds)
}

/// The four resolved band edges of a threshold table, after key aliases and
/// defaults. The classifiers and [`validate_thresholds`] both resolve through
/// here, so validation judges exactly what classification will use.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Bands {
    drift: u32,
    depeg: u32,
    critical: u32,
    black_swan: u32,
    /// `black_swan` was not configured and defaulted from `critical`.
    black_swan_implied: bool,
}

fn band(t: &HashMap<String, u32>, name: &str, alias: &str, default: u32) -> u32 {
    t.get(name)
        .or_else(|| t.get(alias))
        .copied()
        .unwrap_or(default)
}

/// Spread bands (bps): defaults 20/100/300, black_swan = 2 x critical.
fn resolved_bps_bands(t: &HashMap<String, u32>) -> Bands {
    let critical = band(t, "critical", "critical_bps", 300);
    let configured = t
        .get("black_swan")
        .or_else(|| t.get("black_swan_bps"))
        .copied();
    Bands {
        drift: band(t, "drift", "drift_bps", 20),
        depeg: band(t, "depeg", "depeg_bps", 100),
        critical,
        black_swan: configured.unwrap_or_else(|| critical.saturating_mul(2)),
        black_swan_implied: configured.is_none(),
    }
}

/// Collateral-ratio bands (percent): defaults 150/130/110/100.
fn resolved_cr_bands(t: &HashMap<String, u32>) -> Bands {
    Bands {
        drift: band(t, "drift", "cr_drift", 150),
        depeg: band(t, "depeg", "cr_depeg", 130),
        critical: band(t, "critical", "cr_critical", 110),
        black_swan: band(t, "black_swan", "cr_black_swan", 100),
        black_swan_implied: false,
    }
}

/// Convert smoothed discount → state using BPS thresholds.
/// `discount` is `1 - market/intrinsic`. |discount| is what matters for
/// symmetric classes (LST, fiat, dn, fx, synth_lev). For yield-bearing
/// stables, use `state_for_bps_discount_aware` instead.
pub fn state_for_bps_discount(discount: Decimal, thresholds: &HashMap<String, u32>) -> PegState {
    // `discount` is `discount_smooth`, whose magnitude isn't bounded by
    // apply_ewma_pure's overflow guard alone (see premium_sanity_violated).
    // Two independent overflow sites here, both must saturate to u32::MAX
    // (worst case — a larger discount is always worse, unlike a CR ratio
    // where larger = healthier), never to 0/Pegged:
    //   1. checked_mul itself can overflow Decimal (magnitude > ~7.9e24).
    //   2. Below that Decimal ceiling but still > u32::MAX bps (~4.3e9,
    //      i.e. a 430,000%+ discount), the string-to-u32 parse fails on its
    //      own. Caught by re-testing with a correctly re-hashed crafted
    //      receipt that reaches this code, not just a unit test of this
    //      function in isolation — a bare `.unwrap_or(0)` here silently
    //      laundered an implausible reading into a false-healthy Pegged
    //      verdict, arguably worse than the panic this fix started from.
    let abs_bps: u32 = discount
        .abs()
        .checked_mul(Decimal::from(10_000u32))
        .and_then(|scaled| {
            scaled
                .to_string()
                .split('.')
                .next()
                .and_then(|s| s.parse().ok())
        })
        .unwrap_or(u32::MAX);

    // Accept BOTH unsuffixed (assets.toml: `drift = 20`) and suffixed
    // (DB seed JSONB: `"drift_bps": 20`) keys. assets.toml is the canonical
    // source loaded at engine boot; the suffixed fallback exists so a future
    // change to read thresholds straight from the DB row would still work.
    let Bands {
        drift,
        depeg,
        critical,
        black_swan,
        ..
    } = resolved_bps_bands(thresholds);

    match abs_bps {
        n if n >= black_swan => PegState::BlackSwan,
        n if n >= critical => PegState::Critical,
        n if n >= depeg => PegState::Depeg,
        n if n >= drift => PegState::Drift,
        _ => PegState::Pegged,
    }
}

/// Largest spread (bps) the discount path can ever express: `|discount| < 1`.
const MAX_REACHABLE_BPS: u32 = 9_999;

/// Check a per-asset threshold table at load time. Returns advisory warnings, or
/// the first violation as an error.
///
/// `kind` is `"bps"` (spread path: drift < depeg < critical < black_swan) or
/// `"cr"` (collateral-ratio path: drift > depeg > critical > black_swan).
/// Unknown keys are rejected so a typo (`depeg_bp`) cannot silently fall back to
/// a default band. An explicit bps `black_swan` above 9,999 is an error; an
/// IMPLIED one (2 × critical) above it is a warning, because the fifth state is
/// then unreachable for that asset.
pub fn validate_thresholds(
    kind: &str,
    thresholds: &HashMap<String, u32>,
) -> Result<Vec<String>, String> {
    let names = ["drift", "depeg", "critical", "black_swan"];
    let prefixed = |n: &str| match kind {
        "bps" => format!("{n}_bps"),
        _ => format!("cr_{n}"),
    };
    if kind != "bps" && kind != "cr" {
        return Err(format!("unknown threshold kind {kind:?}"));
    }
    for key in thresholds.keys() {
        if !names.iter().any(|n| key == n || *key == prefixed(n)) {
            return Err(format!("unknown threshold key {key:?}"));
        }
    }
    for n in names {
        if thresholds.contains_key(n) && thresholds.contains_key(&prefixed(n)) {
            return Err(format!("{n} is set under both {n:?} and {:?}", prefixed(n)));
        }
    }
    if thresholds.values().any(|v| *v == 0) {
        return Err("a threshold of 0 is not allowed".to_string());
    }
    // Judge the RESOLVED bands (aliases + defaults), exactly as classification
    // will use them, so a partial table cannot hide a mis-ordering.
    let bands = if kind == "bps" {
        resolved_bps_bands(thresholds)
    } else {
        resolved_cr_bands(thresholds)
    };
    let edges = [
        ("drift", bands.drift),
        ("depeg", bands.depeg),
        ("critical", bands.critical),
        ("black_swan", bands.black_swan),
    ];
    for pair in edges.windows(2) {
        let ((lo_name, lo), (hi_name, hi)) = (pair[0], pair[1]);
        let ordered = if kind == "bps" { hi > lo } else { hi < lo };
        if !ordered {
            return Err(format!(
                "{hi_name} ({hi}) must be {} {lo_name} ({lo})",
                if kind == "bps" { "above" } else { "below" }
            ));
        }
    }
    let mut warnings = Vec::new();
    if kind == "bps" && bands.black_swan > MAX_REACHABLE_BPS {
        if bands.black_swan_implied {
            warnings.push(format!(
                "implied black_swan (2 x critical = {} bps) is unreachable; set black_swan explicitly",
                bands.black_swan
            ));
        } else {
            return Err(format!(
                "black_swan ({} bps) is unreachable: |discount| < 1 caps the spread at {MAX_REACHABLE_BPS} bps",
                bands.black_swan
            ));
        }
    }
    Ok(warnings)
}

/// CR-based state for hyUSD-style CDP stables. Thresholds in percentage
/// points (e.g. 150 = 150%).
pub fn state_for_cr(cr: Decimal, thresholds: &HashMap<String, u32>) -> PegState {
    // Same dual-key tolerance as the bps function. assets.toml uses
    // `drift = 150` (no prefix); the DB JSONB uses `cr_drift`.
    let Bands {
        drift,
        depeg,
        critical,
        black_swan,
        ..
    } = resolved_cr_bands(thresholds);

    // Convert CR to integer percent. `checked_mul` guards a `cr` magnitude
    // large enough to overflow Decimal (rust_decimal's `*` PANICS on
    // overflow) — a CR that extreme is already unambiguously deep in
    // "way over-collateralized" territory regardless of its exact value, so
    // saturating to u32::MAX (routed to the healthiest `_ => Pegged` arm
    // below) is the correct classification, not a guess.
    let cr_pct = cr
        .checked_mul(Decimal::from(100u32))
        .and_then(|scaled| {
            scaled
                .to_string()
                .split('.')
                .next()
                .and_then(|s| s.parse::<i64>().ok())
        })
        .map(|n| u32::try_from(n.max(0)).unwrap_or(u32::MAX))
        .unwrap_or(u32::MAX);

    match cr_pct {
        n if n < black_swan => PegState::BlackSwan,
        n if n < critical => PegState::Critical,
        n if n < depeg => PegState::Depeg,
        n if n < drift => PegState::Drift,
        _ => PegState::Pegged,
    }
}

/// The next-worse band below `current` for a CR-driven asset, as a display
/// label + its CR threshold (percent). `None` if already at the worst band
/// (BlackSwan) or not in an alerting band (Pegged/Unknown). Same dual-key
/// tolerance as `state_for_cr` (assets.toml `depeg` / DB JSONB `cr_depeg`).
pub fn next_worse_cr_band(
    current: PegState,
    thresholds: &HashMap<String, u32>,
) -> Option<(&'static str, u32)> {
    let get = |k: &str, alt: &str, d: u32| {
        thresholds
            .get(k)
            .or_else(|| thresholds.get(alt))
            .copied()
            .unwrap_or(d)
    };
    match current {
        PegState::Drift => Some(("DEPEG", get("depeg", "cr_depeg", 130))),
        PegState::Depeg => Some(("CRITICAL", get("critical", "cr_critical", 110))),
        PegState::Critical => Some(("BLACK_SWAN", get("black_swan", "cr_black_swan", 100))),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;
    use std::str::FromStr;

    fn bps_thresholds() -> HashMap<String, u32> {
        let mut m = HashMap::new();
        m.insert("drift_bps".into(), 20);
        m.insert("depeg_bps".into(), 100);
        m.insert("critical_bps".into(), 300);
        m
    }

    fn cr_thresholds() -> HashMap<String, u32> {
        let mut m = HashMap::new();
        m.insert("cr_drift".into(), 150);
        m.insert("cr_depeg".into(), 130);
        m.insert("cr_critical".into(), 110);
        m.insert("cr_black_swan".into(), 100);
        m
    }

    #[test]
    fn cr_hysteresis_escalates_immediately() {
        let t = cr_thresholds(); // drift=150
                                 // CR dropping below drift fires DRIFT at once, even with a deadband.
        assert_eq!(
            classify_cr_with_hysteresis(
                Decimal::from_str("1.49").unwrap(),
                &t,
                PegState::Pegged,
                2
            ),
            PegState::Drift
        );
        // A deeper drop escalates straight to DEPEG — never slow a worsening peg.
        assert_eq!(
            classify_cr_with_hysteresis(Decimal::from_str("1.29").unwrap(), &t, PegState::Drift, 2),
            PegState::Depeg
        );
    }

    #[test]
    fn cr_hysteresis_holds_inside_the_deadband() {
        let t = cr_thresholds(); // drift=150; 2% exit band = 153
                                 // CR recovered above drift (151 >= 150) but not above the 153 exit band:
                                 // stay DRIFT instead of flapping back to PEGGED.
        assert_eq!(
            classify_cr_with_hysteresis(Decimal::from_str("1.51").unwrap(), &t, PegState::Drift, 2),
            PegState::Drift
        );
    }

    #[test]
    fn cr_hysteresis_relaxes_once_cr_clears_the_band() {
        let t = cr_thresholds(); // 2% exit band = 153
        assert_eq!(
            classify_cr_with_hysteresis(Decimal::from_str("1.53").unwrap(), &t, PegState::Drift, 2),
            PegState::Pegged
        );
    }

    #[test]
    fn cr_hysteresis_zero_deadband_is_plain_state_for_cr() {
        let t = cr_thresholds();
        // deadband_pct = 0 → relaxes at the bare threshold (151 >= 150 → PEGGED).
        assert_eq!(
            classify_cr_with_hysteresis(Decimal::from_str("1.51").unwrap(), &t, PegState::Drift, 0),
            PegState::Pegged
        );
    }

    #[test]
    fn next_worse_from_drift_is_depeg() {
        let t = cr_thresholds(); // drift=150 depeg=130 critical=110 black_swan=100
        assert_eq!(
            next_worse_cr_band(PegState::Drift, &t),
            Some(("DEPEG", 130))
        );
    }

    #[test]
    fn next_worse_from_depeg_is_critical() {
        assert_eq!(
            next_worse_cr_band(PegState::Depeg, &cr_thresholds()),
            Some(("CRITICAL", 110))
        );
    }

    #[test]
    fn next_worse_from_critical_is_black_swan() {
        assert_eq!(
            next_worse_cr_band(PegState::Critical, &cr_thresholds()),
            Some(("BLACK_SWAN", 100))
        );
    }

    #[test]
    fn next_worse_from_black_swan_or_pegged_is_none() {
        let t = cr_thresholds();
        assert_eq!(next_worse_cr_band(PegState::BlackSwan, &t), None);
        assert_eq!(next_worse_cr_band(PegState::Pegged, &t), None);
    }

    #[test]
    fn bps_pegged() {
        assert_eq!(
            state_for_bps_discount(Decimal::from_str("0.0010").unwrap(), &bps_thresholds()),
            PegState::Pegged
        );
    }

    #[test]
    fn bps_drift() {
        assert_eq!(
            state_for_bps_discount(Decimal::from_str("0.0050").unwrap(), &bps_thresholds()),
            PegState::Drift
        );
    }

    #[test]
    fn bps_depeg() {
        assert_eq!(
            state_for_bps_discount(Decimal::from_str("0.0150").unwrap(), &bps_thresholds()),
            PegState::Depeg
        );
    }

    #[test]
    fn bps_critical_negative() {
        assert_eq!(
            state_for_bps_discount(Decimal::from_str("-0.0350").unwrap(), &bps_thresholds()),
            PegState::Critical
        );
    }

    // BLACK_SWAN spread band (methodology 0.4.0): default 2× critical, reachable
    // from spread. critical=300 → default black_swan=600.
    #[test]
    fn bps_black_swan_default_is_two_times_critical() {
        // 650 bps ≥ 600 (2×300) → BLACK_SWAN.
        assert_eq!(
            state_for_bps_discount(Decimal::from_str("0.0650").unwrap(), &bps_thresholds()),
            PegState::BlackSwan
        );
        // 500 bps: ≥ critical(300) but < black_swan(600) → still CRITICAL
        // (nothing at/below the old top band changes).
        assert_eq!(
            state_for_bps_discount(Decimal::from_str("0.0500").unwrap(), &bps_thresholds()),
            PegState::Critical
        );
    }

    #[test]
    fn bps_black_swan_explicit_key_overrides_default() {
        let mut t = bps_thresholds(); // critical=300
        t.insert("black_swan_bps".into(), 500);
        // 450 bps: < explicit black_swan(500) → CRITICAL.
        assert_eq!(
            state_for_bps_discount(Decimal::from_str("0.0450").unwrap(), &t),
            PegState::Critical
        );
        // 550 bps: ≥ explicit black_swan(500) → BLACK_SWAN.
        assert_eq!(
            state_for_bps_discount(Decimal::from_str("0.0550").unwrap(), &t),
            PegState::BlackSwan
        );
    }

    #[test]
    fn yield_class_ignores_premium_side() {
        assert_eq!(
            state_for_bps_discount_aware(
                AssetClass::StableYield,
                Decimal::from_str("-0.0200").unwrap(),
                &bps_thresholds()
            ),
            PegState::Pegged,
        );
        assert_eq!(
            state_for_bps_discount_aware(
                AssetClass::StableYield,
                Decimal::from_str("0.0200").unwrap(),
                &bps_thresholds()
            ),
            PegState::Depeg,
        );
    }

    // #3: an LST premium (market > intrinsic) is demand pressure, not
    // redemption stress — the risk side is the DISCOUNT (cf. stETH 2022,
    // ezETH). So LSTs join the direction-sensitive set: premium → PEGGED,
    // discount → classified normally.
    #[test]
    fn lst_ignores_premium_side() {
        // premium (negative discount) normalizes to PEGGED
        assert_eq!(
            state_for_bps_discount_aware(
                AssetClass::Lst,
                Decimal::from_str("-0.0150").unwrap(),
                &bps_thresholds()
            ),
            PegState::Pegged,
        );
        // discount side still classifies (real stress signal)
        assert_eq!(
            state_for_bps_discount_aware(
                AssetClass::Lst,
                Decimal::from_str("0.0150").unwrap(),
                &bps_thresholds()
            ),
            PegState::Depeg,
        );
    }

    // #1: Schmitt-trigger deadband. drift=60 → exit at 60×0.75=45 (25% band).
    fn jup_thresholds() -> HashMap<String, u32> {
        let mut m = HashMap::new();
        m.insert("drift".into(), 60);
        m.insert("depeg".into(), 150);
        m.insert("critical".into(), 300);
        m
    }

    #[test]
    fn hysteresis_enters_drift_at_normal_threshold() {
        // From PEGGED, escalation uses the full threshold (no deadband on the way up).
        assert_eq!(
            classify_with_hysteresis(
                AssetClass::Lst,
                Decimal::from_str("0.0065").unwrap(), // 65bps ≥ 60
                &jup_thresholds(),
                PegState::Pegged,
                25,
            ),
            PegState::Drift,
        );
    }

    #[test]
    fn hysteresis_holds_drift_inside_deadband() {
        // Already DRIFT, discount dips to 50bps: below entry (60) but above
        // exit (45) → must STAY DRIFT (this is the flap the old code emitted).
        assert_eq!(
            classify_with_hysteresis(
                AssetClass::Lst,
                Decimal::from_str("0.0050").unwrap(),
                &jup_thresholds(),
                PegState::Drift,
                25,
            ),
            PegState::Drift,
        );
    }

    #[test]
    fn hysteresis_repegs_below_exit_threshold() {
        // Already DRIFT, discount falls to 40bps (< exit 45) → repeg to PEGGED.
        assert_eq!(
            classify_with_hysteresis(
                AssetClass::Lst,
                Decimal::from_str("0.0040").unwrap(),
                &jup_thresholds(),
                PegState::Drift,
                25,
            ),
            PegState::Pegged,
        );
    }

    #[test]
    fn hysteresis_escalates_without_deadband() {
        // Deadband must NEVER slow an escalation: PEGGED → DEPEG at 160bps.
        assert_eq!(
            classify_with_hysteresis(
                AssetClass::Lst,
                Decimal::from_str("0.0160").unwrap(),
                &jup_thresholds(),
                PegState::Pegged,
                25,
            ),
            PegState::Depeg,
        );
    }

    #[test]
    fn hysteresis_steps_down_one_band_with_deadband() {
        // From DEPEG, 140bps: below depeg(150) but above depeg-exit(112) →
        // stay DEPEG.
        assert_eq!(
            classify_with_hysteresis(
                AssetClass::Lst,
                Decimal::from_str("0.0140").unwrap(),
                &jup_thresholds(),
                PegState::Depeg,
                25,
            ),
            PegState::Depeg,
        );
        // 100bps: below depeg-exit(112) → leave DEPEG; still ≥ drift-exit(45) → DRIFT.
        assert_eq!(
            classify_with_hysteresis(
                AssetClass::Lst,
                Decimal::from_str("0.0100").unwrap(),
                &jup_thresholds(),
                PegState::Depeg,
                25,
            ),
            PegState::Drift,
        );
    }

    #[test]
    fn hysteresis_zero_deadband_is_plain_classification() {
        assert_eq!(
            classify_with_hysteresis(
                AssetClass::Lst,
                Decimal::from_str("0.0050").unwrap(),
                &jup_thresholds(),
                PegState::Drift,
                0,
            ),
            PegState::Pegged, // 50 < 60, no deadband → repegs immediately
        );
    }

    #[test]
    fn hysteresis_lst_premium_stays_pegged_even_when_current_drift() {
        // A premium can't hold an LST in DRIFT — directional carve-out wins.
        assert_eq!(
            classify_with_hysteresis(
                AssetClass::Lst,
                Decimal::from_str("-0.0080").unwrap(),
                &jup_thresholds(),
                PegState::Drift,
                25,
            ),
            PegState::Pegged,
        );
    }

    #[test]
    fn cr_healthy() {
        assert_eq!(
            state_for_cr(Decimal::from(2), &cr_thresholds()),
            PegState::Pegged
        );
    }

    #[test]
    fn cr_drift() {
        assert_eq!(
            state_for_cr(Decimal::from_str("1.40").unwrap(), &cr_thresholds()),
            PegState::Drift
        );
    }

    #[test]
    fn cr_critical() {
        assert_eq!(
            state_for_cr(Decimal::from_str("1.05").unwrap(), &cr_thresholds()),
            PegState::Critical
        );
    }

    #[test]
    fn cr_black_swan() {
        assert_eq!(
            state_for_cr(Decimal::from_str("0.95").unwrap(), &cr_thresholds()),
            PegState::BlackSwan
        );
    }

    /// Regression: `rederive`'s `hyusd_cr` comes straight from an untrusted
    /// receipt with no bound on magnitude. A CR of `Decimal::MAX` previously
    /// panicked ("Multiplication overflowed") on the `cr * 100` scaling
    /// instead of classifying — a CR this astronomically high is unambiguously
    /// deep in over-collateralized territory, so it must classify as Pegged,
    /// not crash the verifier.
    #[test]
    fn cr_overflow_saturates_to_pegged_not_panic() {
        assert_eq!(
            state_for_cr(Decimal::MAX, &cr_thresholds()),
            PegState::Pegged
        );
    }

    /// Regression: `discount_smooth` (rederive's EWMA output) has no bound on
    /// magnitude beyond "didn't overflow the EWMA arithmetic itself" — an
    /// untrusted rederive() alpha/ewma_prev can produce a value large enough
    /// that `discount.abs() * 10_000` (bps scaling) overflows on its own,
    /// even though the EWMA step that produced it succeeded cleanly. Unlike
    /// state_for_cr (where overflow means "obviously healthy"), an
    /// overflowing DISCOUNT is unambiguously the worst case — it must
    /// saturate to BlackSwan, not panic.
    #[test]
    fn bps_discount_overflow_saturates_to_black_swan_not_panic() {
        assert_eq!(
            state_for_bps_discount(Decimal::MAX, &jup_thresholds()),
            PegState::BlackSwan
        );
    }

    /// Second, independent overflow site at a SMALLER magnitude than
    /// `Decimal::MAX`: `checked_mul(10_000)` succeeds (no Decimal overflow),
    /// but the scaled value is still too large to fit in `u32`, so the
    /// string-to-u32 parse fails. Caught only by re-testing against a
    /// correctly re-hashed crafted CLI receipt that actually reaches this
    /// code (a `Decimal::MAX`-only unit test does not exercise this path) —
    /// a bare `.unwrap_or(0)` here previously laundered this into a
    /// false-healthy Pegged verdict instead of the worst-case BlackSwan.
    #[test]
    fn bps_discount_between_decimal_and_u32_ceiling_saturates_to_black_swan() {
        let d = Decimal::from_str("59229100000000000000000").unwrap();
        assert!(d.checked_mul(Decimal::from(10_000u32)).is_some());
        assert_eq!(
            state_for_bps_discount(d, &jup_thresholds()),
            PegState::BlackSwan
        );
    }

    /// Same untrusted-magnitude overflow, but through the NAV-sanity guards
    /// rather than the classification bands. An overflowing discount must
    /// register as "sanity violated" (forces honest-dark UNKNOWN in
    /// rederive), not panic and not silently pass as plausible.
    #[test]
    fn premium_and_discount_sanity_overflow_violates_not_panic() {
        assert!(premium_sanity_violated(AssetClass::Lst, -Decimal::MAX));
        assert!(discount_sanity_violated(AssetClass::Lst, Decimal::MAX));
    }

    /// H2 consequence guard. A missing hyUSD collateral ratio USED to be
    /// synthesized to CR = 1.0 via `unwrap_or(Decimal::ONE)`. This pins exactly
    /// why that was a false alarm: CR = 1.0 → cr_pct = 100, which is NOT
    /// `< black_swan(100)` but IS `< critical(110)`, so it reads as Critical.
    /// The H2 fix (engine `try_recompute`) skips the tick instead of feeding
    /// this fabricated value here — if anyone reverts that, this is the state a
    /// silent CR read-failure would have published.
    #[test]
    fn cr_one_reads_as_critical_h2_consequence() {
        assert_eq!(
            state_for_cr(Decimal::ONE, &cr_thresholds()),
            PegState::Critical,
            "synthesized CR=1.0 fires a FALSE Critical — H2 skips the tick instead",
        );
    }

    // NAV-sanity: an absurd PREMIUM on a direction-sensitive class means the
    // intrinsic/NAV anchor is broken (sHYUSD: market ≈ +30% over a thin NAV
    // print), and the ADR-0021 premium→PEGGED carve-out would mask it. The gate
    // flips those to UNKNOWN; legitimate premiums and the whole discount side
    // are untouched.
    #[test]
    fn premium_sanity_flags_broken_intrinsic_on_direction_sensitive() {
        // sHYUSD-class: ~−30% premium on a yield stable / LST → broken anchor.
        assert!(premium_sanity_violated(
            AssetClass::StableYield,
            Decimal::from_str("-0.30").unwrap()
        ));
        assert!(premium_sanity_violated(
            AssetClass::Lst,
            Decimal::from_str("-0.30").unwrap()
        ));
    }

    #[test]
    fn premium_sanity_allows_real_premiums() {
        // INF ≈ −162 bps is the widest legit premium observed — must pass.
        assert!(!premium_sanity_violated(
            AssetClass::Lst,
            Decimal::from_str("-0.0162").unwrap()
        ));
        // Boundary is strict: exactly 10% is allowed, a hair past is not.
        assert!(!premium_sanity_violated(
            AssetClass::Lst,
            Decimal::from_str("-0.10").unwrap()
        ));
        assert!(premium_sanity_violated(
            AssetClass::Lst,
            Decimal::from_str("-0.1001").unwrap()
        ));
    }

    #[test]
    fn premium_sanity_ignores_discount_side_and_symmetric_classes() {
        // A huge DISCOUNT (positive) is a real depeg — classified by the bands,
        // never laundered into UNKNOWN.
        assert!(!premium_sanity_violated(
            AssetClass::StableYield,
            Decimal::from_str("0.30").unwrap()
        ));
        // Symmetric classes (fiat) are not direction-sensitive — no masking to
        // undo; a 30% deviation there already trips CRITICAL on its own.
        assert!(!premium_sanity_violated(
            AssetClass::StableFiat,
            Decimal::from_str("-0.30").unwrap()
        ));
    }

    #[test]
    fn discount_sanity_flags_garbage_high_intrinsic() {
        // JLP incident: intrinsic 21817.55, market 3.41 -> discount 0.9998.
        assert!(discount_sanity_violated(
            AssetClass::StableYield,
            Decimal::from_str("0.9998").unwrap()
        ));
        assert!(discount_sanity_violated(
            AssetClass::Lst,
            Decimal::from_str("0.3001").unwrap()
        ));
    }

    #[test]
    fn discount_sanity_allows_real_depegs() {
        assert!(!discount_sanity_violated(
            AssetClass::Lst,
            Decimal::from_str("0.07").unwrap()
        )); // stETH -7%
        assert!(!discount_sanity_violated(
            AssetClass::StableYield,
            Decimal::from_str("0.25").unwrap()
        )); // 25% break
        assert!(!discount_sanity_violated(
            AssetClass::Lst,
            Decimal::from_str("0.30").unwrap()
        )); // exactly 30% still classifies (strict >)
    }

    #[test]
    fn discount_sanity_ignores_premium_and_symmetric() {
        assert!(!discount_sanity_violated(
            AssetClass::StableYield,
            Decimal::from_str("-0.30").unwrap()
        )); // premium side = premium_sanity's job
        assert!(!discount_sanity_violated(
            AssetClass::StableFiat,
            Decimal::from_str("0.30").unwrap()
        )); // symmetric class: real 30% break stays CRITICAL/BLACK_SWAN
    }

    // ── Pyth confidence-gate (ADR-0032, 2026-07-16) ────────────────────────
    // Conservative-only guard: a wide Pyth confidence/price ratio taints trust
    // in the reading regardless of asset class, mirroring
    // premium_sanity_violated/discount_sanity_violated's shape (empty/tight →
    // false, wide/overflow → true).

    fn pyth_entry(price: &str, confidence: &str) -> PythEntry {
        PythEntry {
            price: Decimal::from_str(price).unwrap(),
            confidence: Decimal::from_str(confidence).unwrap(),
            publish_time: chrono::DateTime::from_timestamp(1_704_067_200, 0).unwrap(),
        }
    }

    #[test]
    fn pyth_confidence_gate_inert_on_empty_map() {
        assert!(!pyth_confidence_gate_violated(&HashMap::new()));
    }

    #[test]
    fn pyth_confidence_gate_inert_on_tight_confidence() {
        // price=1.0, confidence=0.0005 → 5bps, well under the 500bps gate.
        let mut entries = HashMap::new();
        entries.insert("FEED".to_string(), pyth_entry("1.0", "0.0005"));
        assert!(!pyth_confidence_gate_violated(&entries));
    }

    #[test]
    fn pyth_confidence_gate_fires_on_wide_confidence() {
        // price=1.0, confidence=0.06 → 600bps > 500bps gate.
        let mut entries = HashMap::new();
        entries.insert("FEED".to_string(), pyth_entry("1.0", "0.06"));
        assert!(pyth_confidence_gate_violated(&entries));
    }

    // ── override_final_state_attributed — state + attribution centralized ────────
    // The published state AND which gate fired both come from this one function. The
    // engine keys its force_unknown/metrics/state_reason side effects off the returned
    // OverrideReason; `rederive` + the xtask runner take the state via the
    // `override_final_state` wrapper. Locking the attribution + wrapper agreement here
    // is what makes all three agree BY CONSTRUCTION — the drift this fn exists to end.

    // A wide numéraire feed: 600bps > the 500bps PYTH_CONFIDENCE_GATE_BPS.
    fn wide_conf() -> HashMap<String, PythEntry> {
        HashMap::from([("SOL/USD".to_string(), pyth_entry("1.0", "0.06"))])
    }

    #[test]
    fn attributed_names_the_gate_that_fired() {
        let c = AssetClass::Lst; // direction-sensitive
        let d = |s: &str| Decimal::from_str(s).unwrap();
        // premium beyond bound (negative discount) → PremiumSanity.
        assert_eq!(
            override_final_state_attributed(
                c,
                d("-0.9"),
                &HashMap::new(),
                false,
                false,
                PegState::Pegged
            ),
            (PegState::Unknown, Some(OverrideReason::PremiumSanity)),
        );
        // discount beyond bound → DiscountSanity (state not BlackSwan).
        assert_eq!(
            override_final_state_attributed(
                c,
                d("0.9"),
                &HashMap::new(),
                false,
                false,
                PegState::Depeg
            ),
            (PegState::Unknown, Some(OverrideReason::DiscountSanity)),
        );
        // wide Pyth, benign discount → PythConfidence.
        assert_eq!(
            override_final_state_attributed(
                c,
                Decimal::ZERO,
                &wide_conf(),
                false,
                false,
                PegState::Pegged
            ),
            (PegState::Unknown, Some(OverrideReason::PythConfidence)),
        );
        // stale intrinsic only → IntrinsicLiveness.
        assert_eq!(
            override_final_state_attributed(
                c,
                Decimal::ZERO,
                &HashMap::new(),
                true,
                false,
                PegState::Pegged
            ),
            (PegState::Unknown, Some(OverrideReason::IntrinsicLiveness)),
        );
        // nothing fires → state carried through, no reason to attribute.
        assert_eq!(
            override_final_state_attributed(
                c,
                Decimal::ZERO,
                &HashMap::new(),
                false,
                false,
                PegState::Drift
            ),
            (PegState::Drift, None),
        );
    }

    #[test]
    fn attributed_precedence_is_discount_then_pyth_then_intrinsic() {
        let c = AssetClass::Lst;
        // discount + pyth + intrinsic all fire → discount wins (checked first).
        assert_eq!(
            override_final_state_attributed(
                c,
                Decimal::from_str("0.9").unwrap(),
                &wide_conf(),
                true,
                false,
                PegState::Depeg,
            )
            .1,
            Some(OverrideReason::DiscountSanity),
        );
        // pyth + intrinsic fire → pyth wins.
        assert_eq!(
            override_final_state_attributed(
                c,
                Decimal::ZERO,
                &wide_conf(),
                true,
                false,
                PegState::Pegged
            )
            .1,
            Some(OverrideReason::PythConfidence),
        );
    }

    #[test]
    fn attributed_confirmed_blackswan_is_exempt_from_all_feed_gates() {
        let c = AssetClass::Lst;
        // A confirmed BLACK_SWAN yields to NONE of the four gates (ADR-0039 extended):
        // discount-sanity / Pyth / intrinsic all violated, yet the state survives with
        // no override to attribute. The "masked forever" regression's guard.
        assert_eq!(
            override_final_state_attributed(
                c,
                Decimal::from_str("0.9").unwrap(),
                &wide_conf(),
                true,
                false,
                PegState::BlackSwan,
            ),
            (PegState::BlackSwan, None),
        );
        // Premium-sanity ALSO yields now (audit-review fix): a garbage-LOW NAV print
        // during a confirmed collapse reads as a huge premium and MUST NOT re-mask the
        // committed catastrophe as UNKNOWN — the exact ADR-0039 bug via the last gate.
        assert_eq!(
            override_final_state_attributed(
                c,
                Decimal::from_str("-0.9").unwrap(),
                &HashMap::new(),
                false,
                false,
                PegState::BlackSwan,
            ),
            (PegState::BlackSwan, None),
        );
        // ...but on a NON-BlackSwan state the premium gate still masks (sanity intact).
        assert_eq!(
            override_final_state_attributed(
                c,
                Decimal::from_str("-0.9").unwrap(),
                &HashMap::new(),
                false,
                false,
                PegState::Pegged,
            ),
            (PegState::Unknown, Some(OverrideReason::PremiumSanity)),
        );
    }

    #[test]
    fn cr_percentage_above_u32_saturates_to_the_healthiest_band() {
        // 50,000,000 → 5e9 percent: above u32::MAX; previously wrapped modulo 2^32.
        let t: HashMap<String, u32> = [
            ("drift", 150),
            ("depeg", 130),
            ("critical", 110),
            ("black_swan", 100),
        ]
        .into_iter()
        .map(|(k, v)| (k.to_string(), v))
        .collect();
        assert_eq!(
            state_for_cr(Decimal::from(50_000_000u32), &t),
            PegState::Pegged
        );
    }

    #[test]
    fn threshold_validation_rejects_typos_misordering_and_unreachable_black_swan() {
        let map = |pairs: &[(&str, u32)]| -> HashMap<String, u32> {
            pairs.iter().map(|(k, v)| (k.to_string(), *v)).collect()
        };
        assert_eq!(
            validate_thresholds(
                "bps",
                &map(&[("drift", 20), ("depeg", 100), ("critical", 300)])
            ),
            Ok(vec![])
        );
        assert!(validate_thresholds("bps", &map(&[("drift", 20), ("depeg_bp", 100)])).is_err());
        assert!(validate_thresholds("bps", &map(&[("drift", 100), ("depeg", 50)])).is_err());
        assert!(
            validate_thresholds("bps", &map(&[("critical", 300), ("black_swan", 20_000)])).is_err()
        );
        let warn = validate_thresholds(
            "bps",
            &map(&[("drift", 1500), ("depeg", 3000), ("critical", 6000)]),
        )
        .unwrap();
        assert_eq!(warn.len(), 1, "implied black_swan 12000 bps is unreachable");
        assert!(validate_thresholds(
            "cr",
            &map(&[
                ("drift", 150),
                ("depeg", 130),
                ("critical", 110),
                ("black_swan", 100)
            ])
        )
        .is_ok());
        assert!(validate_thresholds("cr", &map(&[("drift", 150), ("depeg", 160)])).is_err());
    }

    #[test]
    fn validation_judges_resolved_bands_and_rejects_zero_and_duplicates() {
        let map = |pairs: &[(&str, u32)]| -> HashMap<String, u32> {
            pairs.iter().map(|(k, v)| (k.to_string(), *v)).collect()
        };
        // Partial table: drift above the DEFAULT depeg (100) is a mis-ordering.
        assert!(validate_thresholds("bps", &map(&[("drift", 500)])).is_err());
        // Critical below the default depeg.
        assert!(validate_thresholds("bps", &map(&[("critical", 50)])).is_err());
        assert!(validate_thresholds("bps", &map(&[("drift", 0)])).is_err());
        assert!(validate_thresholds("bps", &map(&[("drift", 20), ("drift_bps", 20)])).is_err());
        assert!(validate_thresholds("bps", &map(&[("drift_bps", 20), ("depeg_bps", 100)])).is_ok());
    }

    #[test]
    fn market_unavailable_forces_unknown_first_and_yields_to_black_swan() {
        let c = AssetClass::Lst;
        assert_eq!(
            override_final_state_attributed(
                c,
                Decimal::ZERO,
                &HashMap::new(),
                false,
                true,
                PegState::Pegged
            ),
            (PegState::Unknown, Some(OverrideReason::MarketUnavailable)),
        );
        // Attribution precedes the discount-derived gates (stale values must not
        // be reported as a premium/discount-sanity failure).
        assert_eq!(
            override_final_state_attributed(
                c,
                Decimal::from_str("-0.9").unwrap(),
                &HashMap::new(),
                false,
                true,
                PegState::Pegged
            )
            .1,
            Some(OverrideReason::MarketUnavailable),
        );
        assert_eq!(
            override_final_state_attributed(
                c,
                Decimal::ZERO,
                &HashMap::new(),
                false,
                true,
                PegState::BlackSwan
            ),
            (PegState::BlackSwan, None),
        );
    }

    #[test]
    fn override_final_state_wrapper_returns_the_attributed_state() {
        let c = AssetClass::Lst;
        let d = |s: &str| Decimal::from_str(s).unwrap();
        let cases: [(Decimal, HashMap<String, PythEntry>, bool, PegState); 6] = [
            (d("-0.9"), HashMap::new(), false, PegState::Pegged),
            (d("0.9"), HashMap::new(), false, PegState::Depeg),
            (Decimal::ZERO, wide_conf(), false, PegState::Pegged),
            (Decimal::ZERO, HashMap::new(), true, PegState::Pegged),
            (d("0.9"), wide_conf(), true, PegState::BlackSwan),
            (Decimal::ZERO, HashMap::new(), false, PegState::Drift),
        ];
        for (disc, pyth, stale, fs) in cases {
            assert_eq!(
                override_final_state(c, disc, &pyth, stale, false, fs),
                override_final_state_attributed(c, disc, &pyth, stale, false, fs).0,
                "the state-only wrapper must equal the attributed state",
            );
        }
    }

    #[test]
    fn pyth_confidence_gate_boundary_at_exact_threshold_does_not_fire() {
        // price=1.0, confidence=0.05 → exactly 500bps. Strict `>`, matches
        // premium_sanity_violated's `bps > NAV_PREMIUM_SANITY_BPS`.
        let mut entries = HashMap::new();
        entries.insert("FEED".to_string(), pyth_entry("1.0", "0.05"));
        assert!(!pyth_confidence_gate_violated(&entries));
    }

    #[test]
    fn pyth_confidence_gate_skips_zero_price() {
        let mut entries = HashMap::new();
        entries.insert("FEED".to_string(), pyth_entry("0", "1.0"));
        assert!(!pyth_confidence_gate_violated(&entries));
    }

    #[test]
    fn pyth_confidence_gate_overflow_treated_as_violated() {
        // The DIVISION itself must succeed (ratio = 1e15 / 1e-12 = 1e27, well
        // under Decimal::MAX ≈ 7.92e28) — it's the subsequent
        // `checked_mul(10_000)` (1e27 * 1e4 = 1e31) that overflows, mirroring
        // the existing overflow-as-violated pattern (thresholds.rs
        // premium_and_discount_sanity_overflow_violates_not_panic).
        let mut entries = HashMap::new();
        entries.insert(
            "FEED".to_string(),
            PythEntry {
                price: Decimal::from_str("0.000000000001").unwrap(),
                confidence: Decimal::from_str("1000000000000000").unwrap(),
                publish_time: chrono::DateTime::from_timestamp(1_704_067_200, 0).unwrap(),
            },
        );
        assert!(pyth_confidence_gate_violated(&entries));
    }

    #[test]
    fn pyth_confidence_gate_division_overflow_treated_as_violated_not_panic() {
        // Audit F-12: a near-zero price makes `confidence / price` overflow
        // Decimal, which the raw `/` operator PANICS on (crashing the public
        // verifier + the live engine). With checked_div it must instead read as
        // gate-VIOLATED (true) — no panic. price=1e-28, confidence=100 →
        // ratio 1e30 > Decimal::MAX ≈ 7.92e28.
        let mut entries = HashMap::new();
        entries.insert(
            "FEED".to_string(),
            PythEntry {
                price: Decimal::from_str("0.0000000000000000000000000001").unwrap(),
                confidence: Decimal::from_str("100").unwrap(),
                publish_time: chrono::DateTime::from_timestamp(1_704_067_200, 0).unwrap(),
            },
        );
        assert!(
            pyth_confidence_gate_violated(&entries),
            "division overflow must read as violated, never panic"
        );
    }

    #[test]
    fn pyth_confidence_gate_any_one_of_multiple_feeds_trips_it() {
        // .any() semantics — a single bad leg (e.g. a numéraire feed) can
        // still degrade an otherwise-healthy asset.
        let mut entries = HashMap::new();
        entries.insert("TIGHT".to_string(), pyth_entry("1.0", "0.0005")); // 5bps
        entries.insert("WIDE".to_string(), pyth_entry("1.0", "0.06")); // 600bps
        assert!(pyth_confidence_gate_violated(&entries));
    }

    // ── SynthLev (leveraged, e.g. xSOL) direction-awareness — B, 2026-07-15 ──
    // A leveraged token trades at large, LEGITIMATE premiums by design; a
    // premium must read PEGGED (informational), never DRIFT — and, unlike an
    // LST/yield, must NOT be masked to UNKNOWN by the premium-sanity bound (a
    // big leverage premium is real, not a broken anchor). The DISCOUNT side
    // keeps the normal risk bands (market below NAV = real exit stress).
    #[test]
    fn synth_lev_premium_is_pegged_at_any_magnitude() {
        // +24% premium (the live xSOL reading) → PEGGED, not DRIFT.
        assert_eq!(
            state_for_bps_discount_aware(
                AssetClass::SynthLev,
                Decimal::from_str("-0.24").unwrap(),
                &bps_thresholds()
            ),
            PegState::Pegged
        );
        // even an absurd +500% premium stays informational for a leveraged token.
        assert_eq!(
            state_for_bps_discount_aware(
                AssetClass::SynthLev,
                Decimal::from_str("-5.0").unwrap(),
                &bps_thresholds()
            ),
            PegState::Pegged
        );
    }

    #[test]
    fn synth_lev_discount_still_classifies_on_bands() {
        // market BELOW NAV = real exit/redemption stress → normal risk bands.
        assert_eq!(
            state_for_bps_discount_aware(
                AssetClass::SynthLev,
                Decimal::from_str("0.0150").unwrap(),
                &bps_thresholds()
            ),
            PegState::Depeg
        );
        assert_eq!(
            state_for_bps_discount_aware(
                AssetClass::SynthLev,
                Decimal::from_str("0.0010").unwrap(),
                &bps_thresholds()
            ),
            PegState::Pegged
        );
    }

    #[test]
    fn synth_lev_premium_not_masked_to_unknown() {
        // THE trap this fix avoids: a +24% premium exceeds NAV_PREMIUM_SANITY_BPS
        // (1000 = 10%), but SynthLev is NOT direction-sensitive, so the sanity
        // mask must NOT fire (which would force a false UNKNOWN instead of a real
        // "broken anchor").
        assert!(!premium_sanity_violated(
            AssetClass::SynthLev,
            Decimal::from_str("-0.24").unwrap()
        ));
        // contrast: the SAME +24% premium on an LST DOES trip the mask (a real
        // LST premium never approaches 10% → broken anchor → UNKNOWN).
        assert!(premium_sanity_violated(
            AssetClass::Lst,
            Decimal::from_str("-0.24").unwrap()
        ));
    }

    #[test]
    fn premium_informational_covers_synth_lev_but_sanity_masks_do_not() {
        // premium→PEGGED set includes SynthLev …
        assert!(premium_is_informational(AssetClass::SynthLev));
        assert!(premium_is_informational(AssetClass::Lst));
        assert!(premium_is_informational(AssetClass::StableYield));
        assert!(!premium_is_informational(AssetClass::StableFiat));
        // … but the sanity-mask set stays LST/yield-only (SynthLev excluded).
        assert!(is_direction_sensitive(AssetClass::Lst));
        assert!(is_direction_sensitive(AssetClass::StableYield));
        assert!(!is_direction_sensitive(AssetClass::SynthLev));
    }

    #[test]
    fn symmetric_class_premium_still_classifies_regression() {
        // a fiat-stable premium is NOT informational → normal bands (guard the
        // change didn't leak direction-awareness into symmetric classes).
        assert_eq!(
            state_for_bps_discount_aware(
                AssetClass::StableFiat,
                Decimal::from_str("-0.0150").unwrap(),
                &bps_thresholds()
            ),
            PegState::Depeg
        );
    }

    // ── Proptest helpers ──────────────────────────────────────────────────────

    /// Build a strictly ordered bps threshold map (drift < depeg < critical,
    /// all >= 1) so `state_for_bps_discount` has a well-formed input.
    fn arb_bps_thresholds() -> impl Strategy<Value = HashMap<String, u32>> {
        (1u32..=50u32, 1u32..=50u32, 1u32..=50u32).prop_map(|(a, b, c)| {
            // Sort and spread so drift < depeg < critical.
            let mut vals = [a, a + b, a + b + c];
            vals.sort_unstable();
            let [drift, depeg, critical] = vals;
            let mut m = HashMap::new();
            m.insert("drift".into(), drift.max(1));
            m.insert("depeg".into(), depeg.max(drift + 1));
            m.insert("critical".into(), critical.max(depeg + 1));
            m
        })
    }

    /// Build a strictly ordered CR threshold map (black_swan < critical < depeg <
    /// drift, all >= 1). For CR: lower ratio = worse, so thresholds descend in
    /// severity order.
    fn arb_cr_thresholds() -> impl Strategy<Value = HashMap<String, u32>> {
        (1u32..=30u32, 1u32..=30u32, 1u32..=30u32, 1u32..=30u32).prop_map(|(a, b, c, d)| {
            // We need black_swan < critical < depeg < drift.
            let mut vals = [a, a + b, a + b + c, a + b + c + d];
            vals.sort_unstable();
            let [bs, crit, dep, drift] = vals;
            let mut m = HashMap::new();
            m.insert("drift".into(), (drift + 100).max(1));
            m.insert("depeg".into(), (dep + 100).max(1));
            m.insert("critical".into(), (crit + 100).max(1));
            m.insert("black_swan".into(), (bs + 100).max(1));
            m
        })
    }

    fn arb_peg_state() -> impl Strategy<Value = PegState> {
        prop_oneof![
            Just(PegState::Pegged),
            Just(PegState::Drift),
            Just(PegState::Depeg),
            Just(PegState::Critical),
            Just(PegState::BlackSwan),
            Just(PegState::Unknown),
        ]
    }

    proptest! {
        /// BPS MONOTONICITY: for any two discounts d1 and d2 with |d1| <= |d2|,
        /// the strictness rank of the classified state must be non-decreasing.
        /// A bigger deviation must never yield a less-strict classification.
        #[test]
        fn bps_monotonicity(
            thresholds in arb_bps_thresholds(),
            // d1_abs in [0, 0.2], d2_delta in [0, 0.2] (so d2 = d1 + delta >= d1)
            d1_abs_thousandths in 0u32..=2000u32,
            delta_thousandths   in 0u32..=2000u32,
        ) {
            let d1 = Decimal::new(d1_abs_thousandths as i64, 4);
            let d2 = d1 + Decimal::new(delta_thousandths as i64, 4);

            let s1 = state_for_bps_discount(d1, &thresholds);
            let s2 = state_for_bps_discount(d2, &thresholds);

            prop_assert!(
                rank(s2) >= rank(s1),
                "monotonicity violated: |d1|={} → {:?}(rank {}) but |d2|={} → {:?}(rank {})",
                d1, s1, rank(s1), d2, s2, rank(s2)
            );
        }

        /// CR MONOTONICITY (inverted): for any two ratios cr1 >= cr2,
        /// rank(state_for_cr(cr2)) >= rank(state_for_cr(cr1)).
        /// Lower CR → stricter or equal state.
        #[test]
        fn cr_monotonicity_inverted(
            thresholds in arb_cr_thresholds(),
            // cr1 in [0.5, 3.0], delta >= 0 → cr2 = cr1 - delta <= cr1
            cr1_hundredths in 50u32..=300u32,
            delta_hundredths in 0u32..=200u32,
        ) {
            // cr2 = cr1 - delta (may go negative; state_for_cr clamps via .max(0))
            let cr1 = Decimal::new(cr1_hundredths as i64, 2);
            let delta = Decimal::new(delta_hundredths as i64, 2);
            let cr2 = cr1 - delta; // cr2 <= cr1 → cr2 should be >= as strict

            let s1 = state_for_cr(cr1, &thresholds);
            let s2 = state_for_cr(cr2, &thresholds);

            prop_assert!(
                rank(s2) >= rank(s1),
                "CR monotonicity violated: cr1={} → {:?}(rank {}) but cr2={} → {:?}(rank {})",
                cr1, s1, rank(s1), cr2, s2, rank(s2)
            );
        }

        /// HYSTERESIS NON-FLAP (bps): within the deadband the state must be
        /// held.  Specifically: if `candidate_raw` is strictly LESS strict than
        /// `current`, but the discount is still above the exit threshold
        /// (`threshold × (1 − deadband_pct / 100)`), the result must equal
        /// `current` (not flap to the looser state).
        ///
        /// Implementation note: `classify_with_hysteresis` uses `lower_thresholds`
        /// which is private; we test the observable behaviour at the public API.
        #[test]
        fn hysteresis_no_flap_within_deadband(
            deadband_pct in 1u32..=50u32,
            // Drift threshold in [10, 100] bps (0.001 to 0.01).
            drift_bps in 10u32..=100u32,
            // discount just below drift entry but above exit:
            // entry = drift_bps; exit = drift_bps × (1 - deadband/100)
            // pick discount in [exit+1, drift_bps-1].
        ) {
            // Build a threshold map with just drift (depeg/critical far away).
            let mut thresholds = HashMap::new();
            thresholds.insert("drift".into(), drift_bps);
            thresholds.insert("depeg".into(), drift_bps * 10);
            thresholds.insert("critical".into(), drift_bps * 20);

            // exit = floor(drift_bps × (100 - deadband) / 100)
            let keep = 100u32.saturating_sub(deadband_pct);
            let exit_bps = drift_bps.saturating_mul(keep) / 100;

            // Need at least one integer bps in (exit, drift) for a test to be meaningful.
            prop_assume!(exit_bps + 1 < drift_bps);

            // Pick a discount midway through the deadband.
            let mid_bps = exit_bps + 1; // strictly above exit, strictly below entry
            let discount = Decimal::new(mid_bps as i64, 4); // bps / 10000

            // Current is DRIFT; raw classification at this discount is PEGGED
            // (below the entry drift_bps). The deadband should hold us in DRIFT.
            let result = classify_with_hysteresis(
                AssetClass::StableFiat, // symmetric class
                discount,
                &thresholds,
                PegState::Drift,
                deadband_pct,
            );

            prop_assert_eq!(
                result,
                PegState::Drift,
                "inside deadband (discount={} bps, entry={}, exit={}): \
                 should stay DRIFT but got {:?}",
                mid_bps, drift_bps, exit_bps, result
            );
        }

        /// ESCALATION-NEVER-SLOWED-BY-DEADBAND: the deadband must never
        /// prevent or delay escalation to a stricter band.  For any discount
        /// >= entry threshold from `current`, the result must be at least as
        /// strict as the plain classification.
        #[test]
        fn escalation_never_slowed_by_deadband(
            deadband_pct in 0u32..=50u32,
            thresholds in arb_bps_thresholds(),
            current in arb_peg_state(),
            discount_thousandths in 0u32..=10_000u32,
        ) {
            let discount = Decimal::new(discount_thousandths as i64, 4);
            let plain = state_for_bps_discount(discount, &thresholds);
            let with_band = classify_with_hysteresis(
                AssetClass::StableFiat,
                discount,
                &thresholds,
                current,
                deadband_pct,
            );
            // When the plain result is stricter (or equal) than current, the
            // hysteresis function must return exactly the plain result.
            if rank(plain) >= rank(current) {
                prop_assert_eq!(
                    with_band, plain,
                    "deadband slowed escalation: current={:?} discount={} plain={:?} but got={:?}",
                    current, discount, plain, with_band
                );
            }
        }

        /// CR HYSTERESIS NON-FLAP: within the CR deadband (above drift entry,
        /// below drift exit), the state must be held at DRIFT.  For CR, lower
        /// CR is worse (escalation) so the exit threshold is ABOVE the entry.
        ///
        /// Constraint: we only generate parameter combinations where all
        /// band exit thresholds remain below the drift entry threshold
        /// (`exit_depeg < drift`). This matches the production use case
        /// (hyUSD, assets.toml: deadband=2%, depeg=115, exit_depeg=117,
        /// drift=130 — so 117 < 130, no band overlap).
        /// When deadbands are so large that `exit_depeg >= drift`, the bands
        /// overlap and the "hold" invariant becomes more complex (cross-band
        /// contamination). We use `prop_assume!` to stay in the safe zone.
        #[test]
        fn cr_hysteresis_no_flap_within_deadband(
            deadband_pct in 1u32..=20u32,
            // drift at 150% ± jitter so the arithmetic stays simple.
            drift_offset in 0u32..=30u32,
        ) {
            let drift = 150 + drift_offset;
            // Keep a reasonable gap between drift and depeg so the exit
            // threshold for depeg stays well below the drift entry.
            // depeg = drift - 20 ensures exit_depeg = (drift-20)*(1+pct/100)
            // < drift for any deadband_pct up to ~5%.
            let depeg = drift - 20;
            let critical = drift - 40;
            let black_swan = drift - 60;

            // Guard: only proceed if exit_depeg < drift (no band overlap).
            let exit_depeg = depeg.saturating_mul(100 + deadband_pct) / 100;
            prop_assume!(exit_depeg < drift);

            let mut thresholds = HashMap::new();
            thresholds.insert("drift".into(), drift);
            thresholds.insert("depeg".into(), depeg);
            thresholds.insert("critical".into(), critical);
            thresholds.insert("black_swan".into(), black_swan);

            // exit_drift = drift × (1 + deadband/100)
            let exit_drift = drift.saturating_mul(100 + deadband_pct) / 100;

            // Need at least one integer CR value between drift and exit_drift.
            prop_assume!(exit_drift > drift + 1);

            // CR just above the drift entry but below exit_drift:
            // raw = Pegged (rank 0), but should stay Drift via deadband.
            let mid_pct = drift + 1; // strictly above entry
            prop_assume!(mid_pct < exit_drift); // strictly below exit

            let cr = Decimal::new(mid_pct as i64, 2);
            let result = classify_cr_with_hysteresis(cr, &thresholds, PegState::Drift, deadband_pct);

            prop_assert_eq!(
                result,
                PegState::Drift,
                "inside CR deadband (cr={}%, entry={}%, exit_drift={}%, exit_depeg={}%): \
                 should stay DRIFT but got {:?}",
                mid_pct, drift, exit_drift, exit_depeg, result
            );
        }
    }
}
