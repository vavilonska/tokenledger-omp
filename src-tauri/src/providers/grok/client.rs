use std::time::Duration;

use reqwest::{blocking::Client, StatusCode};
use serde_json::Value;

use super::GrokError;

const CREDITS_URL: &str = "https://cli-chat-proxy.grok.com/v1/billing?format=credits";
const SETTINGS_URL: &str = "https://cli-chat-proxy.grok.com/v1/settings";
const TOKEN_AUTH_HEADER: &str = "xai-grok-cli";

#[derive(Debug)]
pub struct GrokResponse {
    pub status: StatusCode,
    pub body: Value,
}

pub struct GrokClient {
    client: Client,
    credits_url: String,
    settings_url: String,
}

impl GrokClient {
    pub fn new() -> Result<Self, GrokError> {
        Self::with_endpoints(CREDITS_URL, SETTINGS_URL, Duration::from_secs(15))
    }

    fn with_endpoints(
        credits_url: &str,
        settings_url: &str,
        timeout: Duration,
    ) -> Result<Self, GrokError> {
        let client = Client::builder()
            .connect_timeout(Duration::from_secs(8))
            .timeout(timeout)
            .user_agent(concat!("TokenLedger OMP/", env!("CARGO_PKG_VERSION")))
            .build()
            .map_err(|_| GrokError::ConnectionFailed)?;
        Ok(Self {
            client,
            credits_url: credits_url.to_owned(),
            settings_url: settings_url.to_owned(),
        })
    }

    pub fn fetch_credits(&self, access_token: &str) -> Result<GrokResponse, GrokError> {
        self.fetch_authenticated(&self.credits_url, access_token, "billing")
    }

    pub fn fetch_settings(&self, access_token: &str) -> Result<GrokResponse, GrokError> {
        self.fetch_authenticated(&self.settings_url, access_token, "settings")
    }

    fn fetch_authenticated(
        &self,
        url: &str,
        access_token: &str,
        endpoint: &str,
    ) -> Result<GrokResponse, GrokError> {
        let started = std::time::Instant::now();
        let response = self
            .send_request(|| {
                self.client
                    .get(url)
                    .bearer_auth(access_token.trim())
                    .header("X-XAI-Token-Auth", TOKEN_AUTH_HEADER)
                    .header("Accept", "application/json")
            })
            .map_err(|_| {
                crate::app_warn!("http", "grok {endpoint} request failed (transport)");
                GrokError::ConnectionFailed
            })?;
        let status = response.status();
        crate::app_debug!(
            "http",
            "grok {endpoint} HTTP {} ({}ms)",
            status.as_u16(),
            started.elapsed().as_millis()
        );
        let text = response.text().map_err(|_| GrokError::InvalidResponse)?;
        Ok(GrokResponse {
            status,
            body: serde_json::from_str(&text).unwrap_or(Value::Null),
        })
    }

    #[cfg(not(test))]
    fn send_request(
        &self,
        request: impl Fn() -> reqwest::blocking::RequestBuilder,
    ) -> Result<reqwest::blocking::Response, reqwest::Error> {
        request().send()
    }

    #[cfg(test)]
    fn send_request(
        &self,
        request: impl Fn() -> reqwest::blocking::RequestBuilder,
    ) -> Result<reqwest::blocking::Response, reqwest::Error> {
        // Windows CI can transiently reject short-lived loopback connections. Production keeps
        // the single-attempt implementation above; only local HTTP tests receive this tolerance.
        const ATTEMPTS: usize = 3;
        let mut last_error = None;
        for attempt in 0..ATTEMPTS {
            match request().send() {
                Ok(response) => return Ok(response),
                Err(error) => {
                    last_error = Some(error);
                    if attempt + 1 < ATTEMPTS {
                        std::thread::sleep(Duration::from_millis(10));
                    }
                }
            }
        }
        Err(last_error.expect("at least one test request attempt"))
    }

    #[cfg(test)]
    pub fn for_test(credits_url: &str, settings_url: &str, timeout: Duration) -> Self {
        Self::with_endpoints(credits_url, settings_url, timeout).unwrap()
    }
}

#[cfg(test)]
mod tests {
    use std::{
        io::{Read, Write},
        net::TcpListener,
        sync::mpsc,
        thread,
        time::Duration,
    };

    use super::GrokClient;
    use crate::providers::{grok::GrokError, test_http};

    fn capture_once(
        response_body: &str,
    ) -> (String, mpsc::Receiver<String>, thread::JoinHandle<()>) {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let response_body = response_body.to_owned();
        let (sender, receiver) = mpsc::channel();
        let handle = thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut request = Vec::new();
            loop {
                let mut chunk = [0_u8; 1024];
                let count = stream.read(&mut chunk).unwrap();
                if count == 0 {
                    break;
                }
                request.extend_from_slice(&chunk[..count]);
                let text = String::from_utf8_lossy(&request);
                let Some(header_end) = text.find("\r\n\r\n") else {
                    continue;
                };
                let content_length = text[..header_end]
                    .lines()
                    .find_map(|line| {
                        line.to_ascii_lowercase()
                            .strip_prefix("content-length: ")
                            .and_then(|value| value.parse::<usize>().ok())
                    })
                    .unwrap_or(0);
                if request.len() >= header_end + 4 + content_length {
                    break;
                }
            }
            sender
                .send(String::from_utf8_lossy(&request).into_owned())
                .unwrap();
            let response = format!(
                "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nContent-Type: application/json\r\nConnection: close\r\n\r\n{}",
                response_body.len(),
                response_body
            );
            stream.write_all(response.as_bytes()).unwrap();
        });
        (format!("http://{address}"), receiver, handle)
    }

    #[test]
    fn authenticated_fetch_uses_cli_headers_without_leaking_the_token() {
        let (url, request, handle) = capture_once(r#"{"config":{}}"#);
        let client = GrokClient::for_test(&url, &url, Duration::from_secs(1));

        let response = client.fetch_credits(" secret-token ").unwrap();

        assert!(response.body.get("config").is_some());
        let request = request.recv_timeout(Duration::from_secs(1)).unwrap();
        let request_lower = request.to_ascii_lowercase();
        assert!(request_lower.contains("authorization: bearer secret-token"));
        assert!(request_lower.contains("x-xai-token-auth: xai-grok-cli"));
        assert!(request_lower.contains("accept: application/json"));
        handle.join().unwrap();
    }

    #[test]
    fn transport_timeout_is_typed() {
        let delayed = test_http::serve_once_after(
            test_http::TIMEOUT_TEST_RESPONSE_DELAY,
            200,
            &[],
            r#"{"config":{}}"#,
        );
        let client = GrokClient::for_test(&delayed, &delayed, test_http::TIMEOUT_TEST_CLIENT_LIMIT);
        assert!(matches!(
            client.fetch_credits("secret"),
            Err(GrokError::ConnectionFailed)
        ));
    }
}
