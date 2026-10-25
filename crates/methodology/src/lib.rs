//! pegana-methodology — pure functions that turn raw inputs into peg-state decisions.
//!
//! Every function is deterministic and side-effect free. Engine calls them at
//! runtime; pegana-replay-cli calls the same functions offline to verify alerts.
//! No tokio, no I/O, no global state.

/// EWMA smoothing factor, confirm-up and decay-down windows every receipt must
/// carry. They have been fixed since the first release; the verifier binds a
/// receipt's frozen values to them so a forged receipt cannot choose its own.
/// The engine and the backtest runner use these directly. Changing any of them
/// REQUIRES a methodology version bump.
pub const RECEIPT_EWMA_ALPHA: &str = "0.3";
pub const RECEIPT_CONFIRM_UP_SECS: i64 = 30;
pub const RECEIPT_DECAY_DOWN_SECS: i64 = 120;

/// Engine-emitted `state_reason` values for the market-confirmation gates
/// (ADR-0047). Both force UNKNOWN through the dark-market path.
pub const STATE_REASON_MARKET_REFERENCE_DIVERGENCE: &str = "market_reference_divergence";
pub const STATE_REASON_PREMIUM_HOLD_TIMEOUT: &str = "premium_hold_timeout";
pub const STATE_REASON_DEPTH_BELOW_FLOOR: &str = "depth_below_floor";

/// Emitted by the engine when the NAV-sanity check overrides the verdict
/// (smoothed discount exceeded the premium-sanity bound; intrinsic anchor
/// suspect; state forced to UNKNOWN). Mirrors `Receipt.state_reason` and
/// `discount_snapshots.state_reason`. The API SQL uses the string literal
/// `'premium_sanity'` which must stay identical to this const.
pub const STATE_REASON_PREMIUM_SANITY: &str = "premium_sanity";

pub const STATE_REASON_DISCOUNT_SANITY: &str = "discount_sanity";

/// Emitted by the engine when the Pyth confidence-gate override forces the
/// verdict to UNKNOWN (a cached Pyth reading's confidence/price ratio exceeded
/// `PYTH_CONFIDENCE_GATE_BPS`). Mirrors `Receipt.state_reason` and
/// `discount_snapshots.state_reason`, same shape as
/// `STATE_REASON_PREMIUM_SANITY`/`STATE_REASON_DISCOUNT_SANITY`. See ADR-0032.
pub const STATE_REASON_PYTH_CONFIDENCE_GATE: &str = "pyth_confidence_gate";

/// Synthesized by the API layer (not the engine) when the most-recent
/// discount snapshot is older than 15 minutes (stale feed). The state is
/// collapsed from the stored value to UNKNOWN. The API SQL uses the string
/// literal `'stale_source'` which must stay identical to this const.
/// Not present in engine receipts — the engine stops writing when data is
/// absent, so a stale row implies the engine saw a dead feed, not a decision.
pub const STATE_REASON_STALE_SOURCE: &str = "stale_source";

/// The market leg is dark. Two producers share this string:
/// - the API layer synthesizes it instead of [`STATE_REASON_STALE_SOURCE`] when
///   there is no fresh discount snapshot but the intrinsic leg IS fresh and no
///   market snapshot exists in the same 15-min window (the API SQL uses the
///   literal `'market_unavailable'`; not in receipts);
/// - the engine emits it, and freezes `InputsFrozen.market_unavailable`, when
///   the dark-market gate (ADR-0046, `DARK_MARKET_GATE_ENABLED`) forces UNKNOWN.
///   That path IS in receipts and re-derives.
pub const STATE_REASON_MARKET_UNAVAILABLE: &str = "market_unavailable";

/// Emitted by the engine when the intrinsic-liveness gate (ADR-0038) forces the
/// verdict to UNKNOWN — the receipt froze `intrinsic_stale = true` (a sanctum_lst
/// the directory flags `stale`, or a stalled SPL stake-pool crank). Only present
/// on receipts emitted with `INTRINSIC_LIVENESS_GATE` enabled; the gate ships
/// disabled by default. Same shape as the other override reasons.
pub const STATE_REASON_INTRINSIC_LIVENESS: &str = "intrinsic_liveness";

pub mod canonical;
pub mod discount;
pub mod ewma;
pub mod receipt;
pub mod rederive;
pub mod thresholds;
pub mod transition;
pub mod version;

pub use canonical::{canonical_assets_hash, canonical_receipt_hash, hex_sha256, MethodologyError};
pub use discount::{compute_discount, is_plausible_discount_sample, uses_sol_path};
pub use ewma::apply_ewma_pure;
pub use receipt::{Computed, InputsFrozen, PythEntry, Receipt};
pub use rederive::{rederive, MethodologyRederiveError, Rederived};
pub use thresholds::{
    classify_cr_with_hysteresis, classify_with_hysteresis, confirm_premium_candidate,
    depeg_band_bps, discount_sanity_violated, intrinsic_liveness_gate_violated,
    is_direction_sensitive, market_unavailable_applies, next_worse_cr_band, override_final_state,
    override_final_state_attributed, premium_confirmation_floor_bps,
    premium_confirmation_tolerance_bps, premium_sanity_violated, pyth_confidence_gate_violated,
    state_for_bps_discount, state_for_bps_discount_aware, state_for_cr, validate_thresholds,
    OverrideReason, CR_DEADBAND_PCT, DEADBAND_PCT, NAV_DISCOUNT_SANITY_BPS, NAV_PREMIUM_SANITY_BPS,
    PYTH_CONFIDENCE_GATE_BPS,
};
pub use transition::{transition_decide, TransitionDecision};
pub use version::{methodology_git_sha, methodology_version};
