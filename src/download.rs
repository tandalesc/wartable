use hmac::{Hmac, Mac};
use sha2::Sha256;
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

type HmacSha256 = Hmac<Sha256>;

const DEFAULT_TTL_SECS: u64 = 900; // 15 minutes

#[derive(Clone, Debug)]
pub struct DownloadSigner {
    key: Arc<Vec<u8>>,
    base_url: Arc<String>,
}

impl DownloadSigner {
    pub fn new(base_url: String) -> Self {
        let key: Vec<u8> = (0..32).map(|_| rand::random::<u8>()).collect();
        Self {
            key: Arc::new(key),
            base_url: Arc::new(base_url),
        }
    }

    /// Generate a presigned download URL for the given file path.
    pub fn sign(&self, path: &str) -> String {
        let exp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_secs()
            + DEFAULT_TTL_SECS;

        let sig = self.compute_sig(path, exp);
        let encoded_path = urlencoding::encode(path);
        format!("{}/api/dl?path={}&exp={}&sig={}", self.base_url, encoded_path, exp, sig)
    }

    /// Verify a presigned token. Returns the file path if valid.
    pub fn verify(&self, path: &str, exp: u64, sig: &str) -> Result<(), &'static str> {
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_secs();
        if now > exp {
            return Err("Token expired");
        }

        let expected = self.compute_sig(path, exp);
        if !constant_time_eq(sig.as_bytes(), expected.as_bytes()) {
            return Err("Invalid signature");
        }

        Ok(())
    }

    fn compute_sig(&self, path: &str, exp: u64) -> String {
        let mut mac = HmacSha256::new_from_slice(&self.key).unwrap();
        mac.update(format!("{}|{}", path, exp).as_bytes());
        hex::encode(mac.finalize().into_bytes())
    }
}

fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    a.iter().zip(b.iter()).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}
