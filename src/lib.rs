//! CDK ehash payment processor for Hashpool mining share rewards.
//!
//! This crate provides [`EhashPaymentProcessor`], a [`MintPayment`] implementation
//! that allows a CDK mint to accept mining shares as payment for Cashu tokens.
//!
//! ## Flow
//!
//! 1. A miner submits `POST /v1/mint/quote/ehash` with `{"amount": N, "unit": "SAT",
//!    "header_hash": "<64-hex-chars>"}` in the request body.
//! 2. The upstream CDK custom router calls [`EhashPaymentProcessor::create_incoming_payment_request`],
//!    which validates the `header_hash` and creates a quote.
//! 3. Hashpool's validation logic calls [`EhashPaymentProcessor::pay_ehash_quote`] when
//!    the share is accepted, triggering the CDK payment pipeline.
//! 4. The miner polls `GET /v1/mint/quote/ehash/{id}` until state is `PAID`, then calls
//!    `POST /v1/mint/ehash` (or `POST /v1/mint/ehash/batch`) to receive tokens.
//!
//! ## Registration
//!
//! Register the processor with `cdk-mintd` by adding `"ehash"` to the custom payment
//! methods list and providing an `EhashPaymentProcessor` instance. The upstream
//! `create_custom_routers` function handles all HTTP routing automatically.

/// Error types for the ehash payment processor.
pub mod error;
/// [`MintPayment`] implementation for the ehash custom payment method.
pub mod payment;

pub use error::EhashError;
pub use payment::EhashPaymentProcessor;
