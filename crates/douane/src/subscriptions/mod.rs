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
