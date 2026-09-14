//! HTTP request and readiness helpers shared by the harnesses.

use crate::{Error, Result};
use reqwest::{Client, Method, StatusCode};
use serde_json::Value;
use std::time::Duration;

/// Make a JSON request and return both non-success statuses and their payloads.
pub(crate) async fn request_json(
    client: &Client,
    method: &str,
    url: &str,
    payload: Option<&Value>,
) -> Result<(u16, Value)> {
    let method = Method::from_bytes(method.as_bytes())
        .map_err(|error| Error::Message(format!("invalid HTTP method {method}: {error}")))?;
    let mut request = client
        .request(method, url)
        .header("accept", "application/json")
        .timeout(Duration::from_secs(2));
    if let Some(payload) = payload {
        request = request
            .header("content-type", "application/json")
            .json(payload);
    }
    let response = request.send().await.map_err(|source| Error::Http {
        url: url.to_owned(),
        source,
    })?;
    let status = response.status();
    let body = response.bytes().await.map_err(|source| Error::Http {
        url: url.to_owned(),
        source,
    })?;
    let payload = if body.is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(&body).map_err(|source| Error::Json {
            context: url.to_owned(),
            source,
        })?
    };
    Ok((status.as_u16(), payload))
}

/// Wait until an HTTP endpoint can be reached and optionally has a status.
pub(crate) async fn wait_for_http(
    client: &Client,
    url: &str,
    expected_status: Option<u16>,
    timeout: Duration,
) -> Result<()> {
    let deadline = tokio::time::Instant::now() + timeout;
    loop {
        if tokio::time::Instant::now() >= deadline {
            return Err(Error::Message(format!("timed out waiting for {url}")));
        }
        match request_json(client, "GET", url, None).await {
            Ok((status, _)) if expected_status.is_none_or(|expected| expected == status) => {
                return Ok(());
            }
            Ok(_) | Err(Error::Http { .. } | Error::Json { .. }) => {}
            Err(error) => return Err(error),
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}

/// Build the HTTP client used by deterministic tools.
pub(crate) fn client() -> Result<Client> {
    Client::builder()
        .connect_timeout(Duration::from_secs(2))
        .build()
        .map_err(|source| Error::Http {
            url: "client configuration".to_owned(),
            source,
        })
}

/// Reserve an available loopback port for an isolated fixture process.
pub(crate) fn free_port() -> Result<u16> {
    let listener = std::net::TcpListener::bind(("127.0.0.1", 0))
        .map_err(|source| Error::io("bind temporary port", source))?;
    listener
        .local_addr()
        .map(|address| address.port())
        .map_err(|source| Error::io("read temporary port", source))
}

/// Assert that a response has the expected success status.
pub(crate) fn require_success(status: u16, payload: &Value, operation: &str) -> Result<()> {
    if StatusCode::from_u16(status).is_ok_and(|status| status.is_success()) {
        Ok(())
    } else {
        Err(Error::Message(format!(
            "{operation} returned HTTP {status}: {payload}"
        )))
    }
}

#[cfg(test)]
mod tests {
    use super::{client, request_json};
    use axum::http::StatusCode;
    use axum::routing::post;
    use axum::{Json, Router};
    use serde_json::json;
    use tokio::net::TcpListener;

    #[tokio::test]
    async fn request_json_preserves_error_status_and_payload()
    -> Result<(), Box<dyn std::error::Error>> {
        let listener = TcpListener::bind(("127.0.0.1", 0)).await?;
        let port = listener.local_addr()?.port();
        let router = Router::new().route(
            "/check",
            post(|| async {
                (
                    StatusCode::UNPROCESSABLE_ENTITY,
                    Json(json!({"error": "invalid fixture"})),
                )
            }),
        );
        let server = tokio::spawn(axum::serve(listener, router).into_future());
        let (status, payload) = request_json(
            &client()?,
            "POST",
            &format!("http://127.0.0.1:{port}/check"),
            Some(&json!({"fixture": true})),
        )
        .await?;
        server.abort();
        assert_eq!(status, 422);
        assert_eq!(payload, json!({"error": "invalid fixture"}));
        Ok(())
    }
}
