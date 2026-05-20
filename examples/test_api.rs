use hmac::{Hmac, Mac};
use sha2::Sha384;

type HmacSha384 = Hmac<Sha384>;

fn main() {
    let api_key = "399f897a2225ac01c89bc119d061d5fff811f484e67";
    let api_secret = "8a87b584abffebb1a61606d758cf81107dff981b849";

    let nonce = chrono::Utc::now().timestamp_millis().to_string();
    let endpoint = "/v2/auth/r/wallets";
    let body = "{}";
    let payload = format!("{}{}{}", endpoint, nonce, body);

    let mut mac = HmacSha384::new_from_slice(api_secret.as_bytes()).unwrap();
    mac.update(payload.as_bytes());
    let signature = hex::encode(mac.finalize().into_bytes());

    println!("Key: {}...", &api_key[..10]);
    println!("Secret: {}...", &api_secret[..10]);
    println!("Nonce: {}", nonce);
    println!("Payload: {}", payload);
    println!("Signature: {}...", &signature[..20]);

    let client = reqwest::blocking::Client::new();
    let resp = client
        .post(format!("https://api.bitfinex.com{}", endpoint))
        .header("Content-Type", "application/json")
        .header("bfx-apikey", api_key)
        .header("bfx-nonce", &nonce)
        .header("bfx-signature", &signature)
        .body(body.to_string())
        .send();

    match resp {
        Ok(r) => {
            println!("Status: {}", r.status());
            println!("Body: {}", r.text().unwrap_or_default());
        }
        Err(e) => println!("Error: {}", e),
    }
}
