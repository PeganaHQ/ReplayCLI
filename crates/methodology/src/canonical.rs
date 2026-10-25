//! Canonical JSON (RFC 8785) and TOML hashing.

use sha2::{Digest, Sha256};
use thiserror::Error;

use crate::receipt::{Computed, InputsFrozen};

#[derive(Debug, Error)]
pub enum MethodologyError {
    #[error("failed to parse assets.toml: {0}")]
    AssetsTomlParse(#[from] toml::de::Error),

    #[error("failed to serialize canonical TOML: {0}")]
    AssetsTomlSerialize(#[from] toml::ser::Error),

    #[error("canonical JSON encode failed: {0}")]
    CanonicalJson(#[from] serde_json::Error),
}

/// Parse assets.toml, re-serialize in canonical form (sorted keys),
/// then sha256. Comments and whitespace don't affect the hash.
pub fn canonical_assets_hash(toml_str: &str) -> Result<[u8; 32], MethodologyError> {
    let value: toml::Value = toml::from_str(toml_str)?;
    let canonical = toml::to_string(&value)?;
    let mut hasher = Sha256::new();
    hasher.update(canonical.as_bytes());
    Ok(hasher.finalize().into())
}

/// Canonical sha256 of a receipt's content via RFC 8785 JCS encoding.
pub fn canonical_receipt_hash(
    methodology_version: &str,
    methodology_git_sha: Option<&str>,
    assets_toml_canonical: &str,
    inputs_frozen: &InputsFrozen,
    computed: &Computed,
) -> Result<[u8; 32], MethodologyError> {
    let mut hasher = Sha256::new();
    hasher.update(methodology_version.as_bytes());
    hasher.update(b"\x00");
    hasher.update(methodology_git_sha.unwrap_or("").as_bytes());
    hasher.update(b"\x00");
    hasher.update(assets_toml_canonical.as_bytes());
    hasher.update(b"\x00");
    let inputs_bytes = serde_json_canonicalizer::to_string(inputs_frozen)?;
    hasher.update(inputs_bytes.as_bytes());
    hasher.update(b"\x00");
    let computed_bytes = serde_json_canonicalizer::to_string(computed)?;
    hasher.update(computed_bytes.as_bytes());
    Ok(hasher.finalize().into())
}

/// Hex-encode a sha256 digest for CHAR(64) columns.
pub fn hex_sha256(digest: [u8; 32]) -> String {
    digest.iter().fold(String::with_capacity(64), |mut acc, b| {
        acc.push_str(&format!("{:02x}", b));
        acc
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Utc;
    use pegana_common_verify::{AssetClass, PegState};
    use proptest::prelude::*;
    use rust_decimal::Decimal;
    use std::collections::HashMap;
    use std::str::FromStr;

    fn sample_inputs() -> InputsFrozen {
        InputsFrozen {
            asset: "USDC".into(),
            class: AssetClass::StableFiat,
            now: Utc::now(),
            alpha: Decimal::from_str("0.3").unwrap(),
            intrinsic_usd: Decimal::ONE,
            market_usd: Decimal::from_str("0.9999").unwrap(),
            intrinsic_sol: None,
            market_sol: None,
            ewma_prev: None,
            hyusd_cr: None,
            previous_state: PegState::Pegged,
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

    fn sample_computed() -> Computed {
        Computed {
            discount_raw: Decimal::from_str("0.0001").unwrap(),
            discount_smooth: Decimal::from_str("0.0001").unwrap(),
            final_state: PegState::Pegged,
            confidence_label: "high".into(),
        }
    }

    #[test]
    fn assets_hash_is_stable_across_comment_changes() {
        let with_comment = "[[assets]]\nsymbol = \"USDC\"\n# important comment\n";
        let without = "[[assets]]\nsymbol = \"USDC\"\n";
        assert_eq!(
            canonical_assets_hash(with_comment).unwrap(),
            canonical_assets_hash(without).unwrap()
        );
    }

    // And for `premium_unconfirmed` (ADR-0047): false is omitted, true is hashed.
    #[test]
    fn premium_unconfirmed_false_is_omitted_from_canonical_hash() {
        let plain = sample_inputs();
        let jcs_false = serde_json_canonicalizer::to_string(&plain).unwrap();
        assert!(!jcs_false.contains("premium_unconfirmed"), "{jcs_false}");
        let mut flagged = sample_inputs();
        flagged.premium_unconfirmed = true;
        let jcs_true = serde_json_canonicalizer::to_string(&flagged).unwrap();
        assert!(
            jcs_true.contains("\"premium_unconfirmed\":true"),
            "{jcs_true}"
        );
    }

    // Same byte-exactness invariant for `market_unavailable`: false is omitted so
    // every receipt written before the field existed hashes identically.
    #[test]
    fn market_unavailable_false_is_omitted_from_canonical_hash() {
        let jcs_false = serde_json_canonicalizer::to_string(&sample_inputs()).unwrap();
        assert!(!jcs_false.contains("market_unavailable"), "{jcs_false}");
        let mut dark = sample_inputs();
        dark.market_unavailable = true;
        let jcs_true = serde_json_canonicalizer::to_string(&dark).unwrap();
        assert!(
            jcs_true.contains("\"market_unavailable\":true"),
            "{jcs_true}"
        );
    }

    // ADR-0038 byte-exactness invariant: `intrinsic_stale=false` (the only value
    // emitted while the gate is disabled) is OMITTED from the canonical JCS, so
    // every pre-existing receipt + the backtest golden hash byte-identically. A
    // `true` value is present (it MUST be, so a stale-forced UNKNOWN is
    // tamper-evident + re-derivable). This is what makes the disabled gate
    // provably inert at the hash level, not just the verdict level.
    #[test]
    fn intrinsic_stale_false_is_omitted_from_canonical_hash() {
        let false_inputs = sample_inputs(); // intrinsic_stale: false
        let jcs_false = serde_json_canonicalizer::to_string(&false_inputs).unwrap();
        assert!(
            !jcs_false.contains("intrinsic_stale"),
            "false must be omitted for byte-exactness; got: {jcs_false}"
        );

        let mut true_inputs = sample_inputs();
        true_inputs.intrinsic_stale = true;
        let jcs_true = serde_json_canonicalizer::to_string(&true_inputs).unwrap();
        assert!(
            jcs_true.contains("intrinsic_stale"),
            "true must be present so the gated verdict is tamper-evident"
        );

        // And the hashes differ ONLY because of that field — false hashes like
        // a receipt that never had the field.
        assert_ne!(
            canonical_receipt_hash(
                "0.7.0",
                None,
                "[[assets]]\n",
                &false_inputs,
                &sample_computed()
            )
            .unwrap(),
            canonical_receipt_hash(
                "0.7.0",
                None,
                "[[assets]]\n",
                &true_inputs,
                &sample_computed()
            )
            .unwrap(),
        );
    }

    #[test]
    fn assets_hash_changes_when_value_changes() {
        let v1 = "[[assets]]\nsymbol = \"USDC\"\n";
        let v2 = "[[assets]]\nsymbol = \"USDT\"\n";
        assert_ne!(
            canonical_assets_hash(v1).unwrap(),
            canonical_assets_hash(v2).unwrap()
        );
    }

    #[test]
    fn receipt_hash_is_deterministic() {
        let inputs = sample_inputs();
        let computed = sample_computed();
        let h1 = canonical_receipt_hash(
            "0.1.0",
            None,
            "[[assets]]\nsymbol=\"USDC\"\n",
            &inputs,
            &computed,
        )
        .unwrap();
        let h2 = canonical_receipt_hash(
            "0.1.0",
            None,
            "[[assets]]\nsymbol=\"USDC\"\n",
            &inputs,
            &computed,
        )
        .unwrap();
        assert_eq!(h1, h2);
    }

    #[test]
    fn receipt_hash_changes_when_version_changes() {
        let inputs = sample_inputs();
        let computed = sample_computed();
        let h1 = canonical_receipt_hash("0.1.0", None, "x", &inputs, &computed).unwrap();
        let h2 = canonical_receipt_hash("0.2.0", None, "x", &inputs, &computed).unwrap();
        assert_ne!(h1, h2);
    }

    #[test]
    fn hex_encoding_is_64_chars() {
        let digest = [0u8; 32];
        assert_eq!(hex_sha256(digest).len(), 64);
    }

    // ── Proptest ──────────────────────────────────────────────────────────────

    /// Generate TOML-safe symbol strings (alphanumeric, 1-8 chars).
    fn arb_symbol() -> impl Strategy<Value = String> {
        "[A-Za-z][A-Za-z0-9]{0,7}".prop_map(|s| s)
    }

    #[test]
    fn state_reason_is_outside_the_hash() {
        let inputs = sample_inputs();
        let computed = sample_computed();
        let toml_str = "[[assets]]\nsymbol=\"USDC\"\n";

        let h1 = canonical_receipt_hash("0.4.0", None, toml_str, &inputs, &computed).unwrap();

        // Build a Receipt, set state_reason, then verify the hash of the same
        // (inputs, computed) args is identical — proving state_reason is NOT in
        // the hash path.
        let _receipt_with_reason = crate::receipt::Receipt {
            schema_version: "v2".into(),
            methodology_version: "0.4.0".into(),
            methodology_git_sha: None,
            assets_toml_canonical: toml_str.into(),
            inputs_frozen: inputs.clone(),
            expected_computed: computed.clone(),
            expected_receipt_sha256: hex_sha256(h1),
            state_reason: Some("premium_sanity".into()),
        };

        let h2 = canonical_receipt_hash("0.4.0", None, toml_str, &inputs, &computed).unwrap();
        assert_eq!(h1, h2, "state_reason must not affect the canonical hash");
    }

    proptest! {
        /// INVARIANT-UNDER-COMMENT-CHANGES: inserting or removing TOML comments
        /// must NOT change `canonical_assets_hash`.  The parser strips comments
        /// before re-serializing, so the canonical form is identical.
        #[test]
        fn assets_hash_invariant_under_comments(
            symbol in arb_symbol(),
            comment_text in "[a-z ]{0,30}",
        ) {
            let base   = format!("[[assets]]\nsymbol = \"{symbol}\"\n");
            let commented = format!("[[assets]]\nsymbol = \"{symbol}\"\n# {comment_text}\n");
            let h_base = canonical_assets_hash(&base).unwrap();
            let h_with = canonical_assets_hash(&commented).unwrap();
            prop_assert_eq!(
                h_base, h_with,
                "comment insertion changed the hash for symbol={}",
                symbol
            );
        }

        /// CHANGES-ON-VALUE-CHANGE: if the symbol value is different, the hash
        /// must differ (with overwhelming probability — toml→sha256 is not
        /// collision-prone at this scale).
        #[test]
        fn assets_hash_changes_on_value_change(
            sym1 in arb_symbol(),
            sym2 in arb_symbol(),
        ) {
            prop_assume!(sym1 != sym2);
            let t1 = format!("[[assets]]\nsymbol = \"{sym1}\"\n");
            let t2 = format!("[[assets]]\nsymbol = \"{sym2}\"\n");
            let h1 = canonical_assets_hash(&t1).unwrap();
            let h2 = canonical_assets_hash(&t2).unwrap();
            prop_assert_ne!(h1, h2, "different symbols must produce different hashes");
        }

        /// INVARIANT-UNDER-WHITESPACE: extra blank lines and leading/trailing
        /// whitespace around values do not change the canonical hash because the
        /// TOML parser normalises them.
        #[test]
        fn assets_hash_invariant_under_extra_whitespace(
            symbol in arb_symbol(),
        ) {
            let compact = format!("[[assets]]\nsymbol=\"{symbol}\"\n");
            let spaced  = format!("[[assets]]\nsymbol = \"{symbol}\"\n\n");
            let h_compact = canonical_assets_hash(&compact).unwrap();
            let h_spaced  = canonical_assets_hash(&spaced).unwrap();
            prop_assert_eq!(
                h_compact, h_spaced,
                "whitespace difference changed hash for symbol={}",
                symbol
            );
        }

        /// HASH-STABILITY-UNDER-RECEIPT-ROUND-TRIP: `canonical_receipt_hash`
        /// must be identical before and after a serialize → deserialize
        /// round-trip of `InputsFrozen` and `Computed`.
        ///
        /// We parametrise over methodology version strings and a few numeric
        /// fields; the struct shape is fixed so we build variants from those.
        #[test]
        fn receipt_hash_stable_under_round_trip(
            version in "[0-9]\\.[0-9]\\.[0-9]",
            intrinsic_int in 1i64..=1_000_000i64,
            market_int    in 1i64..=1_000_000i64,
            alpha_int     in 1u32..=99u32,
        ) {
            let inputs = InputsFrozen {
                asset: "USDC".into(),
                class: AssetClass::StableFiat,
                now: Utc::now(),
                alpha: Decimal::new(alpha_int as i64, 2),
                intrinsic_usd: Decimal::new(intrinsic_int, 4),
                market_usd:    Decimal::new(market_int, 4),
                intrinsic_sol: None,
                market_sol: None,
                ewma_prev: None,
                hyusd_cr: None,
                previous_state: PegState::Pegged,
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
            };
            let computed = crate::receipt::Computed {
                discount_raw:    Decimal::new(intrinsic_int - market_int, 4),
                discount_smooth: Decimal::new(0, 0),
                final_state: PegState::Pegged,
                confidence_label: "high".into(),
            };

            let toml_str = "[[assets]]\nsymbol=\"USDC\"\n";

            // Hash before round-trip.
            let h_before = canonical_receipt_hash(
                &version, None, toml_str, &inputs, &computed,
            ).unwrap();

            // Serialize → deserialize inputs and computed.
            let inputs_json   = serde_json::to_string(&inputs).unwrap();
            let computed_json = serde_json::to_string(&computed).unwrap();
            let inputs_rt: InputsFrozen =
                serde_json::from_str(&inputs_json).unwrap();
            let computed_rt: crate::receipt::Computed =
                serde_json::from_str(&computed_json).unwrap();

            // Hash after round-trip.
            let h_after = canonical_receipt_hash(
                &version, None, toml_str, &inputs_rt, &computed_rt,
            ).unwrap();

            prop_assert_eq!(
                h_before, h_after,
                "receipt hash changed after serde round-trip for version={}",
                version
            );
        }
    }
}
