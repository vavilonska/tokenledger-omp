use chrono::{DateTime, Utc};
use reqwest::{blocking::Client, StatusCode};
use serde::Deserialize;
use serde_json::{json, Value};

use super::{auth::ClaudeOAuthConfig, ClaudeError};

const OAUTH_SCOPES: &str =
    "user:profile user:inference user:sessions:claude_code user:mcp_servers user:file_upload";

#[derive(Debug, Deserialize)]
pub struct ClaudeRefreshResponse {
    pub access_token: String,
    pub refresh_token: Option<String>,
    pub expires_in: Option<f64>,
}

#[derive(Clone)]
pub struct ClaudeClient {
    client: Client,
}

impl ClaudeClient {
    pub fn new() -> Result<Self, ClaudeError> {
        Self::with_timeout(std::time::Duration::from_secs(15))
    }

    fn with_timeout(timeout: std::time::Duration) -> Result<Self, ClaudeError> {
        let default_redirect_policy = reqwest::redirect::Policy::default();
        Ok(Self {
            client: Client::builder()
                .timeout(timeout)
                .redirect(reqwest::redirect::Policy::custom(move |attempt| {
                    if super::auth::oauth_url_is_secure(attempt.url()) {
                        default_redirect_policy.redirect(attempt)
                    } else {
                        attempt.error("Claude OAuth redirects require HTTPS outside loopback")
                    }
                }))
                .build()
                .map_err(|_| ClaudeError::ConnectionFailed)?,
        })
    }

    pub fn fetch_usage(
        &self,
        token: &str,
        config: &ClaudeOAuthConfig,
    ) -> Result<(StatusCode, Value, Option<u64>), ClaudeError> {
        let started = std::time::Instant::now();
        let response = self
            .client
            .get(&config.usage_url)
            .bearer_auth(token.trim())
            .header("Accept", "application/json")
            .header("anthropic-beta", "oauth-2025-04-20")
            .header("User-Agent", "claude-code/2.1.69")
            .send()
            .map_err(|_| {
                crate::app_warn!("http", "claude usage request failed (transport)");
                ClaudeError::ConnectionFailed
            })?;
        let status = response.status();
        crate::app_debug!(
            "http",
            "claude usage HTTP {} ({}ms)",
            status.as_u16(),
            started.elapsed().as_millis()
        );
        let retry_after = response
            .headers()
            .get("retry-after")
            .and_then(|value| value.to_str().ok())
            .and_then(|value| parse_retry_after(value, Utc::now()));
        let body = response.json().map_err(|_| ClaudeError::InvalidResponse)?;
        Ok((status, body, retry_after))
    }

    pub fn refresh_token(
        &self,
        token: &str,
        config: &ClaudeOAuthConfig,
    ) -> Result<ClaudeRefreshResponse, ClaudeError> {
        let started = std::time::Instant::now();
        crate::app_info!("auth:claude", "token refresh attempt");
        let response = self
            .client
            .post(&config.refresh_url)
            .json(&json!({
                "grant_type": "refresh_token",
                "refresh_token": token,
                "client_id": config.client_id,
                "scope": OAUTH_SCOPES
            }))
            .send()
            .map_err(|_| {
                crate::app_warn!("auth:claude", "token refresh failed (transport)");
                ClaudeError::ConnectionFailed
            })?;
        let status = response.status();
        crate::app_debug!(
            "http",
            "claude token refresh HTTP {} ({}ms)",
            status.as_u16(),
            started.elapsed().as_millis()
        );
        if !status.is_success() {
            if matches!(status, StatusCode::UNAUTHORIZED | StatusCode::BAD_REQUEST) {
                let body = response.json::<Value>().ok();
                let code = body.as_ref().and_then(|body| {
                    body.get("error")
                        .and_then(Value::as_str)
                        .or_else(|| body.get("error_description").and_then(Value::as_str))
                });
                if code == Some("invalid_grant") {
                    return Err(ClaudeError::SessionExpired);
                }
            }
            return Err(ClaudeError::RequestFailed(status.as_u16()));
        }
        let refreshed: ClaudeRefreshResponse =
            response.json().map_err(|_| ClaudeError::InvalidResponse)?;
        if refreshed.access_token.trim().is_empty() {
            return Err(ClaudeError::TokenExpired);
        }
        crate::app_info!("auth:claude", "token refresh succeeded");
        Ok(refreshed)
    }
}

fn parse_retry_after(value: &str, now: DateTime<Utc>) -> Option<u64> {
    let value = value.trim();
    if let Ok(seconds) = value.parse::<u64>() {
        return Some(seconds);
    }
    let date = DateTime::parse_from_rfc2822(value).ok()?.to_utc();
    let milliseconds = date.signed_duration_since(now).num_milliseconds().max(0) as u64;
    Some(milliseconds.div_ceil(1000))
}

#[cfg(test)]
mod tests {
    use reqwest::StatusCode;

    use chrono::{TimeZone, Utc};

    use super::{parse_retry_after, ClaudeClient};
    use crate::providers::{
        claude::{auth::ClaudeOAuthConfig, ClaudeError},
        test_http,
    };

    fn config(base: &str) -> ClaudeOAuthConfig {
        ClaudeOAuthConfig {
            usage_url: format!("{base}/usage"),
            refresh_url: format!("{base}/token"),
            client_id: "test-client".into(),
        }
    }

    #[test]
    fn usage_success_reads_json_and_retry_after() {
        let base = test_http::serve_once(200, &[("retry-after", "120")], r#"{"plan":"max"}"#);
        let (status, body, retry_after) = ClaudeClient::new()
            .unwrap()
            .fetch_usage("secret-token", &config(&base))
            .unwrap();

        assert_eq!(status, StatusCode::OK);
        assert_eq!(body["plan"], "max");
        assert_eq!(retry_after, Some(120));
    }

    #[test]
    fn credential_requests_reject_remote_plaintext_redirects() {
        let client = ClaudeClient::new().unwrap();
        for refresh in [false, true] {
            let base = test_http::serve_once(
                307,
                &[("location", "http://oauth.example.invalid/credentials")],
                "",
            );
            let error = if refresh {
                client
                    .refresh_token("secret-refresh", &config(&base))
                    .unwrap_err()
            } else {
                client
                    .fetch_usage("secret-token", &config(&base))
                    .unwrap_err()
            };
            assert!(matches!(error, ClaudeError::ConnectionFailed));
            assert!(!error.to_string().contains("secret-"));
        }
    }

    #[test]
    fn plaintext_redirects_fail_at_the_policy_boundary_before_dns_or_connection() {
        let client = ClaudeClient::new().unwrap();
        for method in [reqwest::Method::GET, reqwest::Method::POST] {
            let base = test_http::serve_once(
                307,
                &[("location", "http://oauth.example.invalid/credentials")],
                "",
            );
            let error = client
                .client
                .request(method, &base)
                .bearer_auth("local-test-token")
                .json(&serde_json::json!({"refresh_token": "local-test-refresh"}))
                .send()
                .unwrap_err();
            assert!(error.is_redirect(), "{error}");
            assert!(!error.is_connect(), "{error}");
        }
    }

    #[test]
    fn local_development_redirects_remain_usable() {
        let destination = test_http::serve_once(200, &[], r#"{"plan":"max"}"#);
        let source = test_http::serve_once(307, &[("location", &destination)], "");
        let (status, body, _) = ClaudeClient::new()
            .unwrap()
            .fetch_usage("local-test-token", &config(&source))
            .unwrap();
        assert_eq!(status, StatusCode::OK);
        assert_eq!(body["plan"], "max");
    }

    #[test]
    fn malformed_usage_and_timeout_return_safe_errors() {
        let malformed = test_http::serve_once(200, &[], "secret-token: not-json");
        let error = ClaudeClient::new()
            .unwrap()
            .fetch_usage("secret-token", &config(&malformed))
            .unwrap_err();
        assert!(matches!(error, ClaudeError::InvalidResponse));
        assert!(!error.to_string().contains("secret-token"));

        let timeout = test_http::serve_once_after(
            test_http::TIMEOUT_TEST_RESPONSE_DELAY,
            200,
            &[],
            r#"{"plan":"max"}"#,
        );
        let error = ClaudeClient::with_timeout(test_http::TIMEOUT_TEST_CLIENT_LIMIT)
            .unwrap()
            .fetch_usage("secret-token", &config(&timeout))
            .unwrap_err();
        assert!(matches!(error, ClaudeError::ConnectionFailed));
        assert!(!error.to_string().contains("secret-token"));
    }

    #[test]
    fn refresh_distinguishes_auth_failures_and_rate_limits() {
        let forbidden = test_http::serve_once(400, &[], r#"{"error":"invalid_grant"}"#);
        assert!(matches!(
            ClaudeClient::new()
                .unwrap()
                .refresh_token("secret-refresh", &config(&forbidden)),
            Err(ClaudeError::SessionExpired)
        ));

        let limited = test_http::serve_once(429, &[], r#"{"error":"slow_down"}"#);
        assert!(matches!(
            ClaudeClient::new()
                .unwrap()
                .refresh_token("secret-refresh", &config(&limited)),
            Err(ClaudeError::RequestFailed(429))
        ));
    }

    #[test]
    fn refresh_does_not_misclassify_an_unrecognized_400_as_expired() {
        let base = test_http::serve_once(400, &[], "<html>Bad Gateway</html>");
        assert!(matches!(
            ClaudeClient::new()
                .unwrap()
                .refresh_token("secret-refresh", &config(&base)),
            Err(ClaudeError::RequestFailed(400))
        ));
    }

    #[test]
    fn retry_after_accepts_seconds_and_http_dates() {
        let now = Utc.with_ymd_and_hms(2015, 10, 21, 7, 27, 0).unwrap();
        assert_eq!(parse_retry_after("120", now), Some(120));
        assert_eq!(
            parse_retry_after("Wed, 21 Oct 2015 07:28:00 GMT", now),
            Some(60)
        );
    }
}
