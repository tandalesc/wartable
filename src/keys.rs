use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::RwLock;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ApiKey {
    pub name: String,
    pub key: String,
    pub created_at: DateTime<Utc>,
    /// Runtime-generated MCP client keys are revocable. Admin and config keys are not.
    #[serde(default)]
    pub revocable: bool,
}

/// Thread-safe runtime key store. Keys are indexed by their secret value.
#[derive(Debug, Clone)]
pub struct KeyStore {
    inner: Arc<RwLock<HashMap<String, ApiKey>>>,
}

impl KeyStore {
    /// Create a key store with an auto-generated admin key, optionally seeded with config keys.
    /// Returns (store, admin_key_secret).
    pub fn new(config_keys: Vec<crate::config::ApiKeyEntry>) -> (Self, String) {
        let mut map = HashMap::new();

        // Auto-generate admin key for dashboard
        let admin_secret = generate_secret();
        map.insert(admin_secret.clone(), ApiKey {
            name: "admin".into(),
            key: admin_secret.clone(),
            created_at: Utc::now(),
            revocable: false,
        });

        // Seed config keys (also non-revocable)
        for entry in config_keys {
            map.insert(entry.key.clone(), ApiKey {
                name: entry.name,
                key: entry.key,
                created_at: Utc::now(),
                revocable: false,
            });
        }

        (Self { inner: Arc::new(RwLock::new(map)) }, admin_secret)
    }

    /// Look up a key by its secret value. Returns the key name if valid.
    pub async fn validate(&self, secret: &str) -> Option<String> {
        let keys = self.inner.read().await;
        // Constant-time scan: check all keys to avoid timing leaks
        let mut matched = None;
        for (k, entry) in keys.iter() {
            if constant_time_eq(k.as_bytes(), secret.as_bytes()) {
                matched = Some(entry.name.clone());
            }
        }
        matched
    }

    /// Generate a new API key with the given name. Returns the full key info.
    pub async fn generate(&self, name: String) -> ApiKey {
        let secret = generate_secret();
        let key = ApiKey {
            name,
            key: secret.clone(),
            created_at: Utc::now(),
            revocable: true,
        };
        self.inner.write().await.insert(secret, key.clone());
        key
    }

    /// Revoke a key by name. Only revocable (runtime-generated) keys can be revoked.
    pub async fn revoke(&self, name: &str) -> Result<bool, &'static str> {
        let mut keys = self.inner.write().await;
        let entry = keys.iter().find(|(_, v)| v.name == name).map(|(k, v)| (k.clone(), v.revocable));
        match entry {
            Some((_, false)) => Err("Cannot revoke admin or config key"),
            Some((secret, true)) => {
                keys.remove(&secret);
                Ok(true)
            }
            None => Ok(false),
        }
    }

    /// List all keys (secrets are masked).
    pub async fn list(&self) -> Vec<ApiKeyInfo> {
        let keys = self.inner.read().await;
        let mut list: Vec<_> = keys.values().map(|k| ApiKeyInfo {
            name: k.name.clone(),
            created_at: k.created_at,
            revocable: k.revocable,
            key_prefix: format!("{}...", &k.key[..8.min(k.key.len())]),
        }).collect();
        list.sort_by(|a, b| a.created_at.cmp(&b.created_at));
        list
    }
}

#[derive(Debug, Serialize)]
pub struct ApiKeyInfo {
    pub name: String,
    pub created_at: DateTime<Utc>,
    pub revocable: bool,
    pub key_prefix: String,
}

fn generate_secret() -> String {
    let bytes: Vec<u8> = (0..32).map(|_| rand::random::<u8>()).collect();
    hex::encode(bytes)
}

fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    a.iter().zip(b.iter()).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}
