//! Ladder leg 2: browser-profile HTTP impersonation (§8 Stage 1).
//!
//! This leg does not execute JavaScript. It reuses the hardened HTTP transport
//! and adds a configurable browser User-Agent and navigation headers, then
//! declares `stealth: true` so [`super::FetchLadder`] can select it after an
//! anti-bot response. TLS fingerprint impersonation is intentionally not
//! claimed; that remains a future provider replacement behind [`super::Fetcher`].

use async_trait::async_trait;

use super::{
    FetchCapabilities, FetchError, FetchPolicy, FetchValidators, FetchedDoc, Fetcher, HttpFetcher,
    HttpFetcherParams, NormalizedUrl,
};

/// HTTP impersonation provider for ladder leg 2.
pub struct ImpersonationFetcher {
    inner: HttpFetcher,
}

impl ImpersonationFetcher {
    /// Builds the browser-profile provider with the same network safety limits
    /// as [`HttpFetcher`].
    ///
    /// # Errors
    /// [`FetchError::Protocol`] if the HTTP client cannot be built or the
    /// browser User-Agent is empty.
    pub fn new(params: HttpFetcherParams, browser_user_agent: String) -> Result<Self, FetchError> {
        Ok(Self {
            inner: HttpFetcher::new_impersonated(params, browser_user_agent)?,
        })
    }
}

#[async_trait]
impl Fetcher for ImpersonationFetcher {
    fn capabilities(&self) -> FetchCapabilities {
        self.inner.capabilities()
    }

    async fn fetch_with_policy(
        &self,
        url: &NormalizedUrl,
        policy: &FetchPolicy,
    ) -> Result<FetchedDoc, FetchError> {
        self.inner.fetch_with_policy(url, policy).await
    }

    async fn fetch_with_validators(
        &self,
        url: &NormalizedUrl,
        policy: &FetchPolicy,
        validators: &FetchValidators,
    ) -> Result<FetchedDoc, FetchError> {
        self.inner
            .fetch_with_validators(url, policy, validators)
            .await
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

    use std::time::Duration;

    use super::*;

    fn params() -> HttpFetcherParams {
        HttpFetcherParams {
            user_agent: "ohara/test".to_string(),
            timeout: Duration::from_secs(1),
            rate_limit: Duration::ZERO,
            allow_private_hosts: true,
            max_body_bytes: 1024,
            max_redirects: 2,
        }
    }

    #[test]
    fn declares_browser_profile_without_javascript() {
        let fetcher =
            ImpersonationFetcher::new(params(), "Mozilla/5.0 test browser".to_string()).unwrap();
        assert_eq!(
            fetcher.capabilities(),
            FetchCapabilities {
                js_rendering: false,
                stealth: true,
            }
        );
    }

    #[test]
    fn rejects_an_empty_browser_user_agent() {
        let result = ImpersonationFetcher::new(params(), String::new());
        assert!(
            matches!(result, Err(FetchError::Protocol(message)) if message.contains("user-agent"))
        );
    }
}
