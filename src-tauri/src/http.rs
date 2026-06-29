use std::time::Duration;

const TIMEOUT: Duration = Duration::from_secs(30);

pub fn client() -> reqwest::Client {
    reqwest::Client::builder()
        .timeout(TIMEOUT)
        .build()
        .unwrap_or_else(|_| reqwest::Client::new())
}
