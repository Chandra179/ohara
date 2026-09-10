//! Fetch-ladder composition (§8 Stage 1).
//!
//! Each provider remains a [`Fetcher`] implementation. [`FetchLadder`] only
//! owns deterministic selection and escalation; it does not know provider
//! details or control-plane storage.

use std::sync::Arc;

use async_trait::async_trait;

use super::{
    FetchCapabilities, FetchError, FetchLeg, FetchPolicy, FetchValidators, FetchedDoc, Fetcher,
    NormalizedUrl,
};

/// A deterministic composition of one or more fetch legs.
pub struct FetchLadder {
    legs: Vec<(FetchLeg, Arc<dyn Fetcher>)>,
}

impl FetchLadder {
    /// Builds a ladder from provider legs. Legs are sorted by capability order;
    /// duplicate leg names are rejected so selection cannot be ambiguous.
    ///
    /// # Errors
    /// [`FetchError::Protocol`] when no legs are supplied or a leg is repeated.
    pub fn new(mut legs: Vec<(FetchLeg, Arc<dyn Fetcher>)>) -> Result<Self, FetchError> {
        if legs.is_empty() {
            return Err(FetchError::Protocol(
                "fetch ladder requires at least one leg".to_string(),
            ));
        }
        legs.sort_by_key(|(leg, _)| *leg);
        for pair in legs.windows(2) {
            if pair[0].0 == pair[1].0 {
                return Err(FetchError::Protocol(format!(
                    "fetch ladder contains duplicate {:?} leg",
                    pair[0].0
                )));
            }
        }
        Ok(Self { legs })
    }

    /// Builds a ladder containing one provider, preserving the existing HTTP
    /// runtime behavior while allowing the same runtime seam to grow later.
    ///
    /// This constructor is infallible because it creates exactly one plain leg.
    pub fn single(fetcher: Arc<dyn Fetcher>) -> Self {
        Self {
            legs: vec![(FetchLeg::Plain, fetcher)],
        }
    }

    fn first_leg_at_or_after(&self, requested: FetchLeg) -> Option<usize> {
        self.legs.iter().position(|(leg, _)| *leg >= requested)
    }

    fn unavailable(requested: FetchLeg) -> FetchError {
        FetchError::Protocol(format!(
            "fetch ladder has no provider at or after {requested:?}"
        ))
    }

    fn escalates(error: &FetchError) -> bool {
        matches!(
            error,
            FetchError::AntiBot { .. } | FetchError::JavaScriptRequired { .. }
        )
    }
}

#[async_trait]
impl Fetcher for FetchLadder {
    fn capabilities(&self) -> FetchCapabilities {
        self.legs.iter().fold(
            FetchCapabilities {
                js_rendering: false,
                stealth: false,
            },
            |capabilities, (_, fetcher)| {
                let leg = fetcher.capabilities();
                FetchCapabilities {
                    js_rendering: capabilities.js_rendering || leg.js_rendering,
                    stealth: capabilities.stealth || leg.stealth,
                }
            },
        )
    }

    async fn fetch_with_policy(
        &self,
        url: &NormalizedUrl,
        policy: &FetchPolicy,
    ) -> Result<FetchedDoc, FetchError> {
        self.fetch_with_validators(url, policy, &FetchValidators::default())
            .await
    }

    async fn fetch_with_validators(
        &self,
        url: &NormalizedUrl,
        policy: &FetchPolicy,
        validators: &FetchValidators,
    ) -> Result<FetchedDoc, FetchError> {
        let Some(start) = self.first_leg_at_or_after(policy.start_leg) else {
            return Err(Self::unavailable(policy.start_leg));
        };
        let mut last_error = None;
        for (index, (_, fetcher)) in self.legs.iter().enumerate().skip(start) {
            let result = if index == start {
                fetcher.fetch_with_validators(url, policy, validators).await
            } else {
                fetcher.fetch_with_policy(url, policy).await
            };
            match result {
                Ok(document) => return Ok(document),
                Err(error) if Self::escalates(&error) => last_error = Some(error),
                Err(error) => return Err(error),
            }
        }
        match last_error {
            Some(error) => Err(error),
            None => Err(Self::unavailable(policy.start_leg)),
        }
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

    use std::sync::Mutex;

    use super::*;

    #[derive(Clone, Copy)]
    enum Outcome {
        AntiBot,
        JavaScriptRequired,
        NotFound,
        Success,
    }

    struct FakeFetcher {
        outcome: Outcome,
        calls: Mutex<usize>,
        capabilities: FetchCapabilities,
    }

    impl FakeFetcher {
        fn new(outcome: Outcome, capabilities: FetchCapabilities) -> Self {
            Self {
                outcome,
                calls: Mutex::new(0),
                capabilities,
            }
        }

        fn calls(&self) -> usize {
            *self
                .calls
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
        }
    }

    #[async_trait]
    impl Fetcher for FakeFetcher {
        fn capabilities(&self) -> FetchCapabilities {
            self.capabilities
        }

        async fn fetch_with_policy(
            &self,
            url: &NormalizedUrl,
            _policy: &FetchPolicy,
        ) -> Result<FetchedDoc, FetchError> {
            *self
                .calls
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner) += 1;
            match self.outcome {
                Outcome::AntiBot => Err(FetchError::AntiBot {
                    url: url.to_string(),
                }),
                Outcome::JavaScriptRequired => Err(FetchError::JavaScriptRequired {
                    url: url.to_string(),
                }),
                Outcome::NotFound => Err(FetchError::NotFound {
                    url: url.to_string(),
                }),
                Outcome::Success => Ok(FetchedDoc {
                    html: "<html>ok</html>".to_string(),
                    js_executed: self.capabilities.js_rendering,
                    final_url: url.to_string(),
                    status: 200,
                    content_type: Some("text/html".to_string()),
                    etag: None,
                    last_modified: None,
                    fetched_at: "2026-09-10T00:00:00Z".to_string(),
                }),
            }
        }
    }

    #[tokio::test]
    async fn anti_bot_escalates_to_the_next_available_leg() {
        let plain = Arc::new(FakeFetcher::new(
            Outcome::AntiBot,
            FetchCapabilities {
                js_rendering: false,
                stealth: false,
            },
        ));
        let browser = Arc::new(FakeFetcher::new(
            Outcome::Success,
            FetchCapabilities {
                js_rendering: true,
                stealth: true,
            },
        ));
        let ladder = FetchLadder::new(vec![
            (FetchLeg::Plain, plain.clone()),
            (FetchLeg::Browser, browser.clone()),
        ])
        .unwrap();
        let url = NormalizedUrl::parse("https://example.com/page").unwrap();
        let document = ladder.fetch(&url).await.unwrap();

        assert!(document.js_executed);
        assert_eq!(plain.calls(), 1);
        assert_eq!(browser.calls(), 1);
        assert_eq!(
            ladder.capabilities(),
            FetchCapabilities {
                js_rendering: true,
                stealth: true,
            }
        );
    }

    #[tokio::test]
    async fn javascript_required_escalates_to_a_rendering_leg() {
        let plain = Arc::new(FakeFetcher::new(
            Outcome::JavaScriptRequired,
            FetchCapabilities {
                js_rendering: false,
                stealth: false,
            },
        ));
        let browser = Arc::new(FakeFetcher::new(
            Outcome::Success,
            FetchCapabilities {
                js_rendering: true,
                stealth: true,
            },
        ));
        let ladder = FetchLadder::new(vec![
            (FetchLeg::Plain, plain),
            (FetchLeg::Browser, browser.clone()),
        ])
        .unwrap();
        let url = NormalizedUrl::parse("https://example.com/app").unwrap();

        let document = ladder.fetch(&url).await.unwrap();

        assert!(document.js_executed);
        assert_eq!(browser.calls(), 1);
    }

    #[tokio::test]
    async fn configured_start_leg_skips_lower_legs() {
        let plain = Arc::new(FakeFetcher::new(
            Outcome::Success,
            FetchCapabilities {
                js_rendering: false,
                stealth: false,
            },
        ));
        let impersonate = Arc::new(FakeFetcher::new(
            Outcome::Success,
            FetchCapabilities {
                js_rendering: false,
                stealth: true,
            },
        ));
        let ladder = FetchLadder::new(vec![
            (FetchLeg::Plain, plain.clone()),
            (FetchLeg::Impersonate, impersonate.clone()),
        ])
        .unwrap();
        let url = NormalizedUrl::parse("https://example.com/page").unwrap();
        let policy = FetchPolicy {
            start_leg: FetchLeg::Impersonate,
            ..FetchPolicy::default()
        };
        ladder.fetch_with_policy(&url, &policy).await.unwrap();

        assert_eq!(plain.calls(), 0);
        assert_eq!(impersonate.calls(), 1);
    }

    #[tokio::test]
    async fn permanent_errors_do_not_escalate() {
        let plain = Arc::new(FakeFetcher::new(
            Outcome::NotFound,
            FetchCapabilities {
                js_rendering: false,
                stealth: false,
            },
        ));
        let browser = Arc::new(FakeFetcher::new(
            Outcome::Success,
            FetchCapabilities {
                js_rendering: true,
                stealth: true,
            },
        ));
        let ladder = FetchLadder::new(vec![
            (FetchLeg::Plain, plain),
            (FetchLeg::Browser, browser.clone()),
        ])
        .unwrap();
        let url = NormalizedUrl::parse("https://example.com/page").unwrap();
        let error = ladder.fetch(&url).await.unwrap_err();

        assert!(matches!(error, FetchError::NotFound { .. }));
        assert_eq!(browser.calls(), 0);
    }
}
