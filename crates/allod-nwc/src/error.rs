//! NWC error codes (SPEC §8.3) and the single mapping from core errors.

use allod_core::{AppError, WalletError};
use serde_json::{json, Value};

/// A NIP-47 error code.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Code {
    RateLimited,
    NotImplemented,
    InsufficientBalance,
    QuotaExceeded,
    Restricted,
    Unauthorized,
    Internal,
    UnsupportedEncryption,
    PaymentFailed,
    NotFound,
    Expired,
    BadRequest,
    Other,
    UnsupportedNetwork,
    TemporarilyUnavailable,
    UnsupportedPaymentInstruction,
    MultipleMatches,
    UnsupportedPaymentType,
    FeeLimitExceeded,
}

impl Code {
    pub fn as_str(self) -> &'static str {
        match self {
            Code::RateLimited => "RATE_LIMITED",
            Code::NotImplemented => "NOT_IMPLEMENTED",
            Code::InsufficientBalance => "INSUFFICIENT_BALANCE",
            Code::QuotaExceeded => "QUOTA_EXCEEDED",
            Code::Restricted => "RESTRICTED",
            Code::Unauthorized => "UNAUTHORIZED",
            Code::Internal => "INTERNAL",
            Code::UnsupportedEncryption => "UNSUPPORTED_ENCRYPTION",
            Code::PaymentFailed => "PAYMENT_FAILED",
            Code::NotFound => "NOT_FOUND",
            Code::Expired => "EXPIRED",
            Code::BadRequest => "BAD_REQUEST",
            Code::Other => "OTHER",
            Code::UnsupportedNetwork => "UNSUPPORTED_NETWORK",
            Code::TemporarilyUnavailable => "TEMPORARILY_UNAVAILABLE",
            Code::UnsupportedPaymentInstruction => "UNSUPPORTED_PAYMENT_INSTRUCTION",
            Code::MultipleMatches => "MULTIPLE_MATCHES",
            Code::UnsupportedPaymentType => "UNSUPPORTED_PAYMENT_TYPE",
            Code::FeeLimitExceeded => "FEE_LIMIT_EXCEEDED",
        }
    }
}

/// An error returned to the NWC client.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NwcError {
    pub code: Code,
    pub message: String,
}

impl NwcError {
    pub fn new(code: Code, message: impl Into<String>) -> Self {
        NwcError { code, message: message.into() }
    }

    pub fn to_json(&self) -> Value {
        json!({"code": self.code.as_str(), "message": self.message})
    }
}

/// Maps a wallet error to its NWC code. Internal details are not sent to clients.
pub fn nwc_error_from(e: WalletError) -> NwcError {
    let msg = e.to_string();
    let code = match e {
        WalletError::BadRequest(_) => Code::BadRequest,
        WalletError::Unsupported(_) => Code::NotImplemented,
        WalletError::InsufficientBalance => Code::InsufficientBalance,
        WalletError::QuotaExceeded => Code::QuotaExceeded,
        WalletError::AlreadyPaid | WalletError::InProgress => Code::Other,
        WalletError::NotFound => Code::NotFound,
        WalletError::Expired => Code::Expired,
        WalletError::WrongNetwork(_) => Code::UnsupportedNetwork,
        WalletError::PaymentFailed(_) => Code::PaymentFailed,
        WalletError::Backend(allod_ln::BackendError::Unavailable(_))
        | WalletError::Backend(allod_ln::BackendError::Timeout) => Code::TemporarilyUnavailable,
        WalletError::Backend(allod_ln::BackendError::InvalidRequest(_)) => Code::BadRequest,
        WalletError::Backend(_) => Code::Other,
        WalletError::Internal(_) => {
            tracing::error!(error = %msg, "wallet error in NWC request");
            return NwcError::new(Code::Internal, "internal error");
        }
    };
    NwcError::new(code, msg)
}

pub fn nwc_error_from_app(e: AppError) -> NwcError {
    match e {
        AppError::BadRequest(m) => NwcError::new(Code::BadRequest, m),
        AppError::NotFound => NwcError::new(Code::NotFound, "not found"),
        AppError::Internal(m) => {
            tracing::error!(error = %m, "apps error in NWC request");
            NwcError::new(Code::Internal, "internal error")
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use allod_ln::BackendError;

    #[test]
    fn every_wallet_error_maps_to_a_code() {
        let cases = [
            (WalletError::BadRequest("x".into()), "BAD_REQUEST"),
            (WalletError::Unsupported("x".into()), "NOT_IMPLEMENTED"),
            (WalletError::InsufficientBalance, "INSUFFICIENT_BALANCE"),
            (WalletError::QuotaExceeded, "QUOTA_EXCEEDED"),
            (WalletError::AlreadyPaid, "OTHER"),
            (WalletError::InProgress, "OTHER"),
            (WalletError::NotFound, "NOT_FOUND"),
            (WalletError::Expired, "EXPIRED"),
            (WalletError::WrongNetwork("regtest".into()), "UNSUPPORTED_NETWORK"),
            (WalletError::PaymentFailed("no route".into()), "PAYMENT_FAILED"),
            (
                WalletError::Backend(BackendError::Unavailable("x".into())),
                "TEMPORARILY_UNAVAILABLE",
            ),
            (WalletError::Backend(BackendError::Timeout), "TEMPORARILY_UNAVAILABLE"),
            (WalletError::Backend(BackendError::InvalidRequest("x".into())), "BAD_REQUEST"),
            (WalletError::Backend(BackendError::Lightning("x".into())), "OTHER"),
            (WalletError::Internal("secret detail".into()), "INTERNAL"),
        ];
        for (e, code) in cases {
            assert_eq!(nwc_error_from(e).code.as_str(), code);
        }
        assert_eq!(
            nwc_error_from(WalletError::Internal("db path".into())).message,
            "internal error"
        );
    }
}
