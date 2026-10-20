use std::{fs, path::PathBuf};

use base64::{
    engine::general_purpose::{URL_SAFE, URL_SAFE_NO_PAD},
    Engine,
};
use chrono::{DateTime, Utc};
use serde::Deserialize;
use serde_json::Value;

use super::GrokError;

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
struct GrokAuthEntry {
    key: Option<String>,
    expires_at: Option<String>,
    expires: Option<String>,
}

#[derive(Debug, Clone)]
pub struct GrokAuthState {
    pub token: String,
    expires_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone)]
pub struct GrokAuthStore {
    path: PathBuf,
}

impl GrokAuthStore {
    pub fn new() -> Self {
        Self {
            path: home_directory().join(".grok").join("auth.json"),
        }
    }

    #[cfg(test)]
    pub fn for_path(path: PathBuf) -> Self {
        Self { path }
    }

    pub fn has_local_credentials(&self) -> bool {
        self.load_candidates()
            .is_ok_and(|candidates| !candidates.is_empty())
    }

    // The Grok CLI owns this file. Its writers do not participate in a shared transaction,
    // so TokenLedger must neither rotate these refresh tokens nor overwrite this document.
    pub fn load_candidates(&self) -> Result<Vec<GrokAuthState>, GrokError> {
        if !self.path.is_file() {
            return Err(GrokError::NotLoggedIn);
        }
        let text = fs::read_to_string(&self.path).map_err(|_| GrokError::InvalidAuth)?;
        let document: Value = serde_json::from_str(&text).map_err(|_| GrokError::InvalidAuth)?;
        let entries = document.as_object().ok_or(GrokError::InvalidAuth)?;
        let candidates = entries
            .values()
            .filter_map(|value| {
                let entry = serde_json::from_value::<GrokAuthEntry>(value.clone()).ok()?;
                let token = trimmed(entry.key.as_deref())?.to_owned();
                let expires_at = token_expiry(&token).or_else(|| entry_expiry(&entry));
                Some(GrokAuthState { token, expires_at })
            })
            .collect::<Vec<_>>();
        if candidates.is_empty() {
            Err(GrokError::InvalidAuth)
        } else {
            Ok(candidates)
        }
    }

    pub fn is_expired(&self, state: &GrokAuthState, now: DateTime<Utc>) -> bool {
        state.expires_at.is_some_and(|expiry| now >= expiry)
    }
}

fn entry_expiry(entry: &GrokAuthEntry) -> Option<DateTime<Utc>> {
    trimmed(entry.expires_at.as_deref())
        .or_else(|| trimmed(entry.expires.as_deref()))
        .and_then(|value| DateTime::parse_from_rfc3339(value).ok())
        .map(|value| value.to_utc())
}

fn token_expiry(token: &str) -> Option<DateTime<Utc>> {
    let payload = token.split('.').nth(1)?;
    let bytes = URL_SAFE_NO_PAD
        .decode(payload)
        .or_else(|_| URL_SAFE.decode(payload))
        .ok()?;
    let value: Value = serde_json::from_slice(&bytes).ok()?;
    let seconds = value
        .get("exp")
        .and_then(|value| {
            value
                .as_f64()
                .or_else(|| value.as_str().and_then(|value| value.parse().ok()))
        })
        .filter(|value| value.is_finite())?;
    DateTime::from_timestamp(seconds as i64, 0)
}

fn trimmed(value: Option<&str>) -> Option<&str> {
    value.map(str::trim).filter(|value| !value.is_empty())
}

fn home_directory() -> PathBuf {
    std::env::var_os("HOME")
        .or_else(|| std::env::var_os("USERPROFILE"))
        .map(PathBuf::from)
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use std::fs;

    use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine};
    use chrono::{Duration, TimeZone, Utc};
    use serde_json::json;
    use tempfile::tempdir;

    use super::GrokAuthStore;

    #[test]
    fn loads_all_keyed_accounts_without_changing_the_cli_document() {
        let directory = tempdir().unwrap();
        let path = directory.path().join("auth.json");
        let original = r#"{
          "https://auth.x.ai::client-a":{"key":" token-a ","refresh_token":"refresh-a"},
          "https://auth.x.ai::client-b":{"key":"token-b","unknown":{"keep":true}},
          "empty":{"refresh_token":"refresh-only"}
        }"#;
        fs::write(&path, original).unwrap();
        let store = GrokAuthStore::for_path(path.clone());

        let candidates = store.load_candidates().unwrap();

        assert_eq!(candidates.len(), 2);
        assert_eq!(candidates[0].token, "token-a");
        assert_eq!(candidates[1].token, "token-b");
        assert_eq!(fs::read_to_string(path).unwrap(), original);
    }

    #[test]
    fn jwt_expiry_takes_precedence_over_entry_expiry() {
        let directory = tempdir().unwrap();
        let path = directory.path().join("auth.json");
        let now = Utc.timestamp_opt(1_800_000_000, 0).unwrap();
        let payload = URL_SAFE_NO_PAD.encode(
            serde_json::to_vec(&json!({"exp": (now + Duration::minutes(2)).timestamp()})).unwrap(),
        );
        fs::write(
            &path,
            serde_json::to_vec(&json!({
                "account": {
                    "key": format!("header.{payload}.signature"),
                    "expires_at": (now + Duration::days(1)).to_rfc3339()
                },
                "legacy": {"key": "opaque", "expires": now.to_rfc3339()}
            }))
            .unwrap(),
        )
        .unwrap();
        let store = GrokAuthStore::for_path(path);
        let states = store.load_candidates().unwrap();

        assert!(!store.is_expired(&states[0], now));
        assert!(store.is_expired(&states[0], now + Duration::minutes(3)));
        assert!(store.is_expired(&states[1], now));
    }
}
