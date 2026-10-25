//! Discount computation and plausibility checks.

use pegana_common_verify::AssetClass;
use rust_decimal::Decimal;

/// Whether the discount is computed in SOL terms (the SOL/USD multiplier
/// cancels): an LST with both SOL legs present and a non-zero SOL intrinsic. The
/// single predicate shared by [`compute_discount`] and the engine's Pyth gate.
pub fn uses_sol_path(
    class: AssetClass,
    intrinsic_sol: Option<Decimal>,
    market_sol: Option<Decimal>,
) -> bool {
    matches!(class, AssetClass::Lst)
        && market_sol.is_some()
        && intrinsic_sol.is_some_and(|i| !i.is_zero())
}

/// Compute signed discount `1 - market/intrinsic`.
///
/// For LST class we prefer the SOL-denominated path when both sides have
/// published a SOL value: `intrinsic_usd` and `market_usd` are both
/// `pyth(SOL/USD) × …` and Sanctum vs Jupiter may have cached different
/// SOL/USD snapshots — computing in SOL cancels the multiplier. Stables /
/// yield / FX assets stay on the USD path because the USD value IS the
/// invariant for those.
///
/// Returns `None` when `intrinsic` is zero/absent — there is no economic
/// reading to report, and a zero/absent intrinsic must NOT be laundered into
/// `discount = 0`, which the state machine would read as a confirmed healthy
/// peg. The engine treats `None` as "no signal · skip publish" (hardening H8).
pub fn compute_discount(
    intrinsic: Decimal,
    market: Decimal,
    intrinsic_sol: Option<Decimal>,
    market_sol: Option<Decimal>,
    class: AssetClass,
) -> Option<Decimal> {
    if intrinsic.is_zero() {
        return None;
    }
    if uses_sol_path(class, intrinsic_sol, market_sol) {
        if let (Some(i_sol), Some(m_sol)) = (intrinsic_sol, market_sol) {
            // Checked: a near-zero `i_sol` overflows Decimal on divide, which
            // rust_decimal's `/` PANICS on — inline in the engine loop, that
            // kills the whole process (audit F-12). None = skip (H8), never crash.
            return m_sol
                .checked_div(i_sol)
                .and_then(|q| Decimal::ONE.checked_sub(q));
        }
    }
    // Checked: a micro-but-nonzero `intrinsic` (e.g. a Hylo CR<1 xSOL NAV ≈ 1e-27
    // from a free-collateral rounding residual) makes `market / intrinsic` overflow
    // Decimal::MAX, which rust_decimal's `/` operator PANICS on. `try_recompute`
    // runs inline in the engine select loop with no catch_unwind, so that panic
    // exits the process (crash-loop = monitoring blackout during a depeg, audit
    // F-12). None routes to the existing "no signal · skip publish" path (H8).
    market
        .checked_div(intrinsic)
        .and_then(|q| Decimal::ONE.checked_sub(q))
}

/// Plausibility filter for raw discount samples.
///
/// `|discount| >= 1.0` would mean market trades at **0× or less** (a $0 / total
/// loss) or at **2× or more** of intrinsic — not economically possible for any
/// tracked class. A real depeg always leaves the asset worth *something* (market
/// strictly between $0 and 2× peg, i.e. `|discount| < 1.0`), so it stays
/// plausible and keeps alerting through the normal bands. Filtering the `>= 1.0`
/// boundary keeps a degenerate quote (e.g. a Jupiter route returning
/// `out_amount = 0` → discount = exactly `1.0`) from contaminating the smoothed
/// `discount` for the next ~7 buckets. The bound is **strict** (`<`): the
/// inclusive `<=` historically let a single `$0` tick paint JupUSD CRITICAL for
/// ~7.5 min (2026-06-12 incident).
///
/// Caller (engine `try_recompute`) skips the EWMA update on `false` so the
/// previous smoothed value stays put; the next sane sample resumes the
/// pipeline. No publish, no alert — we never propagate the lie.
pub fn is_plausible_discount_sample(d: Decimal) -> bool {
    d.abs() < Decimal::ONE
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;
    use std::str::FromStr;

    #[test]
    fn zero_intrinsic_returns_none() {
        let d = compute_discount(
            Decimal::ZERO,
            Decimal::ONE,
            None,
            None,
            AssetClass::StableFiat,
        );
        assert_eq!(
            d, None,
            "zero intrinsic must NOT confirm a peg (discount 0)"
        );
    }

    /// Regression (audit 2026-06-19, F-12): a micro-but-nonzero intrinsic —
    /// e.g. an xSOL NAV ≈ 1e-27 produced by a Hylo hyUSD undercollateralization
    /// (CR<1) free-collateral rounding residual — makes `market / intrinsic`
    /// overflow `Decimal::MAX`, which rust_decimal's `/` operator PANICS on.
    /// `try_recompute` runs inline in the engine select loop with no
    /// catch_unwind, so that panic would crash-loop the whole engine (no
    /// monitoring for ANY asset) during exactly the depeg event the product
    /// exists to catch. The division must be checked → None (skip), never panic.
    #[test]
    fn micro_nonzero_intrinsic_returns_none_not_panic() {
        let intrinsic = Decimal::new(1, 27); // 1e-27, survives is_zero()
        let market = Decimal::from(200); // a plausible xSOL USD price
                                         // Before the fix this line panics with "Division overflowed".
        let d = compute_discount(intrinsic, market, None, None, AssetClass::SynthLev);
        assert_eq!(d, None, "overflowing discount must skip (None), not panic");
        // Same on the LST SOL-denominated path: m_sol / i_sol = 200 / 1e-27 =
        // 2e29 > Decimal::MAX → overflow → must skip, not panic.
        let d_sol = compute_discount(
            Decimal::from(200),
            Decimal::from(200),
            Some(Decimal::new(1, 27)),
            Some(Decimal::from(200)),
            AssetClass::Lst,
        );
        assert_eq!(
            d_sol, None,
            "overflowing SOL-path discount must skip, not panic"
        );
    }

    #[test]
    fn stable_uses_usd_path() {
        let d = compute_discount(
            Decimal::from_str("1.00").unwrap(),
            Decimal::from_str("0.99").unwrap(),
            None,
            None,
            AssetClass::StableFiat,
        );
        assert_eq!(d, Some(Decimal::from_str("0.01").unwrap()));
    }

    #[test]
    fn lst_prefers_sol_path_when_both_present() {
        // USD path would say drift (multiplier race), SOL path says 0.
        let intrinsic_usd: Decimal = "111.7".parse().unwrap();
        let market_usd: Decimal = "112.57".parse().unwrap();
        let i_sol: Decimal = "1.117".parse().unwrap();
        let m_sol: Decimal = "1.117".parse().unwrap();
        let d = compute_discount(
            intrinsic_usd,
            market_usd,
            Some(i_sol),
            Some(m_sol),
            AssetClass::Lst,
        );
        assert_eq!(d, Some(Decimal::ZERO));
    }

    #[test]
    fn lst_falls_back_to_usd_when_sol_missing() {
        let d = compute_discount(
            Decimal::from_str("1.10").unwrap(),
            Decimal::from_str("1.09").unwrap(),
            None,
            None,
            AssetClass::Lst,
        )
        .expect("non-zero intrinsic yields Some");
        assert!(
            (d - Decimal::from_str("0.0090909").unwrap()).abs()
                < Decimal::from_str("0.0001").unwrap()
        );
    }

    #[test]
    fn plausibility_rejects_huge_samples() {
        assert!(!is_plausible_discount_sample(
            Decimal::from_str("-1160.47").unwrap()
        ));
        assert!(!is_plausible_discount_sample(
            Decimal::from_str("2.5").unwrap()
        ));
        assert!(is_plausible_discount_sample(
            Decimal::from_str("0.0024").unwrap()
        ));
    }

    /// Regression (2026-06-12): a degenerate market quote of $0 — a Jupiter
    /// route returning `out_amount = 0` — makes `compute_discount` return
    /// exactly `1.0` (a 100% "discount"). No asset trades at $0, so this is
    /// missing data, not a total depeg, and must be rejected BEFORE the EWMA.
    /// The inclusive `<= 1.0` bound let `1.0` through, poisoning the α=0.3
    /// average and painting JupUSD CRITICAL for ~7.5 min off a single tick.
    #[test]
    fn plausibility_rejects_total_loss_and_doubling_boundary() {
        // market = $0      → discount = +1.0  (impossible)
        assert!(!is_plausible_discount_sample(Decimal::ONE));
        // market = 2× peg  → discount = -1.0  (impossible)
        assert!(!is_plausible_discount_sample(
            Decimal::from_str("-1").unwrap()
        ));
        // A severe-but-REAL depeg (market still > $0) stays plausible so the
        // normal bands keep alerting on it — we only reject the impossible.
        assert!(is_plausible_discount_sample(
            Decimal::from_str("0.97").unwrap()
        ));
        assert!(is_plausible_discount_sample(
            Decimal::from_str("-0.97").unwrap()
        ));
    }

    // ── Proptest ──────────────────────────────────────────────────────────────

    /// Build an `AssetClass` strategy — all 8 variants.
    fn arb_asset_class() -> impl Strategy<Value = pegana_common_verify::AssetClass> {
        use pegana_common_verify::AssetClass;
        prop_oneof![
            Just(AssetClass::Lst),
            Just(AssetClass::StableFiat),
            Just(AssetClass::StableCdp),
            Just(AssetClass::StableRwa),
            Just(AssetClass::StableDn),
            Just(AssetClass::StableFx),
            Just(AssetClass::SynthLev),
            Just(AssetClass::StableYield),
        ]
    }

    proptest! {
        /// BOUNDARY: `is_plausible_discount_sample(d)` is equivalent to
        /// `d.abs() < Decimal::ONE` for all values in [-2, 2], including the
        /// exact boundaries ±1.0 (regression for the 2026-06-12 JupUSD incident
        /// where `out_amount = 0` → discount exactly 1.0 was let through).
        #[test]
        fn plausibility_boundary_swept(
            // Generate discount in multiples of 0.001 over [-2.0, 2.0]
            // (4000 distinct values, covers ±1.0 exactly).
            hundredths in -2000i64..=2000i64,
        ) {
            let d = Decimal::new(hundredths, 3); // precision 3 → hundredths/1000
            let expected = d.abs() < Decimal::ONE;
            prop_assert_eq!(
                is_plausible_discount_sample(d),
                expected,
                "d = {} — expected plausible={} but got {}",
                d, expected, is_plausible_discount_sample(d)
            );
        }

        /// NONE-NEVER-LAUNDERED: `compute_discount` must never return
        /// `Some(x)` where `|x| >= 1` for any plausible-looking inputs.
        ///
        /// Note: `compute_discount` itself does NOT filter by
        /// `is_plausible_discount_sample`; that guard lives in the engine
        /// (`try_recompute`). However, a discount of magnitude >= 1 can only
        /// arise when `market / intrinsic >= 2` or `market <= 0`. Within the
        /// domain where `intrinsic > 0` (otherwise None is returned) and
        /// `market` is in [0, 2 × intrinsic), the output is always plausible.
        /// This property asserts that contract: for `market` in
        /// `(0, 2×intrinsic)` the result is `Some(x)` with `|x| < 1`.
        #[test]
        fn compute_discount_none_never_laundered(
            // Use integer multiples to avoid Decimal precision issues.
            // intrinsic in [1, 10_000], market in [1, 2×intrinsic − 1]
            intrinsic_units in 1i64..=10_000i64,
            // market expressed as fraction of intrinsic in (0, 2):
            // numer / denom where numer/denom ∈ (0, 2)
            market_numer in 1u32..=19_999u32, // <2×10000 scaled
        ) {
            // market_numer is in (0, 20000); interpret as (market_numer/10000) × intrinsic.
            // So market ∈ (0, 2×intrinsic).
            let intrinsic = Decimal::new(intrinsic_units, 0);
            // market = market_numer / 10000 × intrinsic
            let market = Decimal::new(market_numer as i64, 0) * intrinsic
                / Decimal::new(10_000, 0);

            // Use StableFiat so we always take the USD path.
            let d = compute_discount(
                intrinsic, market, None, None,
                pegana_common_verify::AssetClass::StableFiat,
            );
            let result = d.expect("non-zero intrinsic → Some");
            prop_assert!(
                result.abs() < Decimal::ONE,
                "discount = {} (market={} intrinsic={}) — magnitude >= 1 must never be returned",
                result, market, intrinsic
            );
        }

        /// ZERO-INTRINSIC-ALWAYS-NONE: no matter the market / class / sol
        /// values, a zero intrinsic must never return Some.
        #[test]
        fn zero_intrinsic_always_none(
            market_int in -100_000i64..=100_000i64,
            class in arb_asset_class(),
        ) {
            let market = Decimal::new(market_int, 4);
            let result = compute_discount(
                Decimal::ZERO, market, None, None, class,
            );
            prop_assert_eq!(result, None, "zero intrinsic must always return None");
        }
    }
}
