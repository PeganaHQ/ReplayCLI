//! Offline re-derivation of the peg-state verdict from frozen receipt inputs.
//!
//! `rederive` replicates the engine `try_recompute` pure pipeline EXACTLY so
//! that anyone can verify "the frozen inputs actually produce the recorded
//! verdict" without running the engine. This is the Grant Thesis ("anyone can
//! recompute our peg history") operationalized.
//!
//! # What is re-derived
//!
//! - `discount_raw` — compute_discount from frozen intrinsic/market prices.
//! - `discount_smooth` — one EWMA step from frozen `ewma_prev` + frozen `alpha`.
//! - `final_state` — classify then transition then NAV-sanity override. As of
//!   ADR-0032, `final_state` also factors `pyth_confidence_gate_violated(&inputs.pyth_entries)`:
//!   a wide confidence/price ratio on any frozen Pyth reading forces UNKNOWN,
//!   conservative-only (never upgrades a verdict, only degrades), same shape
//!   as the premium/discount sanity overrides.
//!
//! # What is NOT re-derived (intentionally deferred)
//!
//! `confidence_label` (the display-only `high`/`medium`/`low`/`unknown` string) is
//! intentionally excluded. Its engine resolver reads `Utc::now()` to gate Pyth feed
//! staleness, making it non-reproducible offline. Option-B follow-up: move the label
//! computation fully into methodology so it can take an explicit `now` — at that point
//! it can be frozen and verified here. Until then, only `(discount_raw, discount_smooth,
//! final_state)` are verified. This NOT-re-derived note applies ONLY to that display
//! string — it does NOT apply to the verdict itself: `pyth_confidence_gate_violated`
//! reads `inputs.pyth_entries`, which IS already frozen (schema v2), so the gate's
//! effect on `final_state` is fully re-derivable here.
//!
//! # Schema v2
//!
//! `rederive` only applies to schema v2 receipts. v1 receipts were emitted with
//! capture-ordering bugs (GAP-1/2/3) that mean `ewma_prev`, `candidate_state`,
//! `candidate_since`, and `previous_state` were not correctly frozen, so
//! re-derivation would produce spurious mismatches. The CLI skips re-derivation
//! for v1 receipts (re-hash only — the existing tamper-evidence path) and
//! applies it only to v2+.

use crate::{
    apply_ewma_pure, classify_cr_with_hysteresis, classify_with_hysteresis, compute_discount,
    confirm_premium_candidate, is_plausible_discount_sample, market_unavailable_applies,
    override_final_state, receipt::InputsFrozen, transition_decide, CR_DEADBAND_PCT, DEADBAND_PCT,
};
use pegana_common_verify::PegState;
use rust_decimal::Decimal;
use thiserror::Error;

/// Error variants for `rederive`.
#[derive(Debug, Error, PartialEq)]
pub enum MethodologyRederiveError {
    /// `compute_discount` returned `None` — intrinsic is zero or produces an
    /// overflow. A v2 receipt is only emitted when discount was `Some`, so this
    /// indicates inconsistent frozen inputs.
    #[error("intrinsic zero or overflow — discount cannot be re-derived")]
    IntrinsicZeroOrOverflow,

    /// The frozen `discount_raw` is outside `|d| < 1.0`. The engine returns
    /// early (no receipt) on implausible samples, so a v2 receipt should never
    /// carry one.
    #[error("implausible discount frozen in receipt — engine would have skipped this tick")]
    ImplausibleSample,

    /// hyUSD CR path requires `hyusd_cr` but the field is `None`.
    #[error("hyUSD CR path selected but hyusd_cr is None in frozen inputs")]
    MissingHyusdCr,

    /// `apply_ewma_pure` overflowed — `ewma_prev`/`alpha` in the frozen inputs
    /// are untrusted and unbounded (unlike the live engine, where they're
    /// always `|x| < 1` by construction). A receipt whose frozen inputs
    /// overflow the EWMA step cannot be re-derived.
    #[error("EWMA smoothing overflowed — frozen ewma_prev/alpha cannot be re-derived")]
    EwmaOverflow,
}

/// The re-derived verdict from frozen `InputsFrozen`.
///
/// Covers the three deterministic outputs: the raw discount, the EWMA-smoothed
/// discount, and the final peg state after classification, time-hysteresis, and
/// the NAV-sanity override. `confidence_label` is intentionally absent — see
/// module-level docs.
#[derive(Debug, Clone, PartialEq)]
pub struct Rederived {
    pub discount_raw: Decimal,
    pub discount_smooth: Decimal,
    pub final_state: PegState,
}

/// Re-derive the peg-state verdict from frozen receipt inputs.
///
/// Replicates the engine `try_recompute` pure pipeline in the exact order:
/// 1. `compute_discount` → `discount_raw`
/// 2. `is_plausible_discount_sample` guard
/// 3. `apply_ewma_pure` → `discount_smooth`
/// 4. `classify_with_hysteresis` / `classify_cr_with_hysteresis` → `candidate`
/// 5. `transition_decide` → `final_state`
/// 6. override → `Unknown` when ANY of: `premium_sanity_violated` /
///    `discount_sanity_violated` / `pyth_confidence_gate_violated` /
///    `intrinsic_liveness_gate_violated(inputs.intrinsic_stale)` (ADR-0038 —
///    a frozen stale backing; `intrinsic_stale` is false on every receipt
///    emitted while the engine's gate is disabled, so this is inert by default)
///
/// # GAP-3 / `previous_state` semantics
///
/// `inputs.previous_state` must be the engine's `g.last_state` captured BEFORE
/// `transition` mutated it (schema v2 guarantee). It is used as `current` for
/// BOTH `classify_*` (deadband seed) and `transition_decide`
/// (`current_last_state`). In steady-state `g.last_state == last_published_state`
/// so both agree. On cold-start `g.last_state` defaults to `Pegged`; v1 receipts
/// froze `prev_opt.unwrap_or(Unknown)` (= `Unknown`) instead — that is the
/// GAP-3 divergence that made re-derivation wrong on cold-start for v1.
///
/// # hyUSD CR branch condition
///
/// The engine branches on `matches!(class, AssetClass::StableCdp) && asset == "hyUSD"`,
/// which corresponds exactly to `threshold_kind == "cr"` in the frozen inputs.
/// We use `threshold_kind` for the branch here so the CLI has zero hard-coded
/// asset-name logic; the field is always `"cr"` for hyUSD and `"bps"` otherwise.
pub fn rederive(inputs: &InputsFrozen) -> Result<Rederived, MethodologyRederiveError> {
    // Step 1: compute raw discount.
    let discount_raw = compute_discount(
        inputs.intrinsic_usd,
        inputs.market_usd,
        inputs.intrinsic_sol,
        inputs.market_sol,
        inputs.class,
    )
    .ok_or(MethodologyRederiveError::IntrinsicZeroOrOverflow)?;

    // Step 2: plausibility guard. The engine returns early (no receipt) when
    // the sample is implausible; a v2 receipt carrying one is inconsistent.
    if !is_plausible_discount_sample(discount_raw) {
        return Err(MethodologyRederiveError::ImplausibleSample);
    }

    // Step 3: EWMA smoothing. `apply_ewma_pure` seeds at `raw` when `prev` is
    // None (cold-start: no prior EWMA for this asset).
    let discount_smooth = apply_ewma_pure(discount_raw, inputs.ewma_prev, inputs.alpha)
        .ok_or(MethodologyRederiveError::EwmaOverflow)?;

    // Step 4: classify. Branch on `threshold_kind` — exactly mirrors the engine
    // condition `matches!(class, AssetClass::StableCdp) && asset == "hyUSD"`.
    // `inputs.previous_state` is the frozen `g.last_state` (schema v2) and
    // serves as the deadband seed (`current`) for both classify functions.
    let candidate = if inputs.threshold_kind == "cr" {
        let cr = inputs
            .hyusd_cr
            .ok_or(MethodologyRederiveError::MissingHyusdCr)?;
        classify_cr_with_hysteresis(
            cr,
            &inputs.thresholds,
            inputs.previous_state,
            CR_DEADBAND_PCT,
        )
    } else {
        classify_with_hysteresis(
            inputs.class,
            discount_smooth,
            &inputs.thresholds,
            inputs.previous_state,
            DEADBAND_PCT,
        )
    };

    // Step 4b: premium-confirmation gate (ADR-0047) on the CANDIDATE, before the
    // time hysteresis, so an unconfirmed premium never matures into a band.
    let candidate = confirm_premium_candidate(
        inputs.class,
        discount_smooth,
        inputs.previous_state,
        inputs.premium_unconfirmed && market_unavailable_applies(&inputs.threshold_kind),
        inputs.premium_cap,
        candidate,
    );

    // Step 5: time-hysteresis (transition_decide). Uses `inputs.previous_state`
    // as `current_last_state` — same field that seeds classify above.
    // `candidate_state`/`candidate_since` are the frozen pre-transition values
    // (schema v2 guarantee via GAP-2 fix).
    let decision = transition_decide(
        inputs.previous_state,
        candidate,
        inputs.candidate_state,
        inputs.candidate_since,
        inputs.now,
        inputs.confirm_up_secs,
        inputs.decay_down_secs,
    );
    let mut final_state = decision.new_last_state;

    // Step 6: the honest-dark UNKNOWN override + BLACK_SWAN carve-out, via the SHARED
    // `override_final_state`. All THREE implementations derive the published state from
    // this one function — re-derivation and the xtask backtest runner call it directly;
    // the live engine `try_recompute` calls its attributed sibling
    // `override_final_state_attributed` (same gate order + carve-out, plus the reason it
    // keys force_unknown/metrics/state_reason off) — so they agree BY CONSTRUCTION and
    // cannot drift. (A confirmed BLACK_SWAN yields to none of the three feed/value gates;
    // premium-sanity stays unconditional — ADR-0039 extended.)
    final_state = override_final_state(
        inputs.class,
        discount_smooth,
        &inputs.pyth_entries,
        inputs.intrinsic_stale,
        inputs.market_unavailable && market_unavailable_applies(&inputs.threshold_kind),
        final_state,
    );

    Ok(Rederived {
        discount_raw,
        discount_smooth,
        final_state,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::receipt::InputsFrozen;
    use chrono::{DateTime, Duration, Utc};
    use pegana_common_verify::{AssetClass, PegState};
    use rust_decimal::Decimal;
    use std::collections::HashMap;
    use std::str::FromStr;

    fn fixed_now() -> DateTime<Utc> {
        DateTime::from_timestamp(1_704_067_200, 0).unwrap()
    }

    fn bps_thresholds() -> HashMap<String, u32> {
        let mut m = HashMap::new();
        m.insert("drift".into(), 20);
        m.insert("depeg".into(), 100);
        m.insert("critical".into(), 300);
        m
    }

    fn cr_thresholds() -> HashMap<String, u32> {
        let mut m = HashMap::new();
        m.insert("drift".into(), 150);
        m.insert("depeg".into(), 130);
        m.insert("critical".into(), 110);
        m.insert("black_swan".into(), 100);
        m
    }

    fn base_inputs(class: AssetClass, threshold_kind: &str) -> InputsFrozen {
        InputsFrozen {
            asset: "TEST".into(),
            class,
            now: fixed_now(),
            alpha: Decimal::from_str("0.3").unwrap(),
            intrinsic_usd: Decimal::ONE,
            market_usd: Decimal::ONE,
            intrinsic_sol: None,
            market_sol: None,
            ewma_prev: None,
            hyusd_cr: None,
            previous_state: PegState::Pegged,
            candidate_state: None,
            candidate_since: None,
            thresholds: bps_thresholds(),
            threshold_kind: threshold_kind.into(),
            pyth_entries: HashMap::new(),
            confirm_up_secs: 30,
            decay_down_secs: 120,
            intrinsic_stale: false,
            market_unavailable: false,
            premium_unconfirmed: false,
            premium_cap: None,
        }
    }

    // (a) USD stable PEGGED — market at intrinsic, previous PEGGED, no EWMA prior.
    #[test]
    fn usd_stable_pegged() {
        let inputs = base_inputs(AssetClass::StableFiat, "bps");
        let r = rederive(&inputs).expect("should succeed");
        assert_eq!(r.discount_raw, Decimal::ZERO);
        assert_eq!(r.discount_smooth, Decimal::ZERO); // cold-start seeds at raw
        assert_eq!(r.final_state, PegState::Pegged);
    }

    // Intrinsic-liveness gate (ADR-0038): a frozen `intrinsic_stale=true` forces
    // UNKNOWN even on an otherwise-PEGGED asset — the frozen-backing (dSOL) mode.
    #[test]
    fn intrinsic_liveness_gate_forces_unknown_when_stale() {
        let mut inputs = base_inputs(AssetClass::Lst, "bps");
        // Perfectly at NAV → would be PEGGED without the gate.
        assert_eq!(
            rederive(&inputs).unwrap().final_state,
            PegState::Pegged,
            "sanity: healthy LST is PEGGED with the gate off (field false)"
        );
        inputs.intrinsic_stale = true;
        assert_eq!(
            rederive(&inputs).unwrap().final_state,
            PegState::Unknown,
            "a stale backing must force UNKNOWN"
        );
    }

    // Market-unavailable gate: a frozen dark market leg forces UNKNOWN, so the
    // engine's dark-market receipt re-derives to the published state.
    #[test]
    fn market_unavailable_receipt_rederives_to_unknown() {
        let mut inputs = base_inputs(AssetClass::StableFiat, "bps");
        assert_eq!(rederive(&inputs).unwrap().final_state, PegState::Pegged);
        inputs.market_unavailable = true;
        assert_eq!(rederive(&inputs).unwrap().final_state, PegState::Unknown);
    }

    // A CR-classified asset (hyUSD) is exempt: its state comes from the on-chain
    // CR, so a dark market leg must not force UNKNOWN (and the receipt agrees).
    #[test]
    fn market_unavailable_does_not_apply_to_cr_assets() {
        let mut inputs = base_inputs(AssetClass::StableCdp, "cr");
        inputs.hyusd_cr = Some(Decimal::from_str("2.0").unwrap());
        let healthy = rederive(&inputs).unwrap().final_state;
        inputs.market_unavailable = true;
        assert_eq!(rederive(&inputs).unwrap().final_state, healthy);
    }

    // A confirmed BLACK_SWAN yields to the dark-market gate (ADR-0039).
    #[test]
    fn market_unavailable_yields_to_a_confirmed_black_swan() {
        let mut inputs = base_inputs(AssetClass::StableFiat, "bps");
        inputs.market_usd = Decimal::from_str("0.40").unwrap();
        inputs.previous_state = PegState::BlackSwan;
        inputs.market_unavailable = true;
        assert_eq!(rederive(&inputs).unwrap().final_state, PegState::BlackSwan);
    }

    // F07: classification is seeded from the FSM state the receipt freezes
    // (`previous_state`). A committed DEPEG with a matured DRIFT relaxation
    // candidate and a smoothed discount inside the sticky deadband (75–100 bps)
    // must stay DEPEG on BOTH the engine (now seeded from `last_state`) and here.
    #[test]
    fn classification_is_seeded_from_the_frozen_fsm_state() {
        let mut inputs = base_inputs(AssetClass::StableFiat, "bps");
        inputs.previous_state = PegState::Depeg;
        inputs.candidate_state = Some(PegState::Drift);
        inputs.candidate_since = Some(fixed_now() - Duration::seconds(3_600));
        inputs.market_usd = Decimal::from_str("0.9915").unwrap(); // 85 bps
        assert_eq!(rederive(&inputs).unwrap().final_state, PegState::Depeg);
        // The same discount from a PEGGED seed is DRIFT: the seed decides.
        inputs.previous_state = PegState::Pegged;
        inputs.candidate_state = None;
        inputs.candidate_since = None;
        assert_ne!(rederive(&inputs).unwrap().final_state, PegState::Depeg);
    }

    // Provably inert when false: the gate never changes a verdict for the
    // default value (which is the ONLY value emitted while the engine's feature
    // flag is disabled → byte-exact receipts).
    #[test]
    fn intrinsic_liveness_gate_is_inert_when_false() {
        for class in [
            AssetClass::Lst,
            AssetClass::StableFiat,
            AssetClass::StableDn,
        ] {
            let mut inputs = base_inputs(class, "bps");
            inputs.market_usd = Decimal::from_str("0.997").unwrap(); // some drift
            let with_default = rederive(&inputs).unwrap();
            inputs.intrinsic_stale = false; // explicit default
            let explicit_false = rederive(&inputs).unwrap();
            assert_eq!(
                with_default.final_state, explicit_false.final_state,
                "{class:?}: intrinsic_stale=false must not alter the verdict"
            );
        }
    }

    // ── NAV-sanity ↔ BLACK_SWAN collision (ADR-0039) ────────────────────────────
    // A discount beyond NAV_DISCOUNT_SANITY_BPS (3000 = 30%) used to be force-
    // downgraded to UNKNOWN UNCONDITIONALLY. For JLP (critical=1500 → default
    // black_swan = critical*2 = 3000, numerically EQUAL to the sanity bound) this
    // masked a real >30% catastrophic collapse as UNKNOWN forever — the product
    // showing "data looks unreliable" during the exact catastrophe it exists to
    // call. The fix: discount-sanity YIELDS to a hysteresis-CONFIRMED BLACK_SWAN.

    // Mirrors assets.toml JLP: {drift=200, depeg=500, critical=1500}, NO black_swan
    // override → default = critical*2 = 3000 = NAV_DISCOUNT_SANITY_BPS (the collision).
    fn jlp_like_thresholds() -> HashMap<String, u32> {
        let mut m = HashMap::new();
        m.insert("drift".into(), 200);
        m.insert("depeg".into(), 500);
        m.insert("critical".into(), 1500);
        m
    }

    // A direction-sensitive asset at a 31% discount (3100 bps): market 0.69 vs
    // intrinsic 1.0. 3100 > NAV_DISCOUNT_SANITY_BPS(3000) → sanity fires; and
    // 3100 >= default black_swan(3000) → the band classifies BLACK_SWAN.
    fn catastrophe_inputs() -> InputsFrozen {
        let mut inputs = base_inputs(AssetClass::Lst, "bps");
        inputs.thresholds = jlp_like_thresholds();
        inputs.market_usd = Decimal::from_str("0.69").unwrap();
        inputs
    }

    #[test]
    fn nav_sanity_yields_to_confirmed_blackswan() {
        // A >30% collapse that has MATURED through the confirm window (candidate
        // BLACK_SWAN since well past confirm_up=30s) must publish BLACK_SWAN — the
        // real catastrophic depeg — NOT the old force-masked UNKNOWN.
        let mut inputs = catastrophe_inputs();
        inputs.previous_state = PegState::Critical;
        inputs.candidate_state = Some(PegState::BlackSwan);
        inputs.candidate_since = Some(fixed_now() - Duration::seconds(60));
        let r = rederive(&inputs).unwrap();
        assert_eq!(r.discount_smooth, Decimal::from_str("0.31").unwrap());
        assert_eq!(
            r.final_state,
            PegState::BlackSwan,
            "a confirmed >30% collapse must surface BLACK_SWAN, not be masked UNKNOWN"
        );
    }

    #[test]
    fn nav_sanity_masks_maturing_catastrophe_as_unknown() {
        // Same extreme discount, but the BLACK_SWAN candidate has NOT yet matured
        // → honest-dark UNKNOWN while the confirm window runs (could still be a
        // garbage-HIGH NAV print). Crucially never a false PEGGED/CRITICAL.
        let mut inputs = catastrophe_inputs();
        inputs.previous_state = PegState::Critical;
        inputs.candidate_state = Some(PegState::BlackSwan);
        inputs.candidate_since = Some(fixed_now()); // 0s elapsed < confirm_up
        assert_eq!(rederive(&inputs).unwrap().final_state, PegState::Unknown);
    }

    #[test]
    fn nav_sanity_masks_first_sighting_as_unknown() {
        // First sighting of the extreme discount from PEGGED (no prior candidate)
        // → UNKNOWN, never a false PEGGED off the untrusted reading. The live
        // engine keeps the freshly-queued BLACK_SWAN candidate maturing so a
        // SUSTAINED collapse later confirms (proven by the confirmed test above).
        let inputs = catastrophe_inputs();
        assert_eq!(rederive(&inputs).unwrap().final_state, PegState::Unknown);
    }

    // ── The carve-out extends to ALL gates (P1 audit 2026-07-18) ─────────────────
    // A wide Pyth confidence interval is the DOCUMENTED signature of the high
    // volatility that PRODUCES a crash — correlated, not orthogonal — so leaving the
    // Pyth (and intrinsic-liveness) gate unconditional re-masked a confirmed collapse
    // as UNKNOWN forever via a side door. `override_final_state` now exempts a
    // hysteresis-confirmed BLACK_SWAN from all three feed/value gates.

    // Confidence/price = 0.10/1.0 = 1000 bps > the 500 bps PYTH_CONFIDENCE_GATE_BPS.
    fn wide_pyth() -> HashMap<String, crate::receipt::PythEntry> {
        HashMap::from([(
            "SOL/USD".to_string(),
            crate::receipt::PythEntry {
                price: Decimal::ONE,
                confidence: Decimal::from_str("0.10").unwrap(),
                publish_time: fixed_now(),
            },
        )])
    }

    #[test]
    fn confirmed_blackswan_survives_wide_pyth_confidence() {
        // A CONFIRMED >30% collapse must still publish BLACK_SWAN even though the
        // shared numéraire feed's confidence is simultaneously wide (same market
        // stress). Without the carve-out this reproduced the pre-ADR-0039 "masked
        // forever" bug through the Pyth gate.
        let mut inputs = catastrophe_inputs();
        inputs.previous_state = PegState::Critical;
        inputs.candidate_state = Some(PegState::BlackSwan);
        inputs.candidate_since = Some(fixed_now() - Duration::seconds(60));
        inputs.pyth_entries = wide_pyth();
        assert_eq!(
            rederive(&inputs).unwrap().final_state,
            PegState::BlackSwan,
            "confirmed BLACK_SWAN must survive a wide Pyth confidence interval"
        );
    }

    #[test]
    fn maturing_blackswan_with_wide_pyth_is_unknown() {
        // Not-yet-confirmed → honest-dark UNKNOWN (the gate still protects an
        // unconfirmed reading); the candidate is preserved so it can still mature.
        let mut inputs = catastrophe_inputs();
        inputs.previous_state = PegState::Critical;
        inputs.candidate_state = Some(PegState::BlackSwan);
        inputs.candidate_since = Some(fixed_now());
        inputs.pyth_entries = wide_pyth();
        assert_eq!(rederive(&inputs).unwrap().final_state, PegState::Unknown);
    }

    #[test]
    fn confirmed_blackswan_survives_stale_intrinsic() {
        // The intrinsic-liveness gate (ADR-0038) analog: a stale backing must not
        // erase a sustained, confirmed catastrophic collapse either.
        let mut inputs = catastrophe_inputs();
        inputs.previous_state = PegState::Critical;
        inputs.candidate_state = Some(PegState::BlackSwan);
        inputs.candidate_since = Some(fixed_now() - Duration::seconds(60));
        inputs.intrinsic_stale = true;
        assert_eq!(rederive(&inputs).unwrap().final_state, PegState::BlackSwan);
    }

    // (b) USD stable crossing into DRIFT — 30bps discount, previous PEGGED.
    // 30bps > drift(20bps) → candidate = DRIFT. Timer just started (new candidate,
    // no prior) → transition_decide returns current (Pegged) with candidate queued.
    // So final_state remains PEGGED until confirm_up elapses.
    #[test]
    fn usd_stable_enters_drift_first_tick() {
        let mut inputs = base_inputs(AssetClass::StableFiat, "bps");
        // market = 0.997, intrinsic = 1.0 → discount = 0.003 = 30bps > 20bps drift
        inputs.market_usd = Decimal::from_str("0.997").unwrap();
        let r = rederive(&inputs).expect("should succeed");
        // raw discount = 1 - 0.997/1.0 = 0.003
        assert_eq!(r.discount_raw, Decimal::from_str("0.003").unwrap());
        // cold-start EWMA seeds at raw
        assert_eq!(r.discount_smooth, Decimal::from_str("0.003").unwrap());
        // New candidate DRIFT, timer just started → Pegged stays (Case 2)
        assert_eq!(r.final_state, PegState::Pegged);
        assert_eq!(inputs.candidate_state, None); // frozen before transition
    }

    // (b2) Confirm DRIFT after timer: candidate already queued for > confirm_up_secs.
    #[test]
    fn usd_stable_commits_drift_after_confirm_window() {
        let mut inputs = base_inputs(AssetClass::StableFiat, "bps");
        inputs.market_usd = Decimal::from_str("0.997").unwrap();
        // Fake: candidate already queued as DRIFT 31s before now → matures.
        inputs.candidate_state = Some(PegState::Drift);
        inputs.candidate_since = Some(fixed_now() - Duration::seconds(31));
        let r = rederive(&inputs).expect("should succeed");
        assert_eq!(r.final_state, PegState::Drift);
    }

    // (c) LST using the SOL path.
    #[test]
    fn lst_sol_path() {
        let mut inputs = base_inputs(AssetClass::Lst, "bps");
        // SOL-denominated: intrinsic_sol = 1.0, market_sol = 1.0 → discount = 0
        inputs.intrinsic_sol = Some(Decimal::from_str("1.0").unwrap());
        inputs.market_sol = Some(Decimal::from_str("1.0").unwrap());
        // Make USD prices differ to confirm we are on the SOL path
        inputs.intrinsic_usd = Decimal::from_str("100.0").unwrap();
        inputs.market_usd = Decimal::from_str("101.0").unwrap(); // USD premium but SOL = 0
        let r = rederive(&inputs).expect("should succeed");
        assert_eq!(r.discount_raw, Decimal::ZERO);
        assert_eq!(r.final_state, PegState::Pegged);
    }

    // (d) hyUSD CR path: StableCdp + threshold_kind="cr" + hyusd_cr present.
    // CR = 1.60 (above drift=150) → PEGGED.
    #[test]
    fn hyusd_cr_path_pegged() {
        let mut inputs = base_inputs(AssetClass::StableCdp, "cr");
        inputs.thresholds = cr_thresholds();
        inputs.hyusd_cr = Some(Decimal::from_str("1.60").unwrap());
        // intrinsic/market can be anything reasonable for the discount path;
        // the CR path uses hyusd_cr for classify but still requires a valid discount.
        inputs.intrinsic_usd = Decimal::from_str("1.0").unwrap();
        inputs.market_usd = Decimal::from_str("0.999").unwrap();
        let r = rederive(&inputs).expect("should succeed");
        assert_eq!(r.final_state, PegState::Pegged);
    }

    // (d2) hyUSD CR path: CR = 1.40 (130 < 140 < 150 = drift threshold) → DRIFT.
    // First tick with new candidate → timer starts, stays Pegged.
    #[test]
    fn hyusd_cr_path_drift_first_tick() {
        let mut inputs = base_inputs(AssetClass::StableCdp, "cr");
        inputs.thresholds = cr_thresholds();
        inputs.hyusd_cr = Some(Decimal::from_str("1.40").unwrap());
        inputs.intrinsic_usd = Decimal::from_str("1.0").unwrap();
        inputs.market_usd = Decimal::from_str("0.999").unwrap();
        let r = rederive(&inputs).expect("should succeed");
        // classify_cr → DRIFT (1.40 < 1.50), transition → Pegged (Case 2, timer started)
        assert_eq!(r.final_state, PegState::Pegged);
    }

    // (d3) hyUSD CR path committed after confirm window.
    #[test]
    fn hyusd_cr_path_drift_committed() {
        let mut inputs = base_inputs(AssetClass::StableCdp, "cr");
        inputs.thresholds = cr_thresholds();
        inputs.hyusd_cr = Some(Decimal::from_str("1.40").unwrap());
        inputs.intrinsic_usd = Decimal::from_str("1.0").unwrap();
        inputs.market_usd = Decimal::from_str("0.999").unwrap();
        inputs.candidate_state = Some(PegState::Drift);
        inputs.candidate_since = Some(fixed_now() - Duration::seconds(31));
        let r = rederive(&inputs).expect("should succeed");
        assert_eq!(r.final_state, PegState::Drift);
    }

    // (e) Premium-sanity override: LST with > 10% premium → Unknown.
    #[test]
    fn premium_sanity_override_to_unknown() {
        let mut inputs = base_inputs(AssetClass::Lst, "bps");
        // discount = 1 - 1.15/1.0 = -0.15 (-1500bps) → premium 15% > 10% → Unknown
        inputs.intrinsic_usd = Decimal::from_str("1.0").unwrap();
        inputs.market_usd = Decimal::from_str("1.15").unwrap();
        // No SOL override → USD path
        inputs.intrinsic_sol = None;
        inputs.market_sol = None;
        let r = rederive(&inputs).expect("should succeed");
        assert_eq!(r.final_state, PegState::Unknown);
    }

    #[test]
    fn discount_sanity_override_to_unknown() {
        let mut inputs = base_inputs(AssetClass::StableYield, "bps");
        // JLP incident: intrinsic 21817.55, market 3.41 -> discount ≈ 0.9998.
        inputs.intrinsic_usd = Decimal::from_str("21817.55").unwrap();
        inputs.market_usd = Decimal::from_str("3.41").unwrap();
        inputs.intrinsic_sol = None;
        inputs.market_sol = None;
        let r = rederive(&inputs).expect("should succeed");
        assert_eq!(r.final_state, PegState::Unknown);
    }

    #[test]
    fn discount_sanity_allows_real_depeg_to_classify() {
        let mut inputs = base_inputs(AssetClass::StableYield, "bps");
        inputs.intrinsic_usd = Decimal::from_str("1.0").unwrap();
        inputs.market_usd = Decimal::from_str("0.85").unwrap();
        inputs.intrinsic_sol = None;
        inputs.market_sol = None;
        inputs.thresholds.insert("drift".into(), 500);
        inputs.thresholds.insert("depeg".into(), 1000);
        inputs.thresholds.insert("critical".into(), 1500);
        inputs.thresholds.insert("black_swan".into(), 3000);
        inputs.candidate_state = Some(PegState::Critical);
        inputs.candidate_since = Some(fixed_now() - Duration::seconds(31));
        let r = rederive(&inputs).expect("should succeed");
        assert_eq!(r.final_state, PegState::Critical);
    }

    // ── Pyth confidence-gate (ADR-0032, 2026-07-16) ────────────────────────

    fn pyth_entry(price: &str, confidence: &str) -> crate::receipt::PythEntry {
        crate::receipt::PythEntry {
            price: Decimal::from_str(price).unwrap(),
            confidence: Decimal::from_str(confidence).unwrap(),
            publish_time: fixed_now(),
        }
    }

    // (g) Pyth confidence-gate override: a healthy discount that would
    // otherwise classify PEGGED/DRIFT is forced to Unknown by a single
    // wide-confidence frozen Pyth entry.
    #[test]
    fn pyth_confidence_gate_override_to_unknown() {
        let mut inputs = base_inputs(AssetClass::StableFiat, "bps");
        // market at intrinsic → discount = 0 (would classify PEGGED).
        let mut entries = HashMap::new();
        entries.insert("FEED".to_string(), pyth_entry("1.0", "0.06")); // 600bps > 500bps gate
        inputs.pyth_entries = entries;
        let r = rederive(&inputs).expect("should succeed");
        assert_eq!(r.final_state, PegState::Unknown);
    }

    // (h) A tight-confidence Pyth entry must NOT force Unknown — proves the
    // gate doesn't leak into unrelated (healthy) ticks.
    #[test]
    fn pyth_confidence_gate_allows_tight_confidence_to_classify() {
        let mut inputs = base_inputs(AssetClass::StableFiat, "bps");
        let mut entries = HashMap::new();
        entries.insert("FEED".to_string(), pyth_entry("1.0", "0.0005")); // 5bps
        inputs.pyth_entries = entries;
        let r = rederive(&inputs).expect("should succeed");
        assert_eq!(r.final_state, PegState::Pegged);
    }

    // (i) No frozen Pyth entries (empty map) — the common no-Pyth-dependency
    // and backtest-fixture case — must classify exactly as before the gate
    // existed. This is the test that pins the backtest's zero-diff guarantee:
    // the xtask runner freezes `pyth_entries: HashMap::new()` (runner.rs:149),
    // so the gate must be a structural no-op there.
    #[test]
    fn pyth_confidence_gate_inert_when_no_pyth_entries_frozen() {
        let inputs = base_inputs(AssetClass::StableFiat, "bps");
        assert!(inputs.pyth_entries.is_empty());
        let r = rederive(&inputs).expect("should succeed");
        assert_eq!(r.final_state, PegState::Pegged);
    }

    // (f) Cold-start: previous_state = Pegged (default), candidate_state = None.
    // Verifies clean seeding without any prior EWMA.
    #[test]
    fn cold_start_pegged_no_ewma() {
        let inputs = base_inputs(AssetClass::StableFiat, "bps");
        // previous_state = Pegged, candidate_state = None, ewma_prev = None
        assert_eq!(inputs.previous_state, PegState::Pegged);
        assert_eq!(inputs.candidate_state, None);
        assert_eq!(inputs.ewma_prev, None);
        let r = rederive(&inputs).expect("cold-start pegged succeeds");
        assert_eq!(r.discount_raw, Decimal::ZERO);
        assert_eq!(r.discount_smooth, Decimal::ZERO); // seeds at raw
        assert_eq!(r.final_state, PegState::Pegged);
    }

    // Error case: zero intrinsic.
    #[test]
    fn zero_intrinsic_errors() {
        let mut inputs = base_inputs(AssetClass::StableFiat, "bps");
        inputs.intrinsic_usd = Decimal::ZERO;
        assert_eq!(
            rederive(&inputs),
            Err(MethodologyRederiveError::IntrinsicZeroOrOverflow)
        );
    }

    // Error case: implausible sample (market = 0 → discount = 1.0 = not plausible).
    #[test]
    fn implausible_sample_errors() {
        let mut inputs = base_inputs(AssetClass::StableFiat, "bps");
        inputs.market_usd = Decimal::ZERO; // discount = 1.0, not plausible
        assert_eq!(
            rederive(&inputs),
            Err(MethodologyRederiveError::ImplausibleSample)
        );
    }

    // Error case: CR path but hyusd_cr missing.
    #[test]
    fn cr_path_missing_cr_errors() {
        let mut inputs = base_inputs(AssetClass::StableCdp, "cr");
        inputs.thresholds = cr_thresholds();
        inputs.hyusd_cr = None; // missing
        assert_eq!(
            rederive(&inputs),
            Err(MethodologyRederiveError::MissingHyusdCr)
        );
    }

    // EWMA: with a prior value.
    #[test]
    fn ewma_blends_with_prior() {
        let mut inputs = base_inputs(AssetClass::StableFiat, "bps");
        // discount_raw = 0.002 (20bps, at the drift threshold = 20)
        inputs.market_usd = Decimal::from_str("0.998").unwrap();
        inputs.ewma_prev = Some(Decimal::from_str("0.001").unwrap());
        // smooth = 0.3 * 0.002 + 0.7 * 0.001 = 0.0006 + 0.0007 = 0.0013
        let r = rederive(&inputs).expect("should succeed");
        assert_eq!(r.discount_raw, Decimal::from_str("0.002").unwrap());
        let expected_smooth = Decimal::from_str("0.0013").unwrap();
        assert_eq!(r.discount_smooth, expected_smooth);
    }

    // ─────────────────────────────────────────────────────────────────────
    // Property tests.
    //
    // WHY THIS MODULE NEEDS THEM MORE THAN ANY OTHER: every other function in
    // this crate is called by the ENGINE, where the inputs are bounded by
    // construction (alpha is config, ewma_prev is the engine's own prior,
    // prices come from validated sources). `rederive()` is the only entry point
    // fed a value an ADVERSARY CHOOSES — it parses `inputs_frozen` out of a
    // receipt handed to it by whoever ran `pegana-replay`. Its own doc comments
    // say so: "`ewma_prev`/`alpha` in the frozen inputs are untrusted and
    // unbounded (unlike the live engine, where they're always |x| < 1)".
    //
    // Yet before this it had 26 example-based tests and ZERO proptests, while
    // ewma.rs, discount.rs, thresholds.rs, transition.rs, canonical.rs and
    // receipt.rs all had them. The audit flagged exactly this.
    //
    // THE INVARIANT THAT MATTERS IS TOTALITY: for ANY input, `rederive` returns
    // Ok or a typed Err — never a panic. A panic here is a denial-of-service on
    // the public verifier CLI, reachable by handing someone a crafted receipt,
    // and it is the difference between "your receipt is invalid" and "the tool
    // that proves our honesty crashed".
    #[cfg(test)]
    mod proptests {
        use super::*;
        use proptest::prelude::*;

        /// Deliberately unbounded: huge magnitudes, negatives, and zero for
        /// every Decimal, because a receipt is untrusted input.
        fn arb_decimal() -> impl Strategy<Value = Decimal> {
            (-1_000_000_000i64..1_000_000_000i64, 0u32..12u32)
                .prop_map(|(m, scale)| Decimal::new(m, scale))
        }

        fn arb_state() -> impl Strategy<Value = PegState> {
            prop_oneof![
                Just(PegState::Pegged),
                Just(PegState::Drift),
                Just(PegState::Depeg),
                Just(PegState::Critical),
                Just(PegState::BlackSwan),
                Just(PegState::Unknown),
            ]
        }

        fn arb_class() -> impl Strategy<Value = AssetClass> {
            prop_oneof![
                Just(AssetClass::StableFiat),
                Just(AssetClass::StableCdp),
                Just(AssetClass::StableDn),
                Just(AssetClass::StableYield),
                Just(AssetClass::StableFx),
                Just(AssetClass::Lst),
                Just(AssetClass::SynthLev),
            ]
        }

        proptest! {
            #![proptest_config(ProptestConfig::with_cases(400))]

            /// TOTALITY. No input may panic — `rederive` must always return.
            /// This is the single most important property in the crate: the
            /// public `pegana-replay` CLI calls it on receipts it did not
            /// produce.
            #[test]
            fn never_panics_on_arbitrary_frozen_inputs(
                intrinsic in arb_decimal(),
                market in arb_decimal(),
                alpha in arb_decimal(),
                ewma_prev in proptest::option::of(arb_decimal()),
                hyusd_cr in proptest::option::of(arb_decimal()),
                prev in arb_state(),
                cand in proptest::option::of(arb_state()),
                class in arb_class(),
                cr_path in any::<bool>(),
                drift in 0u32..100_000,
                depeg in 0u32..100_000,
                critical in 0u32..100_000,
            ) {
                let mut inputs = base_inputs(class, if cr_path { "cr" } else { "bps" });
                inputs.intrinsic_usd = intrinsic;
                inputs.market_usd = market;
                inputs.alpha = alpha;
                inputs.ewma_prev = ewma_prev;
                inputs.hyusd_cr = hyusd_cr;
                inputs.previous_state = prev;
                inputs.candidate_state = cand;
                inputs.thresholds = HashMap::from([
                    ("drift".to_string(), drift),
                    ("depeg".to_string(), depeg),
                    ("critical".to_string(), critical),
                    ("drift_cr".to_string(), drift),
                    ("depeg_cr".to_string(), depeg),
                    ("critical_cr".to_string(), critical),
                    ("black_swan_cr".to_string(), 0),
                ]);

                // The assertion IS that this line returns rather than unwinds.
                let _ = rederive(&inputs);
            }

            /// DETERMINISM. Re-derivation is the product's core claim: the same
            /// frozen inputs must always reproduce the same verdict, byte for
            /// byte. A non-deterministic rederive would make every receipt
            /// unfalsifiable.
            #[test]
            fn is_deterministic(
                intrinsic in arb_decimal(),
                market in arb_decimal(),
                ewma_prev in proptest::option::of(arb_decimal()),
                class in arb_class(),
            ) {
                let mut inputs = base_inputs(class, "bps");
                inputs.intrinsic_usd = intrinsic;
                inputs.market_usd = market;
                inputs.ewma_prev = ewma_prev;

                prop_assert_eq!(rederive(&inputs), rederive(&inputs));
            }

            /// A zero intrinsic can never yield a verdict — it must be the
            /// typed error, never a divide-by-zero panic or a fabricated state.
            #[test]
            fn zero_intrinsic_always_errors(market in arb_decimal()) {
                let mut inputs = base_inputs(AssetClass::StableFiat, "bps");
                inputs.intrinsic_usd = Decimal::ZERO;
                inputs.market_usd = market;
                inputs.intrinsic_sol = None;
                inputs.market_sol = None;

                prop_assert_eq!(
                    rederive(&inputs),
                    Err(MethodologyRederiveError::IntrinsicZeroOrOverflow)
                );
            }

            /// The CR path with no `hyusd_cr` must be the typed error for EVERY
            /// input shape — never an unwrap on None.
            #[test]
            fn cr_path_without_cr_always_errors(
                intrinsic in arb_decimal(),
                market in arb_decimal(),
            ) {
                let mut inputs = base_inputs(AssetClass::StableCdp, "cr");
                inputs.thresholds = cr_thresholds();
                inputs.intrinsic_usd = intrinsic;
                inputs.market_usd = market;
                inputs.hyusd_cr = None;

                // Either the discount step rejects it first, or the CR step
                // does — but it is ALWAYS a typed Err, never a panic.
                prop_assert!(rederive(&inputs).is_err());
            }
        }
    }
}
