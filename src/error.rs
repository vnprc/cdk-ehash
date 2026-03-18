use thiserror::Error;

/// Errors from the ehash payment processor
#[derive(Debug, Error)]
pub enum EhashError {
    /// The incoming payment options were not for the ehash method
    #[error("Expected Custom(ehash) payment options")]
    WrongPaymentOptions,

    /// The extra_json field is missing or malformed
    #[error("Missing or invalid extra_json in request")]
    InvalidExtraJson,

    /// The header_hash field is missing from extra_json
    #[error("Missing header_hash in request extra fields")]
    MissingHeaderHash,

    /// The header_hash value is not a valid 64-char hex string
    #[error("Invalid header_hash: must be 64 hex characters (32 bytes)")]
    InvalidHeaderHash,

    /// No payment receiver is available (channel already consumed)
    #[error("Payment event receiver already consumed")]
    NoReceiver,

    /// Outgoing payments are not supported for ehash
    #[error("Ehash does not support outgoing payments")]
    OutgoingNotSupported,
}

impl From<EhashError> for cdk_common::payment::Error {
    fn from(e: EhashError) -> Self {
        cdk_common::payment::Error::Custom(e.to_string())
    }
}
