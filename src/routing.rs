//! Google Distance Matrix API client. One small wrapper per travel mode.
//! Costs ~$0.005 per call; free $200/month covers ≈40k calls.

use anyhow::{Context, Result};
use reqwest::Client;
use serde::Deserialize;

const API: &str = "https://maps.googleapis.com/maps/api/distancematrix/json";

#[derive(Debug, Clone, Copy)]
pub enum Mode {
    Driving,
    Walking,
    Bicycling,
    Transit,
}

impl Mode {
    fn as_str(&self) -> &'static str {
        match self {
            Mode::Driving => "driving",
            Mode::Walking => "walking",
            Mode::Bicycling => "bicycling",
            Mode::Transit => "transit",
        }
    }

    /// Emoji shown on the Telegram caption line.
    pub fn emoji(&self) -> &'static str {
        match self {
            Mode::Driving => "🚗",
            Mode::Walking => "🚶",
            Mode::Bicycling => "🚴",
            Mode::Transit => "🚌",
        }
    }
}

#[derive(Debug, Clone, Default)]
pub struct CommuteTimes {
    pub driving: Option<u32>,
    pub walking: Option<u32>,
    pub bicycling: Option<u32>,
    pub transit: Option<u32>,
}

impl CommuteTimes {
    pub fn any(&self) -> bool {
        self.driving.is_some()
            || self.walking.is_some()
            || self.bicycling.is_some()
            || self.transit.is_some()
    }
}

#[derive(Deserialize)]
struct ApiResp {
    status: String,
    #[serde(default)]
    rows: Vec<ApiRow>,
    #[serde(default)]
    error_message: Option<String>,
}

#[derive(Deserialize)]
struct ApiRow {
    elements: Vec<ApiElement>,
}

#[derive(Deserialize)]
struct ApiElement {
    status: String,
    duration: Option<ApiDuration>,
}

#[derive(Deserialize)]
struct ApiDuration {
    /// seconds
    value: u32,
}

async fn fetch_one(
    client: &Client,
    api_key: &str,
    origin: (f64, f64),
    destination: (f64, f64),
    mode: Mode,
) -> Result<u32> {
    let url = format!(
        "{API}?origins={lat_o},{lng_o}&destinations={lat_d},{lng_d}&mode={m}&key={k}",
        lat_o = origin.0,
        lng_o = origin.1,
        lat_d = destination.0,
        lng_d = destination.1,
        m = mode.as_str(),
        k = api_key,
    );
    let resp: ApiResp = client
        .get(&url)
        .send()
        .await
        .context("distance-matrix request")?
        .error_for_status()
        .context("distance-matrix non-2xx")?
        .json()
        .await
        .context("decoding distance-matrix json")?;

    if resp.status != "OK" {
        anyhow::bail!(
            "API status {}: {}",
            resp.status,
            resp.error_message.unwrap_or_default()
        );
    }
    let row = resp.rows.into_iter().next().context("no row in response")?;
    let element = row.elements.into_iter().next().context("no element in row")?;
    if element.status != "OK" {
        anyhow::bail!("element status: {}", element.status);
    }
    Ok(element
        .duration
        .context("element has no duration")?
        .value)
}

/// Run all 4 modes in parallel. Each individual failure becomes `None` so the
/// caller can still render the others.
pub async fn fetch_all(
    client: &Client,
    api_key: &str,
    origin: (f64, f64),
    destination: (f64, f64),
) -> CommuteTimes {
    let (d, w, b, t) = tokio::join!(
        fetch_one(client, api_key, origin, destination, Mode::Driving),
        fetch_one(client, api_key, origin, destination, Mode::Walking),
        fetch_one(client, api_key, origin, destination, Mode::Bicycling),
        fetch_one(client, api_key, origin, destination, Mode::Transit),
    );

    for (mode, r) in [("drive", &d), ("walk", &w), ("bike", &b), ("transit", &t)] {
        if let Err(e) = r {
            log::debug!("[routing] {mode} mode failed: {e}");
        }
    }

    CommuteTimes {
        driving: d.ok(),
        walking: w.ok(),
        bicycling: b.ok(),
        transit: t.ok(),
    }
}

/// "25 min" / "1h 32m" / "2h"
pub fn format_duration(secs: u32) -> String {
    let mins = (secs as f64 / 60.0).round() as u32;
    if mins < 60 {
        format!("{mins} min")
    } else {
        let h = mins / 60;
        let m = mins % 60;
        if m == 0 {
            format!("{h}h")
        } else {
            format!("{h}h {m}m")
        }
    }
}
