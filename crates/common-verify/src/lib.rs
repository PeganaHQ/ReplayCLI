//! Public, verifier-safe types for Pegana.
//!
//! This crate is the public-facing carve-out of the original `pegana-common`.
//! It contains only the types needed to:
//!
//!   1. Parse the canonical `assets.toml` config (asset definitions,
//!      thresholds, peg targets, intrinsic + market strategies, Pyth
//!      staleness windows, polling cadences).
//!   2. Deserialize the `AlertEvidence` record the public API returns
//!      under `/v1/audit/:alert_id`.
//!   3. Enumerate the closed set of peg states the methodology can produce.
//!
//! Crates outside of `pegana-replay` (engine, dispatcher, API, indexer, bot)
//! re-export this through `pegana_common::*` and add server-side concerns on
//! top: Redis channel names, Sentry init, webhook signing primitives,
//! prometheus exporter, etc. None of those belong here — keeping them in the
//! private `pegana-common` lets us mirror this single crate to a public repo
//! without leaking operational surface.
//!
//! License: MIT.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

/// Remove complete HTTP URLs from transport errors before logging or persisting
/// them. RPC credentials can appear in either the query string or path.
pub fn redact_urls_in_error(error: impl std::fmt::Display) -> String {
    let input = error.to_string();
    let mut output = String::with_capacity(input.len());
    let mut rest = input.as_str();
    loop {
        // ASCII case folding preserves byte offsets into the original string.
        let lower = rest.to_ascii_lowercase();
        let start = [
            "https://",
            "http://",
            "wss://",
            "ws://",
            "https%3a%2f%2f",
            "http%3a%2f%2f",
        ]
        .into_iter()
        .filter_map(|scheme| lower.find(scheme))
        .min();
        let Some(start) = start else {
            output.push_str(rest);
            break;
        };
        output.push_str(&rest[..start]);
        let url = &rest[start..];
        let end = url
            .find(|c: char| c.is_whitespace() || matches!(c, ')' | ']' | '}' | '>' | '"' | '\''))
            .unwrap_or(url.len());
        output.push_str("<redacted-url>");
        rest = &url[end..];
    }
    redact_key_fragments(&output)
}

/// Consecutive missed Jupiter polls before a market is reported dark.
pub const DARK_POLLS: u64 = 3;
/// Lower bound for the dark-market staleness window.
pub const DARK_MARKET_WINDOW_FLOOR_SECS: i64 = 120;

/// Staleness window for Jupiter quotes under the dark-market gate:
/// `max(floor, (DARK_POLLS + 1) × ceil(assets / concurrency) × tick)`.
/// One full round-robin cycle takes `ceil(assets / concurrency)` ticks, so the
/// window must cover several cycles or a healthy asset (or a single 429) reads
/// as dark. The indexer and the engine both call this, so they agree.
pub fn dark_market_window_secs(assets: usize, concurrency: usize, tick_ms: u64) -> i64 {
    let slots = assets.div_ceil(concurrency.max(1)) as u64;
    let cycle_ms = slots.saturating_mul(tick_ms);
    let window_ms = (DARK_POLLS + 1).saturating_mul(cycle_ms);
    let secs = i64::try_from(window_ms.div_ceil(1000)).unwrap_or(i64::MAX);
    secs.max(DARK_MARKET_WINDOW_FLOOR_SECS)
}

const KEY_MASK: &str = "<redacted>";

/// Mask credential-looking `key=value` fragments that survive without a URL
/// scheme (for example a query string quoted on its own).
fn redact_key_fragments(input: &str) -> String {
    let mut output = String::with_capacity(input.len());
    let mut rest = input;
    loop {
        let lower = rest.to_ascii_lowercase();
        let next = ["api-key=", "apikey=", "key=", "token="]
            .into_iter()
            .filter_map(|key| lower.find(key).map(|start| (start, key.len())))
            .min_by_key(|(start, _)| *start);
        let Some((start, key_len)) = next else {
            output.push_str(rest);
            break;
        };
        let value = &rest[start + key_len..];
        // Already-masked values stay as they are, so the function is idempotent.
        let end = if value.starts_with(KEY_MASK) {
            KEY_MASK.len()
        } else {
            value
                .find(|c: char| {
                    c.is_whitespace()
                        || matches!(c, '&' | ')' | ']' | '}' | '>' | '"' | '\'' | ',' | ';')
                })
                .unwrap_or(value.len())
        };
        output.push_str(&rest[..start + key_len]);
        output.push_str(KEY_MASK);
        rest = &value[end..];
    }
    output
}

#[cfg(test)]
mod rpc_redaction_tests {
    use super::*;

    #[test]
    fn redacts_keyed_rpc_urls_in_query_and_path() {
        let message = "primary https://rpc.example/key-one?api-key=secret-one, fallback (https://rpc.example/key-two) failed";
        let redacted = redact_urls_in_error(message);
        assert_eq!(
            redacted,
            "primary <redacted-url> fallback (<redacted-url>) failed"
        );
        assert!(!redacted.contains("secret-one") && !redacted.contains("key-two"));
    }

    #[test]
    fn redacts_case_variants_websocket_and_encoded_scheme() {
        for message in [
            "HTTPS://rpc.example/FIXTURE_TOKEN",
            "wss://rpc.example/FIXTURE_TOKEN",
            "HtTp://rpc.example/?api-key=FIXTURE_TOKEN",
            "HTTPS%3A%2F%2Frpc.example%2FFIXTURE_TOKEN",
            "ws://rpc.example/FIXTURE_TOKEN",
            "api-key=FIXTURE_TOKEN",
        ] {
            assert!(
                !redact_urls_in_error(message).contains("FIXTURE_TOKEN"),
                "{message}"
            );
        }
    }

    #[test]
    fn dark_window_covers_full_cycles_and_respects_the_floor() {
        // 61 assets, concurrency 1, 2.5 s tick: 152.5 s per cycle, 4 cycles.
        assert_eq!(dark_market_window_secs(61, 1, 2_500), 610);
        assert_eq!(dark_market_window_secs(61, 2, 2_500), 310);
        assert_eq!(
            dark_market_window_secs(4, 4, 1_000),
            DARK_MARKET_WINDOW_FLOOR_SECS
        );
        assert_eq!(
            dark_market_window_secs(0, 0, 0),
            DARK_MARKET_WINDOW_FLOOR_SECS
        );
    }

    #[test]
    fn redacts_bare_http_url() {
        assert_eq!(
            redact_urls_in_error("connect http://10.0.0.1:8899/FIXTURE_TOKEN refused"),
            "connect <redacted-url> refused"
        );
    }

    #[test]
    fn redaction_is_idempotent() {
        for message in [
            "primary https://rpc.example/?api-key=FIXTURE_TOKEN failed",
            "query api-key=FIXTURE_TOKEN&token=FIXTURE_TOKEN",
        ] {
            let once = redact_urls_in_error(message);
            assert_eq!(redact_urls_in_error(&once), once);
        }
    }

    #[test]
    fn leaves_unrelated_text_unchanged() {
        assert_eq!(
            redact_urls_in_error("rpc timeout without an endpoint"),
            "rpc timeout without an endpoint"
        );
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Hash)]
#[serde(rename_all = "snake_case")]
pub enum AssetClass {
    Lst,
    StableFiat,
    StableCdp,
    StableRwa,
    StableDn,
    StableFx,
    /// Synthetic leverage token (e.g. Hylo xSOL). Not pegged — moves with
    /// underlying × variable leverage. Pegana monitors intrinsic-vs-market
    /// gap as an arbitrage / redemption-stress signal, with wider thresholds
    /// than peg-watched assets.
    SynthLev,
    /// Yield-bearing wrapper of a stable token (e.g. Hylo sHYUSD wraps
    /// hyUSD with stability-pool yield). Intrinsic is the wrapper's exchange
    /// rate × underlying NAV.
    StableYield,
}

impl AssetClass {
    /// Map each asset class to its consumer-facing peg anchor.
    ///
    /// Three anchors describe how to interpret fair value without knowledge of
    /// Pegana's internal taxonomy:
    ///
    /// - `"USD"` — fixed $1 target.  The discount from $1 IS the risk signal.
    ///   Reserve-backed, CDP, delta-neutral, and RWA stablecoins all peg to
    ///   exactly $1 regardless of the underlying collateral mechanism.
    ///
    /// - `"FX"` — fixed foreign-exchange peg to a non-USD fiat currency
    ///   (e.g. EURC→EUR, BRZ→BRL).  The target is a fixed exchange rate, not
    ///   $1.  Consumers should read `peg_target` to learn WHICH currency.
    ///   A discount from the FX rate IS the risk signal.
    ///
    /// - `"NAV"` — intrinsic redemption / net-asset value.  Fair value is NOT
    ///   a fixed fiat rate: it moves as staking rewards accrue (LSTs) or the
    ///   yield wrapper compounds (stable_yield), or varies with protocol
    ///   leverage (synth_lev).  The discount is relative to NAV, and NAV
    ///   itself moves.
    ///
    /// EXHAUSTIVE — no wildcard arm.  Adding a 9th variant to `AssetClass`
    /// without extending this method produces a compile error, which is the
    /// point.  The "unknown class → NAV" conservative fallback lives ONLY at
    /// the API-rs string-parse boundary (`anchor_for_class`), not here.
    pub fn anchor(&self) -> &'static str {
        match self {
            // ── USD-anchored (fixed $1 target) ─────────────────────────────
            //
            // StableFiat: reserve-backed $1 stables (USDC, USDT, PYUSD).
            AssetClass::StableFiat => "USD",
            //
            // StableCdp: collateral-debt-position stables (USDS/MakerDAO,
            //   hyUSD/Hylo).  Collateral backs a $1 peg; thresholds are
            //   CR-based but the TARGET is still a fixed $1 redemption value.
            AssetClass::StableCdp => "USD",
            //
            // StableRwa: real-world-asset-backed $1 stables.  Fixed NAV via
            //   redemption against the backing asset at a $1-equivalent rate.
            AssetClass::StableRwa => "USD",
            //
            // StableDn: delta-neutral synthetic stables (USDe/Ethena, JupUSD).
            //   Hold $1 via delta hedging, NOT via yield accrual.
            AssetClass::StableDn => "USD",

            // ── FX-anchored (fixed non-USD fiat peg) ───────────────────────
            //
            // StableFx: FX-rate-pegged stables (BRZ→BRL, EURC→EUR).  The
            //   target is a fixed exchange rate.  Consumers read `peg_target`
            //   (BRL / EUR / …) to learn which currency.
            AssetClass::StableFx => "FX",

            // ── NAV-anchored (intrinsic redemption / net-asset value) ───────
            //
            // Lst: liquid-staking tokens (jitoSOL, mSOL, bSOL, …).  Fair
            //   value is stake-pool sol_per_lst × SOL/USD — NOT a fixed rate.
            AssetClass::Lst => "NAV",
            //
            // StableYield: yield-bearing wrappers (sHYUSD, USDY, sUSDe,
            //   syrupUSDC, sUSD/Solayer, ONyc, pbUSDC).  Intrinsic NAV starts
            //   at $1 and GROWS over time; a market price below NAV is the
            //   risk signal.
            AssetClass::StableYield => "NAV",
            //
            // SynthLev: synthetic leverage tokens (xSOL/Hylo).  Intrinsic is
            //   a variable-leverage NAV driven by the protocol reserve.
            AssetClass::SynthLev => "NAV",
        }
    }

    /// Parse the snake_case DB string into `AssetClass`, matching the
    /// `#[serde(rename_all = "snake_case")]` annotation.
    ///
    /// Returns `None` for any string that is not a known variant.  In
    /// practice this should never happen (the Postgres `asset_class` enum is
    /// migrated in lockstep), but callers may handle unknown future variants
    /// gracefully at a parse boundary rather than crashing.
    pub fn from_db_str(s: &str) -> Option<Self> {
        // Reuse the existing serde rename_all="snake_case" round-trip rather
        // than a second hand-maintained match.
        serde_json::from_value(serde_json::Value::String(s.to_owned())).ok()
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Hash)]
pub enum PegTarget {
    SOL,
    USD,
    BRL,
    EUR,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum TokenProgram {
    Spl,
    #[serde(rename = "token_2022")]
    Token2022,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum IntrinsicStrategy {
    SanctumLst {
        symbol: String,
    },
    /// Generic SPL stake-pool LST not in the Sanctum registry. NAV(SOL/token) =
    /// total_lamports / pool_token_supply, read from the StakePool state account
    /// (owned by `program` — canonical SPL stake-pool OR a SanctumSpl fork; both
    /// share the StakePool layout). USD = NAV × Pyth SOL/USD. The indexer source
    /// `spl_stake_pool` publishes `{value_usd, value_sol}` on SRC_SPL_STAKE_POOL.
    SplStakePool {
        pool: String,
        program: String,
    },
    #[serde(rename = "fixed_1_usd")]
    Fixed1Usd,
    HyloCr {
        exchange_program: String,
        stability_pool: String,
    },
    /// xSOL NAV from Hylo Exchange state.
    /// NAV (SOL) = (collateral_sol − hyusd_supply_in_sol) / xsol_supply
    /// where hyusd_supply_in_sol = hyusd_supply × $1 / SOL_USD (Pyth).
    HyloXsolNav {
        exchange_program: String,
    },
    /// sHYUSD exchange rate from Stability Pool state.
    /// Intrinsic_USD = (sp_hyusd_balance / shyusd_supply) × $1
    HyloShyusdRate {
        stability_pool: String,
    },
    PythFxCross {
        feed: String,
        invert: bool,
    },
    /// Generic Pyth-published redemption-rate / NAV feed (suffix `.RR`). Used
    /// for any wrapped/yield-bearing asset whose issuer publishes its NAV via
    /// Pyth (e.g. SYRUPUSDC/USDC.RR, Maple's official redemption rate).
    /// The indexer routes the cached feed price into `intrinsic_snapshots`
    /// and the engine consumes it directly — no per-issuer adapter needed.
    PythRedemptionRate {
        feed: String,
    },

    // Phase A: yield-bearing stable adapters (one variant per issuer for
    // type-safe verify-assets and explicit account/program addresses).

    // Ethena sUSDe: originally read from Pyth `Crypto.SUSDE/USDE.RR`. That
    // feed is not available on Solana without paid Hermes, so since 2026-10
    // sUSDe (and USDY) read the issuer contract directly via `EvmViewNav`.

    // Maple syrupUSDC: Task 7 found that Maple publishes the official
    // syrupUSDC redemption rate to Pyth as `Crypto.SYRUPUSDC/USDC.RR`, so the
    // generic `PythRedemptionRate` variant covers it. No issuer-specific
    // adapter is needed and the previously-planned `MaplePoolNav` variant is
    // removed to keep the enum dead-code-free.
    /// Solayer sUSD — rebasing share rate, likely Token-2022 InterestBearing
    /// extension or a separate share-rate account.
    SolayerSusdRate {
        share_account: String,
        program: String,
    },

    /// Perena USD* share token, post-migration (2026-10). NAV is the
    /// `accounting.mint_share_price` (u64, 6dp) of the `bankineco` Vault
    /// account (bytemuck, IDL-decoded) that is the USD* mint authority.
    /// Liveness is the vault's `accounting.last_update_ts` (crank_nav, ~5 min).
    PerenaVaultNav {
        vault: String,
        program: String,
    },

    /// Jupiter Perpetuals LP (JLP). NAV = Pool `aum_usd` (u128, 6dp) / JLP
    /// mint supply (6dp), both read in one `getMultipleAccounts` so the pair
    /// is slot-consistent. `pool` is owned by `program`; the LP mint is the
    /// asset's own `mint`.
    JupiterPerpsPoolNav {
        pool: String,
        program: String,
    },

    /// Issuer NAV read from an Ethereum mainnet `eth_call` to a view function
    /// (e.g. Ondo RWADynamicOracle `getPrice()`, ERC-4626 `convertToAssets`).
    /// `calldata` is the full hex call data (selector + ABI-encoded args);
    /// the uint256 result is scaled by `10^decimals`. Read from >= 1 public
    /// RPC; when two answer, they must agree within `max_divergence_bps`.
    EvmViewNav {
        contract: String,
        calldata: String,
        decimals: u32,
        #[serde(default = "default_evm_max_divergence_bps")]
        max_divergence_bps: u32,
        #[serde(default = "default_evm_max_block_age_secs")]
        max_block_age_secs: i64,
    },

    /// Fiat FX rate (e.g. EUR/USD, quoted as USD per 1 unit of `base`) taken
    /// as the median of free public venues (Kraken ticker mid, Coinbase
    /// exchange-rates). Rejected when the venues diverge by more than
    /// `max_divergence_bps`. Not published while the FX market is closed
    /// (the forex weekend), matching the previous Pyth FX behaviour.
    FxHttpMedian {
        base: String,
        quote: String,
        #[serde(default = "default_fx_max_divergence_bps")]
        max_divergence_bps: u32,
    },

    // OnRe ONyc: Task 8 found that OnRe publishes ONyc's NAV directly to Pyth
    // as `Crypto.NAV.ONYC/USD` ("Crypto NAV" asset type). Consumed via the
    // generic PythRedemptionRate variant. OnreOnycNav variant removed.
    /// Piggybank pbUSDC vault — reads NAV from the vault PDA. Strategy is
    /// delta-neutral funding-rate arb across ~10 perp DEXs.
    PiggybankPbVault {
        vault: String,
        program: String,
    },
    // StreamflowUsdPlus deferred to v1.5 — USD+ not yet live on mainnet as of
    // 2026-05-22 (Streamflow announced 2025-12-24, waitlist still open).
    // Re-add this variant when the mint ships.
}

fn default_evm_max_divergence_bps() -> u32 {
    10
}

fn default_evm_max_block_age_secs() -> i64 {
    180
}

fn default_fx_max_divergence_bps() -> u32 {
    10
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum MarketStrategy {
    JupiterUsd {
        numeraire: String,
        numeraire_feed: String,
    },
    /// Pyth-published spot price (USD-quoted). Used for assets that have
    /// both a redemption-rate feed AND a secondary-market spot feed on Pyth
    /// (e.g. USDY, sUSDe, syrupUSDC). The pyth.rs source writes
    /// market_snapshots when the configured feed updates — no Jupiter quote
    /// needed, which unblocks engine recompute even when Jupiter is rate-
    /// limited.
    PythSpot { feed: String },
    /// DexScreener-aggregated USD price (top-liquidity pair across all
    /// indexed Solana DEXes — Raydium, Orca, Meteora, etc.). Free API at
    /// `api.dexscreener.com`, no auth required, rate limit ~300/min.
    /// Used as a fallback market source for assets without a Pyth spot
    /// feed AND where Jupiter lite-api is rate-limited (R2). Lookup keyed
    /// by the asset's mint address.
    DexScreenerUsd,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(untagged)]
pub enum Thresholds {
    Bps {
        drift: u32,
        depeg: u32,
        critical: u32,
    },
    Cr {
        drift: u32,
        depeg: u32,
        critical: u32,
        black_swan: u32,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AssetConfig {
    pub symbol: String,
    pub name: String,
    pub mint: String,
    #[serde(default)]
    pub verified: bool,
    pub decimals: u8,
    pub token_program: TokenProgram,
    pub class: AssetClass,
    pub peg_target: PegTarget,
    pub intrinsic: IntrinsicStrategy,
    pub market: MarketStrategy,
    #[serde(default)]
    pub pyth_feed_id: String,
    #[serde(flatten)]
    pub thresholds: ThresholdsConfig,
    /// Whether the indexer/engine should poll this asset. Defaults to true so
    /// existing entries Just Work. Set to `false` to keep the asset in the
    /// canonical list (and in the DB for historical snapshots) but stop all
    /// runtime processing — useful for assets whose market source is broken
    /// (e.g. Jupiter TOKEN_NOT_TRADABLE) or for v1.5 placeholders.
    #[serde(default = "default_active")]
    pub active: bool,
    /// Jupiter DEX labels to exclude from this asset's quotes (sent as
    /// `excludeDexes`). Used to keep the market leg independent of the
    /// intrinsic leg: an issuer's own mint/redeem route (e.g. "Perena Star V2"
    /// for USD*, "Perps" for JLP) prices the token AT its NAV by construction,
    /// so it can never reveal a discount.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub jupiter_exclude_dexes: Vec<String>,
}

fn default_active() -> bool {
    true
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ThresholdsConfig {
    pub thresholds_bps: Option<HashMap<String, u32>>,
    pub thresholds_cr: Option<HashMap<String, u32>>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AssetsFile {
    pub assets: Vec<AssetConfig>,
    #[serde(default)]
    pub pyth_feeds: HashMap<String, String>,
    #[serde(default)]
    pub polling: PollingConfig,
    /// Per-feed staleness window overrides. Pyth Hermes pushes at ~1Hz for
    /// spot feeds (SOL/USD, USDC/USD, …) but issuer-attested NAV /
    /// redemption-rate feeds (USDY/USD, SUSDE/USDE.RR, SYRUPUSDC/USDC.RR,
    /// ONYC/USD.NAV) only step at the underlying's settlement cadence —
    /// daily, sometimes weekly. A single global 30s window rejects those
    /// feeds permanently. Use `[pyth_staleness.overrides]` to widen the
    /// window per feed; the `default_secs` value (or env override) applies
    /// to anything not listed.
    #[serde(default)]
    pub pyth_staleness: PythStalenessConfig,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct PythStalenessConfig {
    /// Window (seconds) applied to feeds without an override entry.
    /// Defaults to 30s when missing.
    pub default_secs: Option<i64>,
    /// Per-feed-name overrides. Keys match the human feed name from
    /// `[pyth_feeds]` (e.g. "USDY/USD"), not the hex feed_id.
    #[serde(default)]
    pub overrides: HashMap<String, i64>,
}

impl PythStalenessConfig {
    /// Resolve the staleness window (seconds) for a given feed name.
    /// Precedence: explicit override → `default_secs` → caller-provided fallback.
    pub fn limit_for(&self, feed: &str, fallback_secs: i64) -> i64 {
        if let Some(v) = self.overrides.get(feed) {
            return *v;
        }
        self.default_secs.unwrap_or(fallback_secs)
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct PollingConfig {
    pub sanctum_poll_ms: Option<u64>,
    pub jupiter_tick_ms: Option<u64>,
    pub hylo_onchain_poll_ms: Option<u64>,
    pub pyth_stream_reconnect_backoff_max_ms: Option<u64>,
    /// Cadence for the Solayer sUSD source. Falls back to 30s in the adapter
    /// when neither this nor `SOLAYER_POLL_MS` env are set.
    pub solayer_poll_ms: Option<u64>,
    /// Cadence for the Piggybank pbUSDC source. Falls back to 60s in the
    /// adapter when neither this nor `PIGGYBANK_POLL_MS` env are set. The
    /// share rate only changes at 48-hour epoch boundaries, so even 60s is
    /// generous — but cheap polling lets us catch the exact transition.
    pub piggybank_poll_ms: Option<u64>,
    /// Minimum interval between `intrinsic_snapshots` rows written for a
    /// single Pyth redemption-rate feed. Hermes pushes ~1 Hz but redemption
    /// rates only change daily — this floor keeps the table from filling
    /// with near-duplicate rows. Falls back to 30s in the Pyth source when
    /// neither this nor `PYTH_INTRINSIC_INTERVAL_MS` env are set.
    pub pyth_intrinsic_interval_ms: Option<u64>,
}

#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "UPPERCASE")]
pub enum PegState {
    #[default]
    Pegged,
    Drift,
    Depeg,
    Critical,
    BlackSwan,
    Unknown,
}

/// In-process mirror of the `alert_evidence` row (migrations 0028 + 0031 + 0033).
/// Used by Phase 4 API (`GET /v1/audit/:id`) and webhook payloads. Field order
/// matches the column order in the migration files for clarity.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AlertEvidence {
    pub alert_id: uuid::Uuid,
    pub methodology_version: String,
    pub methodology_git_sha: Option<String>,
    pub assets_toml_sha256: String,
    pub inputs_frozen: serde_json::Value,
    pub computed: serde_json::Value,
    pub replay_artifact: Option<serde_json::Value>,
    pub receipt_sha256: String,
    pub onchain_tx_sig: Option<String>,
    /// One of `not_applicable | pending | committed | retry_exhausted | wallet_drained`.
    /// State machine enforced by the trigger from migration 0033 (ADR-0004).
    pub commit_status: String,
    pub created_at: DateTime<Utc>,
}

#[cfg(test)]
mod tests {
    use super::*;

    /// AssetClass::anchor() must be exhaustive over all 8 variants and must
    /// emit the correct three-way anchor value for each.
    /// C1 (2026-06-18): the mapping now lives here so adding a 9th variant
    /// without extending this method produces a compile error in anchor().
    #[test]
    fn asset_class_anchor_exhaustive() {
        // USD-anchored — fixed $1 target.
        assert_eq!(AssetClass::StableFiat.anchor(), "USD", "StableFiat");
        assert_eq!(AssetClass::StableCdp.anchor(), "USD", "StableCdp");
        assert_eq!(AssetClass::StableRwa.anchor(), "USD", "StableRwa");
        assert_eq!(AssetClass::StableDn.anchor(), "USD", "StableDn");

        // FX-anchored — fixed non-USD fiat peg.
        // C2: StableFx must be "FX", not "USD".
        assert_eq!(AssetClass::StableFx.anchor(), "FX", "StableFx");

        // NAV-anchored — intrinsic redemption/net-asset value.
        assert_eq!(AssetClass::Lst.anchor(), "NAV", "Lst");
        assert_eq!(AssetClass::StableYield.anchor(), "NAV", "StableYield");
        assert_eq!(AssetClass::SynthLev.anchor(), "NAV", "SynthLev");
    }

    /// AssetClass::from_db_str round-trips every snake_case DB string.
    #[test]
    fn asset_class_from_db_str_round_trips() {
        let cases = [
            ("lst", AssetClass::Lst),
            ("stable_fiat", AssetClass::StableFiat),
            ("stable_cdp", AssetClass::StableCdp),
            ("stable_rwa", AssetClass::StableRwa),
            ("stable_dn", AssetClass::StableDn),
            ("stable_fx", AssetClass::StableFx),
            ("synth_lev", AssetClass::SynthLev),
            ("stable_yield", AssetClass::StableYield),
        ];
        for (s, expected) in &cases {
            assert_eq!(
                AssetClass::from_db_str(s),
                Some(*expected),
                "from_db_str({s:?})"
            );
        }
        // Unknown strings must return None, not panic.
        assert_eq!(AssetClass::from_db_str("unknown_future_class"), None);
        assert_eq!(AssetClass::from_db_str(""), None);
        assert_eq!(AssetClass::from_db_str("STABLE_FIAT"), None, "wrong case");
    }

    /// Sanity check: assets.toml parses cleanly into the public type tree
    /// and contains at least the post-Phase-0 required symbol set. This
    /// double-purposes as a regression test for AssetsFile field shapes
    /// when adding new IntrinsicStrategy / MarketStrategy variants.
    #[test]
    #[cfg(feature = "workspace-tests")]
    fn assets_toml_parses() {
        let raw = include_str!("../../../assets.toml");
        let parsed: AssetsFile = toml::from_str(raw).expect("assets.toml is valid");
        // AC84 — Phase 0 added 4 new symbols (JupUSD, EURC, dzSOL, vSOL) and
        // dropped JLP. The post-Phase 0 floor is 27 assets.
        assert!(
            parsed.assets.len() >= 27,
            "expected at least 27 assets, found {}",
            parsed.assets.len()
        );
        let symbols: Vec<_> = parsed.assets.iter().map(|a| &a.symbol).collect();
        // Confirmed-on-mainnet symbols only — USD* remains deferred (Save
        // Finance cToken layout still TBD, see Task 10 notes). Task 11
        // resolved pbUSDC's mint + vault on-chain, so it joins the required
        // set. USDY/sUSD/syrupUSDC are the other yield-bearing stables with
        // verified mints. JupUSD/EURC/dzSOL/vSOL added in Phase 0 (AC84).
        for required in [
            "jitoSOL",
            "USDC",
            "USDT",
            "PYUSD",
            "hyUSD",
            "BRZ",
            "USDY",
            "sUSD",
            "syrupUSDC",
            "pbUSDC",
            "JupUSD",
            "EURC",
            "dzSOL",
            "vSOL",
        ] {
            assert!(
                symbols.iter().any(|s| s.as_str() == required),
                "missing {required}"
            );
        }
    }

    #[test]
    fn pyth_staleness_default_falls_through() {
        let cfg = PythStalenessConfig::default();
        assert_eq!(cfg.limit_for("SOL/USD", 30), 30);
    }

    #[test]
    fn pyth_staleness_override_wins() {
        let mut cfg = PythStalenessConfig {
            default_secs: Some(60),
            overrides: HashMap::new(),
        };
        cfg.overrides.insert("USDY/USD".into(), 86_400);
        assert_eq!(cfg.limit_for("USDY/USD", 30), 86_400);
        assert_eq!(cfg.limit_for("SOL/USD", 30), 60);
    }
}
