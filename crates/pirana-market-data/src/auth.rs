use hmac::{Hmac, Mac};
use sha2::Sha384;
use zeroize::Zeroize;

type HmacSha384 = Hmac<Sha384>;

/// Bitfinex API authentication handler
#[derive(Zeroize)]
pub struct BitfinexAuth {
    #[zeroize(skip)]
    api_key: String,
    api_secret: String,
}

impl BitfinexAuth {
    pub fn new(api_key: String, api_secret: String) -> Self {
        Self { api_key, api_secret }
    }

    pub fn api_key(&self) -> &str {
        &self.api_key
    }

    /// Sign a payload with the API secret
    pub fn sign(&self, payload: &str) -> String {
        let mut mac = HmacSha384::new_from_slice(self.api_secret.as_bytes())
            .expect("HMAC can take key of any size");
        mac.update(payload.as_bytes());
        let result = mac.finalize();
        hex::encode(result.into_bytes())
    }

    /// Generate authentication headers for REST API v2
    pub fn generate_headers(&self, path: &str, nonce: &str, body: &str) -> Vec<(String, String)> {
        let payload = format!("{}{}{}", path, nonce, body);
        let signature = self.sign(&payload);

        vec![
            ("bfx-apikey".to_string(), self.api_key.clone()),
            ("bfx-nonce".to_string(), nonce.to_string()),
            ("bfx-signature".to_string(), signature),
            ("Content-Type".to_string(), "application/json".to_string()),
        ]
    }
}
