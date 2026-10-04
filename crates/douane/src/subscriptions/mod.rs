//! Subscription credentials, OAuth sessions, and provider-reported quotas.
//!
//! Credentials are persisted locally, while public account responses use an
//! explicit allowlist. Provider failures never copy response bodies into errors.

mod google;
mod oauth;
mod quota;
mod reset;
mod store;

pub use oauth::{OAuthAttempt, OAuthManager};
pub use quota::{QuotaCredits, QuotaService, QuotaSnapshot, QuotaWindow};
pub use reset::{ResetError, ResetManager, ResetOutcome, ResetRedemption, ResetStatus};
pub use store::{
    AccountCredential, AccountStore, AccountSummary, SubscriptionProvider, SubscriptionTier,
};

pub struct GoogleVerification {
    url: Option<String>,
}

impl std::fmt::Debug for GoogleVerification {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("GoogleVerification")
            .field("verification_available", &self.url.is_some())
            .finish()
    }
}

#[derive(Debug, thiserror::Error)]
pub enum SubscriptionError {
    #[error("invalid authentication file: {0}")]
    InvalidImport(&'static str),
    #[error("account not found")]
    NotFound,
    #[error("provider does not support this operation")]
    Unsupported,
    #[error("authentication storage failed")]
    Storage,
    #[error("upstream authentication or quota request failed (HTTP {0})")]
    Upstream(u16),
    #[error("Google account verification is required; complete verification and retry")]
    VerificationRequired(GoogleVerification),
    #[error(
        "Google did not return a Cloud project for this account; check account setup in Antigravity"
    )]
    GoogleProjectUnavailable,
    #[error("upstream authentication or quota request could not be completed")]
    Transport,
    #[error("upstream returned an invalid response")]
    InvalidResponse,
    #[error("OAuth attempt expired or was already consumed")]
    Expired,
    #[error("OAuth state does not match")]
    InvalidState,
    #[error("OAuth redirect URI is not a permitted callback")]
    InvalidRedirect,
    #[error("account has no refresh token")]
    NoRefreshToken,
    #[error("provider OAuth client is not configured")]
    OAuthNotConfigured,
}

impl SubscriptionError {
    pub(crate) fn verification_required(url: Option<&str>) -> Self {
        Self::VerificationRequired(GoogleVerification {
            url: url.and_then(oauth::validate_google_verification_url),
        })
    }

    pub(crate) fn is_verification_required(&self) -> bool {
        matches!(self, Self::VerificationRequired(_))
    }

    pub(crate) fn verification_url(&self) -> Option<&str> {
        match self {
            Self::VerificationRequired(verification) => verification.url.as_deref(),
            _ => None,
        }
    }
}

pub(crate) fn now() -> String {
    timestamp(time::OffsetDateTime::now_utc())
}

pub(crate) fn timestamp(value: time::OffsetDateTime) -> String {
    value
        .format(&time::format_description::well_known::Rfc3339)
        .unwrap_or_else(|_| value.unix_timestamp().to_string())
}

pub(crate) fn parse_timestamp(value: &str) -> Option<time::OffsetDateTime> {
    time::OffsetDateTime::parse(value, &time::format_description::well_known::Rfc3339).ok()
}

pub(crate) fn random_id() -> String {
    use rand::RngCore;
    let mut bytes = [0u8; 24];
    rand::rng().fill_bytes(&mut bytes);
    hex::encode(bytes)
}
