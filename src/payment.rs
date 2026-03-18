use std::pin::Pin;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use async_trait::async_trait;
use cdk_common::payment::{
    CreateIncomingPaymentResponse, Error as PaymentError, Event, IncomingPaymentOptions,
    MakePaymentResponse, MintPayment, OutgoingPaymentOptions, PaymentIdentifier,
    PaymentQuoteResponse, SettingsResponse, WaitPaymentResponse,
};
use cdk_common::{Amount, CurrencyUnit};
use futures::Stream;
use serde_json::Value;
use tokio::sync::{mpsc, Mutex};
use tokio_stream::wrappers::ReceiverStream;
use tokio_stream::StreamExt as _;
use tracing::instrument;

use crate::error::EhashError;

/// Validates that a string is a 64-character lowercase hex string (32 bytes).
fn is_valid_header_hash(s: &str) -> bool {
    s.len() == 64 && s.chars().all(|c| c.is_ascii_hexdigit())
}

/// Payment processor for the ehash custom payment method.
///
/// Ehash quotes are backed by mining shares: a miner submits a valid
/// proof-of-work share (identified by its `header_hash`) and, once the
/// Hashpool validates it, the quote is marked paid and tokens can be minted.
///
/// The processor exposes [`EhashPaymentProcessor::pay_ehash_quote`] so that
/// Hashpool's validation logic can trigger payment events from outside.
#[derive(Debug)]
pub struct EhashPaymentProcessor {
    unit: CurrencyUnit,
    sender: mpsc::Sender<WaitPaymentResponse>,
    receiver: Mutex<Option<mpsc::Receiver<WaitPaymentResponse>>>,
    wait_invoice_is_active: Arc<AtomicBool>,
    cancel_token: tokio_util::sync::CancellationToken,
}

impl EhashPaymentProcessor {
    /// Create a new processor for the given currency unit.
    pub fn new(unit: CurrencyUnit) -> Self {
        let (sender, receiver) = mpsc::channel(256);
        Self {
            unit,
            sender,
            receiver: Mutex::new(Some(receiver)),
            wait_invoice_is_active: Arc::new(AtomicBool::new(false)),
            cancel_token: tokio_util::sync::CancellationToken::new(),
        }
    }

    /// Mark an ehash quote as paid by emitting a payment event.
    ///
    /// `header_hash` must match the `request_lookup_id` stored on the quote.
    /// `amount` is the amount credited to the miner for this share.
    pub async fn pay_ehash_quote(
        &self,
        header_hash: &str,
        amount: Amount<CurrencyUnit>,
    ) -> Result<(), EhashError> {
        let event = WaitPaymentResponse {
            payment_identifier: PaymentIdentifier::CustomId(header_hash.to_string()),
            payment_amount: amount,
            payment_id: header_hash.to_string(),
        };
        self.sender
            .send(event)
            .await
            .map_err(|_| EhashError::NoReceiver)
    }
}

#[async_trait]
impl MintPayment for EhashPaymentProcessor {
    type Err = PaymentError;

    async fn get_settings(&self) -> Result<SettingsResponse, Self::Err> {
        let mut custom = std::collections::HashMap::new();
        custom.insert("ehash".to_string(), String::new());
        Ok(SettingsResponse {
            unit: self.unit.to_string(),
            bolt11: None,
            bolt12: None,
            custom,
        })
    }

    fn is_wait_invoice_active(&self) -> bool {
        self.wait_invoice_is_active.load(Ordering::SeqCst)
    }

    fn cancel_wait_invoice(&self) {
        self.cancel_token.cancel();
    }

    #[instrument(skip_all)]
    async fn wait_payment_event(
        &self,
    ) -> Result<Pin<Box<dyn Stream<Item = Event> + Send>>, Self::Err> {
        let receiver = self
            .receiver
            .lock()
            .await
            .take()
            .ok_or(PaymentError::from(EhashError::NoReceiver))?;
        self.wait_invoice_is_active.store(true, Ordering::SeqCst);
        let stream = ReceiverStream::new(receiver).map(|resp| Event::PaymentReceived(resp));
        Ok(Box::pin(stream))
    }

    #[instrument(skip_all)]
    async fn create_incoming_payment_request(
        &self,
        options: IncomingPaymentOptions,
    ) -> Result<CreateIncomingPaymentResponse, Self::Err> {
        let custom = match options {
            IncomingPaymentOptions::Custom(c) => c,
            _ => return Err(PaymentError::from(EhashError::WrongPaymentOptions)),
        };

        // Parse extra_json to extract header_hash
        let extra_str = custom
            .extra_json
            .as_deref()
            .ok_or_else(|| PaymentError::from(EhashError::MissingHeaderHash))?;
        let extra: Value = serde_json::from_str(extra_str)
            .map_err(|_| PaymentError::from(EhashError::InvalidExtraJson))?;
        let header_hash = extra["header_hash"]
            .as_str()
            .ok_or_else(|| PaymentError::from(EhashError::MissingHeaderHash))?;

        if !is_valid_header_hash(header_hash) {
            return Err(PaymentError::from(EhashError::InvalidHeaderHash));
        }

        Ok(CreateIncomingPaymentResponse {
            request_lookup_id: PaymentIdentifier::CustomId(header_hash.to_string()),
            request: header_hash.to_string(),
            expiry: custom.unix_expiry,
            extra_json: None,
        })
    }

    async fn get_payment_quote(
        &self,
        _unit: &CurrencyUnit,
        _options: OutgoingPaymentOptions,
    ) -> Result<PaymentQuoteResponse, Self::Err> {
        Err(PaymentError::from(EhashError::OutgoingNotSupported))
    }

    async fn make_payment(
        &self,
        _unit: &CurrencyUnit,
        _options: OutgoingPaymentOptions,
    ) -> Result<MakePaymentResponse, Self::Err> {
        Err(PaymentError::from(EhashError::OutgoingNotSupported))
    }

    async fn check_incoming_payment_status(
        &self,
        _payment_identifier: &PaymentIdentifier,
    ) -> Result<Vec<WaitPaymentResponse>, Self::Err> {
        // Ehash shares are validated externally and pushed via pay_ehash_quote().
        // There is no way to query historical payment status from this processor alone.
        Ok(vec![])
    }

    async fn check_outgoing_payment(
        &self,
        _payment_identifier: &PaymentIdentifier,
    ) -> Result<MakePaymentResponse, Self::Err> {
        Err(PaymentError::from(EhashError::OutgoingNotSupported))
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use cdk_common::payment::{Bolt11IncomingPaymentOptions, CustomIncomingPaymentOptions};
    use cdk_common::{Amount, CurrencyUnit};
    use tokio_stream::StreamExt as _;

    use super::*;

    /// A 64-character lowercase hex string used as a valid header hash in tests.
    const VALID_HASH: &str = "abcdef1234567890abcdef1234567890abcdef1234567890abcdef1234567890";

    fn make_processor() -> EhashPaymentProcessor {
        EhashPaymentProcessor::new(CurrencyUnit::Custom("ehash".to_string()))
    }

    fn custom_options(extra_json: Option<String>) -> IncomingPaymentOptions {
        IncomingPaymentOptions::Custom(Box::new(CustomIncomingPaymentOptions {
            method: "ehash".to_string(),
            description: None,
            amount: Amount::new(1000, CurrencyUnit::Custom("ehash".to_string())),
            unix_expiry: None,
            extra_json,
        }))
    }

    fn valid_options() -> IncomingPaymentOptions {
        custom_options(Some(format!(r#"{{"header_hash":"{}"}}"#, VALID_HASH)))
    }

    // ─── create_incoming_payment_request ────────────────────────────────────

    #[tokio::test]
    async fn test_create_request_valid_header_hash() {
        let processor = make_processor();
        let result = processor
            .create_incoming_payment_request(valid_options())
            .await;
        assert!(result.is_ok(), "expected Ok, got {:?}", result);
        let resp = result.unwrap();
        assert_eq!(
            resp.request_lookup_id,
            PaymentIdentifier::CustomId(VALID_HASH.to_string())
        );
        assert_eq!(resp.request, VALID_HASH);
        assert!(resp.expiry.is_none());
    }

    #[tokio::test]
    async fn test_create_request_uppercase_hex_is_valid() {
        // is_ascii_hexdigit accepts A-F as well as a-f
        let upper_hash = "ABCDEF1234567890ABCDEF1234567890ABCDEF1234567890ABCDEF1234567890";
        let processor = make_processor();
        let result = processor
            .create_incoming_payment_request(custom_options(Some(format!(
                r#"{{"header_hash":"{}"}}"#,
                upper_hash
            ))))
            .await;
        assert!(result.is_ok(), "uppercase hex should be accepted");
    }

    #[tokio::test]
    async fn test_create_request_wrong_options_type_returns_error() {
        let processor = make_processor();
        let options = IncomingPaymentOptions::Bolt11(Bolt11IncomingPaymentOptions {
            description: None,
            amount: Amount::new(1000, CurrencyUnit::Custom("ehash".to_string())),
            unix_expiry: None,
        });
        let result = processor.create_incoming_payment_request(options).await;
        assert!(result.is_err(), "Bolt11 options should be rejected");
        let msg = result.unwrap_err().to_string();
        assert!(
            msg.contains("Expected Custom") || msg.contains("Custom"),
            "unexpected error message: {msg}"
        );
    }

    #[tokio::test]
    async fn test_create_request_missing_extra_json_returns_error() {
        let processor = make_processor();
        let result = processor
            .create_incoming_payment_request(custom_options(None))
            .await;
        assert!(result.is_err(), "missing extra_json should fail");
    }

    #[tokio::test]
    async fn test_create_request_invalid_json_returns_error() {
        let processor = make_processor();
        let result = processor
            .create_incoming_payment_request(custom_options(Some("not-json".to_string())))
            .await;
        assert!(result.is_err(), "invalid JSON should fail");
    }

    #[tokio::test]
    async fn test_create_request_missing_header_hash_field_returns_error() {
        let processor = make_processor();
        let result = processor
            .create_incoming_payment_request(custom_options(Some(
                r#"{"other_field":"value"}"#.to_string(),
            )))
            .await;
        assert!(result.is_err(), "missing header_hash field should fail");
    }

    #[tokio::test]
    async fn test_create_request_header_hash_too_short_returns_error() {
        let processor = make_processor();
        // 63 hex chars — one short of the required 64
        let short_hash = "a".repeat(63);
        let result = processor
            .create_incoming_payment_request(custom_options(Some(format!(
                r#"{{"header_hash":"{}"}}"#,
                short_hash
            ))))
            .await;
        assert!(result.is_err(), "63-char hash should fail");
    }

    #[tokio::test]
    async fn test_create_request_header_hash_too_long_returns_error() {
        let processor = make_processor();
        // 65 hex chars — one over the required 64
        let long_hash = "a".repeat(65);
        let result = processor
            .create_incoming_payment_request(custom_options(Some(format!(
                r#"{{"header_hash":"{}"}}"#,
                long_hash
            ))))
            .await;
        assert!(result.is_err(), "65-char hash should fail");
    }

    #[tokio::test]
    async fn test_create_request_non_hex_header_hash_returns_error() {
        let processor = make_processor();
        // 64 chars but 'g' is not a valid hex digit
        let bad_hash = "g".repeat(64);
        let result = processor
            .create_incoming_payment_request(custom_options(Some(format!(
                r#"{{"header_hash":"{}"}}"#,
                bad_hash
            ))))
            .await;
        assert!(result.is_err(), "non-hex chars should fail");
    }

    // ─── get_settings ────────────────────────────────────────────────────────

    #[tokio::test]
    async fn test_get_settings_contains_ehash_custom_method() {
        let processor = make_processor();
        let settings = processor.get_settings().await.unwrap();
        assert!(
            settings.custom.contains_key("ehash"),
            "settings.custom should contain 'ehash'"
        );
        assert_eq!(settings.unit, "ehash");
        assert!(settings.bolt11.is_none());
        assert!(settings.bolt12.is_none());
    }

    // ─── pay_ehash_quote ─────────────────────────────────────────────────────

    #[tokio::test]
    async fn test_pay_ehash_quote_sends_payment_event() {
        let processor = make_processor();

        // Start the payment event stream (simulates what mint.start() does)
        let mut stream = processor.wait_payment_event().await.unwrap();

        let amount = Amount::new(1000, CurrencyUnit::Custom("ehash".to_string()));
        processor.pay_ehash_quote(VALID_HASH, amount).await.unwrap();

        let event = tokio::time::timeout(Duration::from_secs(1), stream.next())
            .await
            .expect("event should arrive within 1s")
            .expect("stream should not end");

        match event {
            Event::PaymentReceived(resp) => {
                assert_eq!(
                    resp.payment_identifier,
                    PaymentIdentifier::CustomId(VALID_HASH.to_string())
                );
                assert_eq!(resp.payment_amount.value(), 1000);
                assert_eq!(resp.payment_id, VALID_HASH);
            }
        }
    }

    // ─── wait_payment_event ──────────────────────────────────────────────────

    #[tokio::test]
    async fn test_wait_payment_event_second_call_fails() {
        let processor = make_processor();
        // First call takes the receiver — must succeed
        let _stream = processor.wait_payment_event().await.unwrap();
        // Second call finds no receiver — must fail
        let result = processor.wait_payment_event().await;
        assert!(
            result.is_err(),
            "second call to wait_payment_event should fail"
        );
    }
}
