//! Integration tests for `pegana-replay`.
//!
//! Synthesize a Receipt via the methodology crate (the same code path
//! the engine uses in prod), serialize to a temp JSON file, invoke the
//! just-built binary via `CARGO_BIN_EXE_pegana-replay`, assert PASS
//! exit-0 stdout AND that tampering with `market_usd` flips it to
//! exit-1 with "FAIL" on stderr. Covers AC30, AC31, AC32, AC34.

use chrono::Utc;
use pegana_common_verify::{AssetClass, PegState};
use pegana_methodology::{
    canonical_receipt_hash, hex_sha256, methodology_git_sha, methodology_version, Computed,
    InputsFrozen, Receipt,
};
use rust_decimal::Decimal;
use serde_json::json;
use solana_sdk::signature::Signature;
use std::collections::HashMap;
use std::io::Write;
use std::process::Command;
use std::str::FromStr;
use uuid::Uuid;
use wiremock::matchers::{body_string_contains, method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

fn synth_receipt() -> Receipt {
    synth_receipt_state(PegState::Drift)
}

/// Build a v1 receipt with a chosen `final_state`. v1 receipts skip the
/// re-derivation check (only the hash is verified), so the recorded state is
/// authoritative for the on-chain-anchor gate under test here.
fn synth_receipt_state(final_state: PegState) -> Receipt {
    let assets_toml = "[[assets]]\nsymbol = \"USDC\"\n";
    let inputs = InputsFrozen {
        asset: "USDC".into(),
        class: AssetClass::StableFiat,
        now: Utc::now(),
        alpha: Decimal::from_str("0.3").unwrap(),
        intrinsic_usd: Decimal::ONE,
        market_usd: Decimal::from_str("0.99").unwrap(),
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
    let computed = Computed {
        discount_raw: Decimal::from_str("0.01").unwrap(),
        discount_smooth: Decimal::from_str("0.01").unwrap(),
        final_state,
        confidence_label: "high".into(),
    };
    let canonical = toml::to_string(&toml::from_str::<toml::Value>(assets_toml).unwrap()).unwrap();
    let hash = canonical_receipt_hash(
        methodology_version(),
        methodology_git_sha(),
        &canonical,
        &inputs,
        &computed,
    )
    .unwrap();
    Receipt {
        schema_version: "v1".into(),
        methodology_version: methodology_version().to_string(),
        methodology_git_sha: methodology_git_sha().map(str::to_string),
        assets_toml_canonical: canonical,
        inputs_frozen: inputs,
        expected_computed: computed,
        expected_receipt_sha256: hex_sha256(hash),
        state_reason: None,
    }
}

fn write_bundle(receipt: &Receipt) -> tempfile::NamedTempFile {
    let mut f = tempfile::NamedTempFile::new().unwrap();
    f.write_all(serde_json::to_string(receipt).unwrap().as_bytes())
        .unwrap();
    f
}

#[test]
fn pass_on_clean_bundle() {
    let receipt = synth_receipt();
    let file = write_bundle(&receipt);
    let output = Command::new(env!("CARGO_BIN_EXE_pegana-replay"))
        .arg("--bundle")
        .arg(file.path())
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("PASS"), "stdout: {stdout}");
}

#[test]
fn fail_when_input_tampered() {
    let mut receipt = synth_receipt();
    receipt.inputs_frozen.market_usd = Decimal::from_str("0.50").unwrap();
    // expected_receipt_sha256 untouched → recompute will diverge.
    let file = write_bundle(&receipt);
    let output = Command::new(env!("CARGO_BIN_EXE_pegana-replay"))
        .arg("--bundle")
        .arg(file.path())
        .output()
        .unwrap();
    assert!(!output.status.success());
    assert_eq!(output.status.code(), Some(1));
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("FAIL"), "stderr: {stderr}");
}

// ── PASS output wording: v1 vs v2 ───────────────────────────────────────────

/// v1 PASS line must advertise that it is tamper-evident only (no re-derivation).
#[test]
fn v1_pass_line_says_tamper_evident_only() {
    // synth_receipt() produces a v1 receipt.
    let receipt = synth_receipt();
    assert_eq!(receipt.schema_version, "v1");
    let file = write_bundle(&receipt);
    let output = Command::new(env!("CARGO_BIN_EXE_pegana-replay"))
        .arg("--bundle")
        .arg(file.path())
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("tamper-evident only"),
        "v1 PASS line must contain 'tamper-evident only'; stdout: {stdout}"
    );
    assert!(
        !stdout.contains("(re-derived)"),
        "v1 PASS line must NOT contain '(re-derived)'; stdout: {stdout}"
    );
}

/// v2 PASS line must still say "(re-derived)".
#[test]
#[cfg(feature = "workspace-tests")]
fn v2_pass_line_says_re_derived() {
    // Use the committed USDe-004 v2 bundle (the hash-gate bundle from the spec).
    let bundle = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../docs/backtests/2025-10-10-usde-depeg/receipts/USDe-004.json");
    let output = Command::new(env!("CARGO_BIN_EXE_pegana-replay"))
        .arg("--bundle")
        .arg(&bundle)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("(re-derived)"),
        "v2 PASS line must contain '(re-derived)'; stdout: {stdout}"
    );
    assert!(
        !stdout.contains("tamper-evident only"),
        "v2 PASS line must NOT contain 'tamper-evident only'; stdout: {stdout}"
    );
}

// ── Fix 2: ERROR paths must exit 2, not 1 ───────────────────────────────────

#[test]
fn error_exit2_on_malformed_json_bundle() {
    // Write a file with invalid JSON — parse fails → ERROR → exit 2.
    let mut f = tempfile::NamedTempFile::new().unwrap();
    f.write_all(b"{ this is not valid json }").unwrap();
    let output = Command::new(env!("CARGO_BIN_EXE_pegana-replay"))
        .arg("--bundle")
        .arg(f.path())
        .output()
        .unwrap();
    assert!(!output.status.success());
    assert_eq!(
        output.status.code(),
        Some(2),
        "expected exit 2 (ERROR); stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("ERROR"),
        "expected ERROR prefix on stderr; got: {stderr}"
    );
}

#[test]
fn error_exit2_on_nonexistent_bundle_path() {
    // Point to a path that definitely does not exist → read fails → ERROR → exit 2.
    let output = Command::new(env!("CARGO_BIN_EXE_pegana-replay"))
        .arg("--bundle")
        .arg("/tmp/pegana_replay_test_nonexistent_bundle_abc123.json")
        .output()
        .unwrap();
    assert!(!output.status.success());
    assert_eq!(
        output.status.code(),
        Some(2),
        "expected exit 2 (ERROR); stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("ERROR"),
        "expected ERROR prefix on stderr; got: {stderr}"
    );
}

// ── Fix 2 + existing version logic: VERSION_MISMATCH exits 3 ────────────────

#[test]
fn version_mismatch_exit3_on_bogus_version() {
    // Synthesise a valid receipt, then swap the methodology_version to a
    // string that will never match the embedded CLI version.
    let mut receipt = synth_receipt();
    receipt.methodology_version = "0.0.0-bogus-test-version".into();
    // The sha256 no longer matches either, but the version check fires first
    // (exit 3) before the hash check (exit 1) so the exit code is 3.
    let file = write_bundle(&receipt);
    let output = Command::new(env!("CARGO_BIN_EXE_pegana-replay"))
        .arg("--bundle")
        .arg(file.path())
        .output()
        .unwrap();
    assert!(!output.status.success());
    assert_eq!(
        output.status.code(),
        Some(3),
        "expected exit 3 (VERSION_MISMATCH); stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("VERSION_MISMATCH"),
        "expected VERSION_MISMATCH on stderr; got: {stderr}"
    );
}

// ── ADR-0033: default-on --alert-id on-chain verification ──────────────────
//
// These tests exercise the --alert-id path (not --bundle) so the default-on
// on-chain check actually runs. A single wiremock MockServer plays both the
// Pegana API (GET /v1/audit/:id/replay-bundle, GET /v1/audit/:id/onchain)
// and, where needed, the Solana JSON-RPC endpoint (POST /) that the CLI
// calls via --solana-rpc — both point at the same mock server URL.

fn replay_bundle_path(id: Uuid) -> String {
    format!("/v1/audit/{id}/replay-bundle")
}

fn onchain_path(id: Uuid) -> String {
    format!("/v1/audit/{id}/onchain")
}

async fn mock_replay_bundle(server: &MockServer, id: Uuid, receipt: &Receipt) {
    Mock::given(method("GET"))
        .and(path(replay_bundle_path(id)))
        .respond_with(ResponseTemplate::new(200).set_body_json(receipt))
        .mount(server)
        .await;
}

/// RPC-unreachable -> exit 5 (never a mismatch, never a false PASS), AND
/// --offline genuinely skips the network call (proven by still exiting 0
/// against the SAME unreachable RPC).
#[tokio::test]
async fn offline_flag_skips_onchain_network_call() {
    let id = Uuid::new_v4();
    let receipt = synth_receipt();
    let server = MockServer::start().await;
    mock_replay_bundle(&server, id, &receipt).await;
    Mock::given(method("GET"))
        .and(path(onchain_path(id)))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "tx_sig": Signature::default().to_string(),
        })))
        .mount(&server)
        .await;

    // Deliberately unreachable: nothing listens on port 1 (privileged),
    // so the connection is refused fast rather than hanging.
    let unreachable_rpc = "http://127.0.0.1:1";

    // Without --offline: default-on on-chain check attempts the RPC call,
    // fails to connect, and that failure must surface as exit 5
    // ONCHAIN_INCOMPLETE — never a silent pass, never conflated with a real
    // mismatch (exit 4).
    let output = Command::new(env!("CARGO_BIN_EXE_pegana-replay"))
        .arg("--alert-id")
        .arg(id.to_string())
        .arg("--api-url")
        .arg(server.uri())
        .arg("--solana-rpc")
        .arg(unreachable_rpc)
        .output()
        .unwrap();
    assert_eq!(
        output.status.code(),
        Some(5),
        "stdout: {}\nstderr: {}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("PASS"),
        "the hash check must still run and pass; stdout: {stdout}"
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("ONCHAIN_INCOMPLETE"), "stderr: {stderr}");
    assert!(
        stderr.to_lowercase().contains("not a mismatch"),
        "stderr must clarify this is not a mismatch; stderr: {stderr}"
    );

    // With --offline: the on-chain step must be skipped outright. Exiting 0
    // despite the identical unreachable RPC proves the flag genuinely
    // bypassed the network call rather than merely swallowing a connection
    // error.
    let output = Command::new(env!("CARGO_BIN_EXE_pegana-replay"))
        .arg("--alert-id")
        .arg(id.to_string())
        .arg("--api-url")
        .arg(server.uri())
        .arg("--solana-rpc")
        .arg(unreachable_rpc)
        .arg("--offline")
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "stdout: {}\nstderr: {}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("on-chain check skipped (--offline)"),
        "stderr: {stderr}"
    );
}

/// ADR-0004: `pending` is a legitimate transient state (anchor inside its
/// 24h commit window) — it must never read as ONCHAIN_MISMATCH (exit 4).
#[tokio::test]
async fn pending_commit_status_gives_exit5_not_exit4() {
    let id = Uuid::new_v4();
    let receipt = synth_receipt();
    let server = MockServer::start().await;
    mock_replay_bundle(&server, id, &receipt).await;
    Mock::given(method("GET"))
        .and(path(onchain_path(id)))
        .respond_with(
            ResponseTemplate::new(404).set_body_json(json!({ "commit_status": "pending" })),
        )
        .mount(&server)
        .await;

    let output = Command::new(env!("CARGO_BIN_EXE_pegana-replay"))
        .arg("--alert-id")
        .arg(id.to_string())
        .arg("--api-url")
        .arg(server.uri())
        .output()
        .unwrap();
    assert_eq!(
        output.status.code(),
        Some(5),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("pending"), "stderr: {stderr}");
    assert!(stderr.contains("24h"), "stderr: {stderr}");
    assert!(
        !stderr.contains("ANCHOR NOT VERIFIED"),
        "pending must never read as a hard mismatch; stderr: {stderr}"
    );
}

/// `not_applicable` on a state the engine never anchors on-chain (Drift here)
/// may skip silently — pre-existing behavior, now reachable via the default
/// path instead of only via the old opt-in --verify-onchain flag.
#[tokio::test]
async fn not_applicable_commit_status_silently_skips() {
    let id = Uuid::new_v4();
    let receipt = synth_receipt(); // Drift — never anchored on-chain
    let server = MockServer::start().await;
    mock_replay_bundle(&server, id, &receipt).await;
    Mock::given(method("GET"))
        .and(path(onchain_path(id)))
        .respond_with(
            ResponseTemplate::new(404).set_body_json(json!({ "commit_status": "not_applicable" })),
        )
        .mount(&server)
        .await;

    let output = Command::new(env!("CARGO_BIN_EXE_pegana-replay"))
        .arg("--alert-id")
        .arg(id.to_string())
        .arg("--api-url")
        .arg(server.uri())
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("PASS"), "stdout: {stdout}");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("never anchored on-chain"),
        "stderr: {stderr}"
    );
}

/// `not_applicable` on a SEVERE state (Depeg/Critical/BlackSwan) is NOT a
/// silent pass: the anchor policy expects an on-chain commit for that state,
/// so its absence means commit was disabled at emission. Surfaces as
/// ONCHAIN_INCOMPLETE (exit 5) — never a false PASS (exit 0), never a tamper
/// exit 4. This is the fix for the misleading "cost-gated" exit-0 on the most
/// severe alerts.
#[tokio::test]
async fn not_applicable_on_severe_state_gives_exit5_not_pass() {
    let id = Uuid::new_v4();
    let receipt = synth_receipt_state(PegState::Critical);
    let server = MockServer::start().await;
    mock_replay_bundle(&server, id, &receipt).await;
    Mock::given(method("GET"))
        .and(path(onchain_path(id)))
        .respond_with(
            ResponseTemplate::new(404).set_body_json(json!({ "commit_status": "not_applicable" })),
        )
        .mount(&server)
        .await;

    let output = Command::new(env!("CARGO_BIN_EXE_pegana-replay"))
        .arg("--alert-id")
        .arg(id.to_string())
        .arg("--api-url")
        .arg(server.uri())
        .output()
        .unwrap();
    assert_eq!(
        output.status.code(),
        Some(5),
        "severe not_applicable must be ONCHAIN_INCOMPLETE; stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("no on-chain anchor") && stderr.contains("Critical"),
        "stderr: {stderr}"
    );
    assert!(
        !stderr.contains("ANCHOR NOT VERIFIED"),
        "a disabled anchor must not read as tamper (exit 4); stderr: {stderr}"
    );
}

/// A real terminal anchoring failure (ops issue, not tamper) stays a hard
/// ONCHAIN_MISMATCH (exit 4) — only `pending` was carved out into exit 5.
#[tokio::test]
async fn retry_exhausted_status_is_still_hard_onchain_mismatch() {
    let id = Uuid::new_v4();
    let receipt = synth_receipt();
    let server = MockServer::start().await;
    mock_replay_bundle(&server, id, &receipt).await;
    Mock::given(method("GET"))
        .and(path(onchain_path(id)))
        .respond_with(
            ResponseTemplate::new(404).set_body_json(json!({ "commit_status": "retry_exhausted" })),
        )
        .mount(&server)
        .await;

    let output = Command::new(env!("CARGO_BIN_EXE_pegana-replay"))
        .arg("--alert-id")
        .arg(id.to_string())
        .arg("--api-url")
        .arg(server.uri())
        .output()
        .unwrap();
    assert_eq!(
        output.status.code(),
        Some(4),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("ANCHOR NOT VERIFIED"), "stderr: {stderr}");
}

/// --verify-onchain is kept as a back-compat no-op: existing scripts that
/// still pass it must not break, but it no longer does anything (on-chain
/// is default-on now) beyond printing a deprecation note.
#[test]
fn deprecated_verify_onchain_flag_is_backward_compatible_noop() {
    let receipt = synth_receipt();
    let file = write_bundle(&receipt);
    let output = Command::new(env!("CARGO_BIN_EXE_pegana-replay"))
        .arg("--bundle")
        .arg(file.path())
        .arg("--verify-onchain")
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("PASS"), "stdout: {stdout}");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("now a no-op"),
        "deprecated flag must warn it is a no-op; stderr: {stderr}"
    );
}

/// --bundle mode has no alert_id to look an on-chain anchor up with, so it
/// stays offline-only regardless of flags — but it must say so, not stay
/// silent about the weaker guarantee.
#[test]
fn bundle_mode_notes_onchain_not_available() {
    let receipt = synth_receipt();
    let file = write_bundle(&receipt);
    let output = Command::new(env!("CARGO_BIN_EXE_pegana-replay"))
        .arg("--bundle")
        .arg(file.path())
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("on-chain check not available for --bundle"),
        "stderr: {stderr}"
    );
}

/// The on-chain-layer analogue of `fail_when_input_tampered`: tamper the
/// artifact actually being checked (the memo's embedded sha256) and leave
/// the "expected" value (the receipt's own recomputed hash) untouched. The
/// CLI must recompute-and-compare, not trust the tampered field — a forged
/// on-chain memo with a wrong sha must NOT accidentally verify. This is
/// exactly the "verify-oversells-tamper" attack the default-on check exists
/// to close: a compromised API could otherwise swap in a different,
/// equally self-consistent receipt and only the on-chain layer catches it.
#[tokio::test]
async fn onchain_memo_mismatch_still_gives_exit4() {
    use solana_transaction_status_client_types::option_serializer::OptionSerializer;
    use solana_transaction_status_client_types::{
        EncodedConfirmedTransactionWithStatusMeta, EncodedTransaction,
        EncodedTransactionWithStatusMeta, ParsedAccount, ParsedInstruction, UiInstruction,
        UiMessage, UiParsedInstruction, UiParsedMessage, UiTransaction, UiTransactionStatusMeta,
    };

    let id = Uuid::new_v4();
    let receipt = synth_receipt();
    let server = MockServer::start().await;
    mock_replay_bundle(&server, id, &receipt).await;

    let tx_sig = Signature::default().to_string();
    Mock::given(method("GET"))
        .and(path(onchain_path(id)))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({ "tx_sig": tx_sig })))
        .mount(&server)
        .await;

    const PINNED_SIGNER: &str = "7PpoyumFQMmcWzhJxDYr6iPv1fjYN41KBTA8xKKzu7R9";
    const MEMO_PROGRAM: &str = "MemoSq4gqABAXKb96qnH8TysNcWxMyWCqXgDLGmfcHr";
    // Correctly signed by the pinned wallet, but the memo's own sha256
    // field is WRONG — expected_receipt_sha256 on the API-served receipt is
    // untouched, so the hash check (layer 1) still passes; only the
    // on-chain memo check (layer 2) must catch this forgery.
    let wrong_sha = "0".repeat(receipt.expected_receipt_sha256.len());
    assert_ne!(wrong_sha, receipt.expected_receipt_sha256);
    let forged_memo = format!("pegana-v1|0.4.0|{id}|{wrong_sha}");

    let tx = EncodedConfirmedTransactionWithStatusMeta {
        slot: 2,
        transaction: EncodedTransactionWithStatusMeta {
            version: None,
            transaction: EncodedTransaction::Json(UiTransaction {
                signatures: vec![tx_sig.clone()],
                message: UiMessage::Parsed(UiParsedMessage {
                    account_keys: vec![ParsedAccount {
                        pubkey: PINNED_SIGNER.to_string(),
                        writable: true,
                        signer: true,
                        source: None,
                    }],
                    recent_blockhash: "11111111111111111111111111111111".to_string(),
                    instructions: vec![UiInstruction::Parsed(UiParsedInstruction::Parsed(
                        ParsedInstruction {
                            program: "spl-memo".to_string(),
                            program_id: MEMO_PROGRAM.to_string(),
                            parsed: serde_json::Value::String(forged_memo),
                            stack_height: None,
                        },
                    ))],
                    address_table_lookups: None,
                }),
            }),
            meta: Some(UiTransactionStatusMeta {
                err: None,
                status: Ok(()),
                fee: 0,
                pre_balances: vec![],
                post_balances: vec![],
                inner_instructions: OptionSerializer::None,
                log_messages: OptionSerializer::None,
                pre_token_balances: OptionSerializer::None,
                post_token_balances: OptionSerializer::None,
                rewards: OptionSerializer::None,
                loaded_addresses: OptionSerializer::Skip,
                return_data: OptionSerializer::Skip,
                compute_units_consumed: OptionSerializer::Skip,
                cost_units: OptionSerializer::Skip,
            }),
        },
        block_time: None,
    };
    let rpc_result = serde_json::to_value(&tx).unwrap();

    Mock::given(method("POST"))
        .and(path("/"))
        .and(body_string_contains("getTransaction"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "jsonrpc": "2.0",
            "id": 1,
            "result": rpc_result,
        })))
        .mount(&server)
        .await;

    let output = Command::new(env!("CARGO_BIN_EXE_pegana-replay"))
        .arg("--alert-id")
        .arg(id.to_string())
        .arg("--api-url")
        .arg(server.uri())
        .arg("--solana-rpc")
        .arg(server.uri())
        .output()
        .unwrap();
    assert_eq!(
        output.status.code(),
        Some(4),
        "stdout: {}\nstderr: {}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("does NOT match receipt sha256"),
        "stderr: {stderr}"
    );
}

// ── --json: one machine-readable verdict line on stdout (for CI) ─────────────
// The exit code is unchanged from human mode (asserted above across 0-5); these
// only prove the JSON shape + that human text no longer leaks to stdout.

#[test]
fn json_pass_emits_verdict_on_stdout() {
    let receipt = synth_receipt(); // v1, Drift
    let file = write_bundle(&receipt);
    let output = Command::new(env!("CARGO_BIN_EXE_pegana-replay"))
        .arg("--bundle")
        .arg(file.path())
        .arg("--json")
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    let v: serde_json::Value = serde_json::from_str(stdout.trim())
        .unwrap_or_else(|e| panic!("--json stdout must be ONE JSON line: {e}; got: {stdout}"));
    assert_eq!(v["status"], "pass");
    assert_eq!(v["exit_code"], 0);
    assert_eq!(v["asset"], "USDC");
    assert_eq!(v["final_state"], "Drift");
    assert_eq!(v["methodology_version"], methodology_version());
    assert_eq!(v["receipt_sha256"], receipt.expected_receipt_sha256);
    // Human PASS text must NOT leak to stdout in json mode.
    assert!(
        !stdout.contains("PASS "),
        "json mode must not print the human PASS line; got: {stdout}"
    );
}

#[test]
fn json_fail_emits_verdict_on_tamper() {
    let mut receipt = synth_receipt();
    receipt.inputs_frozen.market_usd = Decimal::from_str("0.50").unwrap();
    let file = write_bundle(&receipt);
    let output = Command::new(env!("CARGO_BIN_EXE_pegana-replay"))
        .arg("--bundle")
        .arg(file.path())
        .arg("--json")
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(1));
    let stdout = String::from_utf8_lossy(&output.stdout);
    let v: serde_json::Value = serde_json::from_str(stdout.trim())
        .unwrap_or_else(|e| panic!("--json stdout must be JSON: {e}; got: {stdout}"));
    assert_eq!(v["status"], "fail");
    assert_eq!(v["exit_code"], 1);
    assert!(
        !stdout.contains("FAIL"),
        "json mode must not print the human FAIL text on stdout; got: {stdout}"
    );
}

#[test]
fn json_version_mismatch_emits_verdict() {
    let mut receipt = synth_receipt();
    receipt.methodology_version = "0.0.0-bogus-test-version".into();
    let file = write_bundle(&receipt);
    let output = Command::new(env!("CARGO_BIN_EXE_pegana-replay"))
        .arg("--bundle")
        .arg(file.path())
        .arg("--json")
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(3));
    let stdout = String::from_utf8_lossy(&output.stdout);
    let v: serde_json::Value = serde_json::from_str(stdout.trim())
        .unwrap_or_else(|e| panic!("--json stdout must be JSON: {e}; got: {stdout}"));
    assert_eq!(v["status"], "version_mismatch");
    assert_eq!(v["exit_code"], 3);
}
