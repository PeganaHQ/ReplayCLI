//! Receipt and frozen-input types for audit persistence.

use chrono::{DateTime, Utc};
use pegana_common_verify::{AssetClass, PegState};
use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

/// `skip_serializing_if` predicate for the additive `intrinsic_stale` flag: a
/// `false` value is OMITTED from the canonical JCS, so every pre-existing /
/// gate-disabled receipt hashes byte-identically to before the field was added.
fn is_false(b: &bool) -> bool {
    !*b
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PythEntry {
    pub price: Decimal,
    pub confidence: Decimal,
    pub publish_time: DateTime<Utc>,
}

/// Frozen inputs captured at decision time and hashed into the receipt.
///
/// INVARIANT — NO FLOATS IN THE RECEIPT. Every numeric field here (and in
/// [`Computed`] / [`PythEntry`]) MUST be `rust_decimal::Decimal` /
/// `Option<Decimal>`, never `f64`/`f32`. The receipt is canonicalised (RFC 8785
/// JCS, see `canonical.rs`) then sha256'd; an `f64` serialises with
/// platform/locale-dependent shortest-round-trip formatting, so the *same* value
/// can hash differently across builds/arches. A divergent hash is then read as
/// "receipt does not match its inputs" and the alert is silently suppressed —
/// the exact missing-data failure mode ADR-0019 exists to prevent. Pinned by
/// `canonical::tests::receipt_hash_is_deterministic`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InputsFrozen {
    pub asset: String,
    pub class: AssetClass,
    pub now: DateTime<Utc>,
    pub alpha: Decimal,
    pub intrinsic_usd: Decimal,
    pub market_usd: Decimal,
    pub intrinsic_sol: Option<Decimal>,
    pub market_sol: Option<Decimal>,
    pub ewma_prev: Option<Decimal>,
    pub hyusd_cr: Option<Decimal>,
    pub previous_state: PegState,
    pub candidate_state: Option<PegState>,
    pub candidate_since: Option<DateTime<Utc>>,
    pub thresholds: HashMap<String, u32>,
    pub threshold_kind: String,
    pub pyth_entries: HashMap<String, PythEntry>,
    pub confirm_up_secs: i64,
    pub decay_down_secs: i64,
    /// Intrinsic-liveness gate (ADR-0038): the frozen "backing is stale" verdict
    /// at decision time — `flags.stale` for a sanctum_lst, or a stalled
    /// (`epoch_lag >= 2`) SPL stake-pool crank. When `true`, the methodology
    /// forces the state to UNKNOWN (a frozen-but-served backing must not read as
    /// a confident peg — the 2026-04-01 dSOL failure mode). The ENGINE only sets
    /// this when `INTRINSIC_LIVENESS_GATE` is enabled, so the gate ships DISABLED
    /// by default (depth-gate / ADR-0034 pattern): disabled → always `false` →
    /// omitted from the canonical hash (`skip_serializing_if`) → every existing
    /// receipt + the backtest golden are byte-identical, and the verdict is
    /// provably unchanged. Additive + serde-default so old receipts deserialize.
    #[serde(default, skip_serializing_if = "is_false")]
    pub intrinsic_stale: bool,
    /// Market-availability gate: the market source reported consecutive missing
    /// quotes, so the frozen market leg is a last-known value that must not
    /// support a confident peg. Forces UNKNOWN in the shared override. The
    /// engine only sets this when the dark-market gate is enabled; `false` is
    /// omitted from the canonical hash so every existing receipt stays
    /// byte-exact. Additive + serde-default so old receipts deserialize.
    #[serde(default, skip_serializing_if = "is_false")]
    pub market_unavailable: bool,
    /// Premium-confirmation gate (ADR-0047): the market leg is a PREMIUM beyond
    /// the drift band that no second source confirmed. A symmetric class is then
    /// capped at DRIFT; a direction-sensitive class holds a committed DEPEG+ band
    /// instead of relaxing on it. Set by the engine only when the gate is
    /// enabled; `false` is omitted from the canonical hash, so existing receipts
    /// are byte-exact. Additive + serde-default.
    #[serde(default, skip_serializing_if = "is_false")]
    pub premium_unconfirmed: bool,
    /// With `premium_unconfirmed`, the highest band a SYMMETRIC class may reach: the
    /// state the independent reference itself implies (never below DRIFT). `None`
    /// means no reference, so the cap is DRIFT. Omitted from the hash when `None`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub premium_cap: Option<PegState>,
}

/// Engine output frozen into the receipt. Same NO-FLOATS invariant as
/// [`InputsFrozen`]: `discount_*` are `Decimal`, never `f64`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Computed {
    pub discount_raw: Decimal,
    pub discount_smooth: Decimal,
    pub final_state: PegState,
    pub confidence_label: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Receipt {
    pub schema_version: String,
    pub methodology_version: String,
    pub methodology_git_sha: Option<String>,
    pub assets_toml_canonical: String,
    pub inputs_frozen: InputsFrozen,
    pub expected_computed: Computed,
    pub expected_receipt_sha256: String,
    /// Advisory, machine-readable reason for the current state — explains the
    /// "why" behind UNKNOWN / a premium / a skip. NOT part of the canonical
    /// hash (sibling of `schema_version`): old 0.4.0 receipts re-hash/re-derive
    /// identically. Tamper-EVIDENT only via the surrounding transport, not via
    /// the receipt hash.
    ///
    /// **Emitted today:**
    /// - `premium_sanity` — engine NAV-sanity override: smoothed discount exceeded
    ///   the premium-sanity bound, intrinsic anchor is suspect, state forced to
    ///   UNKNOWN (see `pegana_methodology::STATE_REASON_PREMIUM_SANITY`).
    /// - `discount_sanity`, `pyth_confidence_gate`, `intrinsic_liveness` — the
    ///   other engine override gates (see the matching `STATE_REASON_*` consts).
    /// - `market_unavailable` — engine dark-market gate (ADR-0046, opt-in): the
    ///   market source stopped reporting, state forced to UNKNOWN. The API layer
    ///   also synthesizes this string for a stale row (not in receipts).
    /// - `stale_source` — API layer: feed is >15 min stale, state collapsed to
    ///   UNKNOWN by the API (not in receipts; see `STATE_REASON_STALE_SOURCE`).
    ///
    /// **Reserved — not yet emitted (planned for depth-gate / oracle-crosscheck
    /// work):** `no_oracle_crosscheck | intrinsic_unavailable | shallow_market |
    /// nominal`.
    ///
    /// `None` on legacy receipts and rows where no reason applies (serde default).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub state_reason: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use pegana_common_verify::{AssetClass, PegState};
    use proptest::prelude::*;
    use rust_decimal::Decimal;
    use std::collections::HashMap;

    // ── Proptest ──────────────────────────────────────────────────────────────
    //
    // `InputsFrozen` and `Computed` are both pure data containers with no
    // custom `Deserialize` logic; all fields are either primitive, Decimal,
    // Option<Decimal>, PegState (serde-derived), or String. Generating
    // fully-arbitrary `InputsFrozen` values is straightforward because all types
    // already implement serde.

    fn sample_inputs(
        intrinsic_int: i64,
        market_int: i64,
        alpha_int: u32,
        prev_state: PegState,
    ) -> InputsFrozen {
        InputsFrozen {
            asset: "TEST".into(),
            class: AssetClass::StableFiat,
            now: chrono::DateTime::from_timestamp(1_704_067_200, 0).unwrap(),
            alpha: Decimal::new(alpha_int as i64, 2),
            intrinsic_usd: Decimal::new(intrinsic_int, 4),
            market_usd: Decimal::new(market_int, 4),
            intrinsic_sol: None,
            market_sol: None,
            ewma_prev: None,
            hyusd_cr: None,
            previous_state: prev_state,
            candidate_state: None,
            candidate_since: None,
            thresholds: HashMap::new(),
            threshold_kind: "bps".into(),
            pyth_entries: HashMap::new(),
            confirm_up_secs: 30,
            decay_down_secs: 120,
            intrinsic_stale: false,
            market_unavailable: false,
            premium_unconfirmed: false,
            premium_cap: None,
        }
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
        /// SERDE ROUND-TRIP: serialize `InputsFrozen` → JSON → deserialize must
        /// yield a structurally identical value (all Decimal/Option fields
        /// compare with PartialEq via serde_json's round-trip).
        ///
        /// This guards the NO-FLOATS invariant documented on `InputsFrozen`:
        /// Decimal serialises as a string so there is no platform-specific
        /// float representation that could change between encode and decode.
        #[test]
        fn inputs_frozen_serde_round_trip(
            intrinsic_int in 1i64..=1_000_000i64,
            market_int    in 1i64..=1_000_000i64,
            alpha_int     in 1u32..=99u32,
            prev_state in arb_peg_state(),
        ) {
            let original = sample_inputs(intrinsic_int, market_int, alpha_int, prev_state);
            let json = serde_json::to_string(&original).expect("serialize InputsFrozen");
            let restored: InputsFrozen =
                serde_json::from_str(&json).expect("deserialize InputsFrozen");

            // Field-by-field comparison (InputsFrozen does not derive PartialEq
            // because HashMap<String, PythEntry> contains a non-PartialEq type
            // in general; we compare the fields that matter for the hash).
            prop_assert_eq!(
                original.alpha, restored.alpha,
                "alpha changed in round-trip"
            );
            prop_assert_eq!(
                original.intrinsic_usd, restored.intrinsic_usd,
                "intrinsic_usd changed in round-trip"
            );
            prop_assert_eq!(
                original.market_usd, restored.market_usd,
                "market_usd changed in round-trip"
            );
            prop_assert_eq!(
                original.previous_state, restored.previous_state,
                "previous_state changed in round-trip"
            );
            prop_assert_eq!(
                original.confirm_up_secs, restored.confirm_up_secs,
                "confirm_up_secs changed in round-trip"
            );
            prop_assert_eq!(
                original.decay_down_secs, restored.decay_down_secs,
                "decay_down_secs changed in round-trip"
            );
        }

        /// COMPUTED SERDE ROUND-TRIP: same guarantee for `Computed`.
        #[test]
        fn computed_serde_round_trip(
            raw_int in -999_999i64..=999_999i64,
            smooth_int in -999_999i64..=999_999i64,
            state in arb_peg_state(),
        ) {
            let original = Computed {
                discount_raw:    Decimal::new(raw_int, 6),
                discount_smooth: Decimal::new(smooth_int, 6),
                final_state:     state,
                confidence_label: "high".into(),
            };
            let json = serde_json::to_string(&original).expect("serialize Computed");
            let restored: Computed =
                serde_json::from_str(&json).expect("deserialize Computed");

            prop_assert_eq!(
                original.discount_raw, restored.discount_raw,
                "discount_raw changed in round-trip"
            );
            prop_assert_eq!(
                original.discount_smooth, restored.discount_smooth,
                "discount_smooth changed in round-trip"
            );
            prop_assert_eq!(
                original.final_state, restored.final_state,
                "final_state changed in round-trip"
            );
        }
    }
}
