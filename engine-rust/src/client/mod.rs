pub mod neo4j;
pub mod qdrant;
pub mod embed;

use lazy_static::lazy_static;
use std::time::Duration;

lazy_static! {
    static ref HTTP_CLIENT: reqwest::Client = reqwest::Client::builder()
        .pool_max_idle_per_host(10)
        .pool_idle_timeout(Duration::from_secs(90))
        .timeout(Duration::from_secs(300))
        .build()
        .expect("Failed to create HTTP client");
}

pub fn get_client() -> &'static reqwest::Client {
    &HTTP_CLIENT
}
