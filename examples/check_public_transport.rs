//! Read-only public TLS probe. No configuration, credentials or order endpoints.
use std::error::Error;
#[tokio::main]
async fn main() {
    for rustls in [false, true] {
        let mut builder = reqwest::Client::builder().timeout(std::time::Duration::from_secs(12));
        if rustls { builder = builder.use_rustls_tls(); }
        let client = builder.build().expect("public client construction");
        match client.get("https://api.bitfinex.com/v2/platform/status").send().await {
            Ok(response) => println!("backend={} http={}", if rustls {"rustls"} else {"default"}, response.status()),
            Err(error) => {
                println!("backend={} error={error}", if rustls {"rustls"} else {"default"});
                let mut cause = error.source();
                while let Some(e) = cause { println!("cause={e}"); cause = e.source(); }
            }
        }
    }
}
