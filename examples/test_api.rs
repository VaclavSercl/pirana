#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let api_key = std::env::var("BITFINEX_API_KEY").unwrap_or_default();
    let api_secret = std::env::var("BITFINEX_API_SECRET").unwrap_or_default();

    println!("Bitfinex API test utility initialized.");
    println!("API Key provided: {}", !api_key.is_empty());
    println!("API Secret provided: {}", !api_secret.is_empty());
    Ok(())
}
