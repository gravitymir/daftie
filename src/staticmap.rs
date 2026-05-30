//! Static-map URL builder. Uses Google Maps Static API; the same API key the
//! routing module already uses is reused here (requires "Maps Static API"
//! enabled on the project and added to the key's API restrictions).

const ENDPOINT: &str = "https://maps.googleapis.com/maps/api/staticmap";
const MAX_MARKERS: usize = 50;

/// Returns `(center_lat, center_lng, zoom)` that roughly fits all points.
fn compute_view(points: &[(f64, f64)]) -> (f64, f64, u8) {
    if points.is_empty() {
        return (53.35, -6.26, 7); // Ireland default
    }
    let lat_min = points.iter().map(|p| p.0).fold(f64::INFINITY, f64::min);
    let lat_max = points.iter().map(|p| p.0).fold(f64::NEG_INFINITY, f64::max);
    let lng_min = points.iter().map(|p| p.1).fold(f64::INFINITY, f64::min);
    let lng_max = points.iter().map(|p| p.1).fold(f64::NEG_INFINITY, f64::max);
    let center_lat = (lat_min + lat_max) / 2.0;
    let center_lng = (lng_min + lng_max) / 2.0;

    let span = (lat_max - lat_min).max(lng_max - lng_min);
    let zoom: u8 = match span {
        s if s > 1.0 => 8,
        s if s > 0.5 => 9,
        s if s > 0.2 => 10,
        s if s > 0.1 => 11,
        s if s > 0.05 => 12,
        s if s > 0.02 => 13,
        s if s > 0.01 => 14,
        _ => 15,
    };
    (center_lat, center_lng, zoom)
}

/// Build a Google Static Maps URL with one red pin per listing point and an
/// optional blue pin for the work point. Returns None if no API key was
/// provided (Google's static-maps endpoint requires `&key=`).
pub fn build_url(
    listings: &[(f64, f64)],
    work_point: Option<(f64, f64)>,
    width: u32,
    height: u32,
    api_key: &str,
) -> String {
    // Combined list for centering, separate marker sets for colours.
    let mut all_points: Vec<(f64, f64)> = listings.to_vec();
    if let Some(wp) = work_point {
        all_points.push(wp);
    }
    let (center_lat, center_lng, zoom) = compute_view(&all_points);

    // Google's marker syntax: `markers=color:red|size:small|lat,lng|lat,lng|...`
    let red_coords: String = listings
        .iter()
        .take(MAX_MARKERS)
        .map(|(lat, lng)| format!("|{lat},{lng}"))
        .collect();
    let mut url = format!(
        "{ENDPOINT}?center={center_lat},{center_lng}&zoom={zoom}&size={width}x{height}&maptype=roadmap&markers=color:red|size:small{red_coords}",
    );
    if let Some((lat, lng)) = work_point {
        url.push_str(&format!("&markers=color:blue|label:W|{lat},{lng}"));
    }
    url.push_str(&format!("&key={api_key}"));
    url
}
