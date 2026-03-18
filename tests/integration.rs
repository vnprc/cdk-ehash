//! Integration tests for the cdk-ehash payment processor.
//!
//! These tests verify the end-to-end flow of the ehash custom payment method:
//! quote creation, payment via `pay_ehash_quote`, and state transitions on the mint.
//!
//! All tests use an in-memory SQLite mint so no environment variables are required.

use std::sync::Arc;
use std::time::Duration;

use bip39::Mnemonic;
use cdk::mint::{MintBuilder, MintMeltLimits, MintQuoteRequest, MintQuoteResponse, QuoteId};
use cdk::nuts::nut23::QuoteState;
use cdk::nuts::{CurrencyUnit, MintQuoteCustomRequest, PaymentMethod};
use cdk::types::QuoteTTL;
use cdk::{Amount, Mint};
use cdk_ehash::EhashPaymentProcessor;
use serde_json::json;
use tokio::time::sleep;

const VALID_HASH_1: &str = "abcdef1234567890abcdef1234567890abcdef1234567890abcdef1234567890";
const VALID_HASH_2: &str = "1234567890abcdef1234567890abcdef1234567890abcdef1234567890abcdef";

/// Create an in-memory mint with a single EhashPaymentProcessor registered for the EHASH unit.
/// Returns `(mint, processor_arc)` — the caller keeps the Arc to trigger payments.
async fn create_ehash_mint() -> (Mint, Arc<EhashPaymentProcessor>) {
    let localstore = Arc::new(
        cdk_sqlite::mint::memory::empty()
            .await
            .expect("in-memory sqlite"),
    );

    let processor = Arc::new(EhashPaymentProcessor::new(CurrencyUnit::Custom(
        "ehash".to_string(),
    )));

    let mut mint_builder = MintBuilder::new(localstore.clone());

    mint_builder
        .add_payment_processor(
            CurrencyUnit::Custom("ehash".to_string()),
            PaymentMethod::Custom("ehash".to_string()),
            MintMeltLimits::new(1, 100_000),
            processor.clone(),
        )
        .await
        .expect("add_payment_processor");

    let mnemonic = Mnemonic::generate(12).expect("mnemonic");

    let mint_builder = mint_builder
        .with_name("ehash-test-mint".to_string())
        .with_description("ehash integration test mint".to_string())
        .with_urls(vec!["https://test.example".to_string()])
        .with_limits(2000, 2000);

    let quote_ttl = QuoteTTL::new(10_000, 10_000);

    let mint = mint_builder
        .build_with_seed(localstore, &mnemonic.to_seed_normalized(""))
        .await
        .expect("build mint");

    mint.set_quote_ttl(quote_ttl).await.expect("set quote ttl");
    mint.start().await.expect("mint.start");

    (mint, processor)
}

/// Helper: create an ehash quote on the mint for the given `header_hash` and `amount`.
async fn create_quote(mint: &Mint, header_hash: &str, amount: u64) -> MintQuoteResponse {
    let request = MintQuoteCustomRequest {
        amount: Amount::from(amount),
        unit: CurrencyUnit::Custom("ehash".to_string()),
        description: None,
        pubkey: None,
        extra: json!({ "header_hash": header_hash }),
    };

    mint.get_mint_quote(MintQuoteRequest::Custom {
        method: "ehash".to_string(),
        request,
    })
    .await
    .expect("get_mint_quote")
}

/// Poll `mint.check_mint_quotes` until the custom quote reaches `QuoteState::Paid`,
/// or panic after `timeout`.
async fn wait_for_paid(mint: &Mint, quote_id: QuoteId) {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);

    loop {
        if tokio::time::Instant::now() > deadline {
            panic!("timed out waiting for quote {quote_id} to become PAID");
        }

        let resp = mint
            .check_mint_quote(&quote_id)
            .await
            .expect("check_mint_quote");

        if let MintQuoteResponse::Custom { response, .. } = resp {
            if response.state == QuoteState::Paid {
                return;
            }
        }

        sleep(Duration::from_millis(20)).await;
    }
}

// ─── Tests ──────────────────────────────────────────────────────────────────

/// Creating an ehash quote returns a Custom response with the header_hash as the
/// payment request and initial state UNPAID.
#[tokio::test]
async fn test_create_ehash_quote_initial_state_is_unpaid() {
    let (mint, _processor) = create_ehash_mint().await;

    let response = create_quote(&mint, VALID_HASH_1, 100).await;

    let (method, inner) = match response {
        MintQuoteResponse::Custom { method, response } => (method, response),
        other => panic!("expected Custom response, got {:?}", other),
    };

    assert_eq!(method, "ehash");
    // The payment request is the header_hash itself
    assert_eq!(inner.request, VALID_HASH_1);
    assert_eq!(inner.state, QuoteState::Unpaid);
    assert!(inner.amount.is_some());
}

/// After `pay_ehash_quote` is called, the background payment listener marks the
/// corresponding quote as PAID.
#[tokio::test]
async fn test_pay_ehash_quote_marks_quote_as_paid() {
    let (mint, processor) = create_ehash_mint().await;

    let response = create_quote(&mint, VALID_HASH_1, 500).await;
    let quote_id = match &response {
        MintQuoteResponse::Custom { response, .. } => response.quote.clone(),
        _ => panic!("expected Custom response"),
    };

    let amount = Amount::new(500, CurrencyUnit::Custom("ehash".to_string()));
    processor
        .pay_ehash_quote(VALID_HASH_1, amount)
        .await
        .expect("pay_ehash_quote");

    wait_for_paid(&mint, quote_id).await;
}

/// Multiple quotes backed by different header_hashes can be paid independently.
#[tokio::test]
async fn test_multiple_quotes_can_be_paid_independently() {
    let (mint, processor) = create_ehash_mint().await;

    let resp1 = create_quote(&mint, VALID_HASH_1, 100).await;
    let resp2 = create_quote(&mint, VALID_HASH_2, 200).await;

    let id1 = match &resp1 {
        MintQuoteResponse::Custom { response, .. } => response.quote.clone(),
        _ => panic!("expected Custom"),
    };
    let id2 = match &resp2 {
        MintQuoteResponse::Custom { response, .. } => response.quote.clone(),
        _ => panic!("expected Custom"),
    };

    // Pay the second quote first
    processor
        .pay_ehash_quote(
            VALID_HASH_2,
            Amount::new(200, CurrencyUnit::Custom("ehash".to_string())),
        )
        .await
        .expect("pay quote 2");

    wait_for_paid(&mint, id2).await;

    // First quote should still be UNPAID at this point
    let resp1 = mint.check_mint_quote(&id1).await.expect("check 1");
    if let MintQuoteResponse::Custom { response, .. } = resp1 {
        assert_eq!(
            response.state,
            QuoteState::Unpaid,
            "quote 1 should still be UNPAID before its payment"
        );
    }

    // Now pay the first quote
    processor
        .pay_ehash_quote(
            VALID_HASH_1,
            Amount::new(100, CurrencyUnit::Custom("ehash".to_string())),
        )
        .await
        .expect("pay quote 1");

    wait_for_paid(&mint, id1).await;
}

/// Paying a quote with an unknown header_hash is silently ignored — no quote
/// changes state.
#[tokio::test]
async fn test_pay_unknown_hash_does_not_affect_existing_quotes() {
    let (mint, processor) = create_ehash_mint().await;

    let response = create_quote(&mint, VALID_HASH_1, 100).await;
    let quote_id = match &response {
        MintQuoteResponse::Custom { response, .. } => response.quote.clone(),
        _ => panic!("expected Custom"),
    };

    // Pay with a completely different header hash — no matching quote exists
    let unknown_hash = "deadbeef".repeat(8); // 64 hex chars
    processor
        .pay_ehash_quote(
            &unknown_hash,
            Amount::new(100, CurrencyUnit::Custom("ehash".to_string())),
        )
        .await
        .expect("pay_ehash_quote with unknown hash");

    // Brief wait to allow background task to process
    sleep(Duration::from_millis(100)).await;

    // The existing quote should still be UNPAID
    let resp = mint.check_mint_quote(&quote_id).await.expect("check");
    if let MintQuoteResponse::Custom { response, .. } = resp {
        assert_eq!(
            response.state,
            QuoteState::Unpaid,
            "quote should remain UNPAID when a different hash is paid"
        );
    }
}
