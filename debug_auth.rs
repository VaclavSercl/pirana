use std::env;
fn main() {
    println!("Debug Env:");
    if let Ok(key) = env::var("BITFINEX_API_KEY") {
        println!("API_KEY: '{}' (len: {})", key, key.len());
    } else {
        println!("API_KEY not found in env");
    }
    if let Ok(secret) = env::var("BITFINEX_API_SECRET") {
        println!("API_SECRET: '{}' (len: {})", secret, secret.len());
    } else {
        println!("API_SECRET not found in env");
    }
}
