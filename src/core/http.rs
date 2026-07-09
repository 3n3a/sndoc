//! Minimal blocking HTTP GET helper (reqwest + rustls), used for the `--live`
//! raw-markdown fetch path.

use std::time::Duration;

use crate::core::constants::HTTP_TIMEOUT_S;

/// GET `url`; on a 200 return the body text, otherwise `None`. Follows
/// redirects. Any transport error is treated as a miss (`None`).
pub fn get_text(url: &str) -> Option<String> {
    let client = reqwest::blocking::Client::builder()
        .timeout(Duration::from_secs(HTTP_TIMEOUT_S))
        .build()
        .ok()?;
    let resp = client.get(url).send().ok()?;
    if resp.status().as_u16() != 200 {
        return None;
    }
    resp.text().ok()
}
