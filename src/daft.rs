use anyhow::{Context, Result};
use reqwest::Client;
use serde::{Deserialize, Serialize};
use serde_json::json;

const GATEWAY: &str = "https://gateway.daft.ie/old/v1/listings";
const SITE_BASE: &str = "https://www.daft.ie";
const PAGE_SIZE: u32 = 20;

#[derive(Debug, Clone)]
pub struct DaftQuery {
    pub area_slug: String,
    /// Daft.ie API section name (e.g. `sharing`, `residential-to-rent`).
    /// This is *not* always the URL path segment — see `map_url_section_to_api`.
    pub section: String,
    /// Optional third path segment, e.g. `houses`, `apartments`.
    pub property_type: Option<String>,
    pub rental_price_from: Option<u32>,
    pub rental_price_to: Option<u32>,
}

impl DaftQuery {
    pub fn parse_url(url_str: &str) -> Result<Self> {
        let parsed = url::Url::parse(url_str).context("invalid URL")?;

        let segments: Vec<String> = parsed
            .path_segments()
            .map(|s| s.filter(|x| !x.is_empty()).map(String::from).collect())
            .unwrap_or_default();

        let url_section = segments
            .first()
            .cloned()
            .context("URL path is empty (expected /<section>/<area>)")?;
        let area_slug = segments
            .get(1)
            .cloned()
            .context("URL path missing area slug")?;
        let property_type = segments.get(2).cloned();

        let section = map_url_section_to_api(&url_section).to_string();

        let mut rental_price_from = None;
        let mut rental_price_to = None;

        for (k, v) in parsed.query_pairs() {
            match k.as_ref() {
                "rentalPrice_from" => rental_price_from = v.parse().ok(),
                "rentalPrice_to" => rental_price_to = v.parse().ok(),
                _ => {}
            }
        }

        Ok(Self {
            area_slug,
            section,
            property_type,
            rental_price_from,
            rental_price_to,
        })
    }
}

/// Map the URL-path section to the gateway API's section name.
/// Unknown sections are passed through verbatim.
fn map_url_section_to_api(url_section: &str) -> &str {
    match url_section {
        "property-for-rent" => "residential-to-rent",
        "property-for-sale" => "residential-for-sale",
        "new-homes-for-sale" => "new-homes-for-sale",
        "commercial-property-for-rent" | "commercial-properties-for-rent" => "commercial-to-rent",
        "commercial-property-for-sale" | "commercial-properties-for-sale" => "commercial-for-sale",
        "student-accommodation" => "student-accommodation",
        "short-term-rentals" => "short-term",
        other => other, // sharing, etc.
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct Listing {
    pub id: u64,
    pub title: String,
    pub price: Option<String>,
    pub price_monthly: Option<u32>,
    pub bedrooms: Option<String>,
    pub property_type: Option<String>,
    pub category: Option<String>,
    pub ber_rating: Option<String>,
    pub publish_date: Option<i64>,
    pub seller_name: Option<String>,
    pub seller_phone: Option<String>,
    pub lat: Option<f64>,
    pub lng: Option<f64>,
    pub url: String,
    pub images: Vec<String>,
    pub facilities: Vec<String>,
}

/// Extra fields scraped from the per-listing HTML page (not in the search API).
#[derive(Debug, Clone, Default, Serialize)]
pub struct DetailExtras {
    /// firstPublishDate as Unix epoch milliseconds.
    pub date_listed_ms: Option<i64>,
    /// Cumulative view count shown on the ad page.
    pub views: Option<u64>,
    pub property_overview: Vec<OverviewItem>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OverviewItem {
    pub label: String,
    pub text: String,
}

#[derive(Debug, Deserialize)]
struct ApiResponse {
    listings: Vec<ApiListingWrapper>,
    paging: ApiPaging,
}

#[derive(Debug, Deserialize)]
struct ApiListingWrapper {
    listing: ApiListing,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct ApiListing {
    id: u64,
    title: String,
    price: Option<String>,
    num_bedrooms: Option<String>,
    property_type: Option<String>,
    category: Option<String>,
    publish_date: Option<i64>,
    ber: Option<ApiBer>,
    seller: Option<ApiSeller>,
    point: Option<ApiPoint>,
    media: Option<ApiMedia>,
    seo_friendly_path: Option<String>,
    #[serde(default)]
    facilities: Vec<ApiFacility>,
}

#[derive(Debug, Deserialize)]
struct ApiFacility {
    name: String,
}

#[derive(Debug, Deserialize)]
struct ApiBer {
    rating: Option<String>,
}

#[derive(Debug, Deserialize)]
struct ApiSeller {
    name: Option<String>,
    phone: Option<String>,
}

#[derive(Debug, Deserialize)]
struct ApiPoint {
    coordinates: Option<Vec<f64>>,
}

#[derive(Debug, Deserialize)]
struct ApiMedia {
    #[serde(default)]
    images: Vec<ApiImage>,
}

#[derive(Debug, Deserialize)]
struct ApiImage {
    #[serde(rename = "size720x480")]
    size_720x480: Option<String>,
    #[serde(rename = "size600x600")]
    size_600x600: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct ApiPaging {
    total_results: u32,
    next_from: u32,
}

fn parse_price_monthly(price: &str) -> Option<u32> {
    let digits: String = price
        .chars()
        .skip_while(|c| !c.is_ascii_digit())
        .take_while(|c| c.is_ascii_digit() || *c == ',')
        .filter(|c| *c != ',')
        .collect();
    let num: u32 = digits.parse().ok()?;
    if price.to_lowercase().contains("week") {
        Some(num * 4)
    } else {
        Some(num)
    }
}

pub async fn fetch_all(
    client: &Client,
    query: &DaftQuery,
    max_pages: u32,
) -> Result<Vec<Listing>> {
    let mut all = Vec::new();
    let mut from: u32 = 0;
    let mut pages: u32 = 0;

    loop {
        let resp = fetch_page(client, query, from).await?;
        let total = resp.paging.total_results;
        let got = resp.listings.len() as u32;

        for w in resp.listings {
            let l = convert(w.listing);
            let pass = match (query.rental_price_from, query.rental_price_to, l.price_monthly) {
                (Some(lo), _, Some(p)) if p < lo => false,
                (_, Some(hi), Some(p)) if p > hi => false,
                _ => true,
            };
            if pass {
                all.push(l);
            }
        }
        pages += 1;

        let collected_so_far = from + got;
        if got == 0 || collected_so_far >= total {
            break;
        }
        if max_pages > 0 && pages >= max_pages {
            break;
        }
        from = resp.paging.next_from.max(from + got);
    }

    Ok(all)
}

async fn fetch_page(client: &Client, query: &DaftQuery, from: u32) -> Result<ApiResponse> {
    let mut filters = vec![json!({ "name": "adState", "values": ["published"] })];
    if let Some(pt) = &query.property_type {
        // Daft expects the URL path word verbatim, lower-case plural: "houses",
        // "apartments", "bungalows", etc.
        filters.push(json!({ "name": "propertyType", "values": [pt] }));
    }

    let body = json!({
        "section": query.section,
        "filters": filters,
        "andFilters": [],
        "ranges": [],
        "paging": { "from": from.to_string(), "pageSize": PAGE_SIZE.to_string() },
        "geoFilter": { "storedShapeIds": [], "geoSearchType": "STORED_SHAPES" },
        "terms": query.area_slug,
    });

    let resp = client
        .post(GATEWAY)
        .header("brand", "daft")
        .header("platform", "web")
        .json(&body)
        .send()
        .await
        .context("POST to daft gateway failed")?;

    let status = resp.status();
    if !status.is_success() {
        let text = resp.text().await.unwrap_or_default();
        anyhow::bail!("gateway returned {}: {}", status, text);
    }

    resp.json::<ApiResponse>()
        .await
        .context("failed to decode gateway response")
}

fn convert(api: ApiListing) -> Listing {
    let (lat, lng) = api
        .point
        .as_ref()
        .and_then(|p| p.coordinates.as_ref())
        .filter(|c| c.len() >= 2)
        .map(|c| (Some(c[1]), Some(c[0])))
        .unwrap_or((None, None));

    let images = api
        .media
        .map(|m| {
            m.images
                .into_iter()
                .filter_map(|i| i.size_720x480.or(i.size_600x600))
                .collect()
        })
        .unwrap_or_default();

    let url = api
        .seo_friendly_path
        .as_deref()
        .map(|p| format!("{SITE_BASE}{p}"))
        .unwrap_or_else(|| format!("{SITE_BASE}/share/{}", api.id));

    let (seller_name, seller_phone) = api
        .seller
        .map(|s| (s.name, s.phone))
        .unwrap_or((None, None));

    let price_monthly = api.price.as_deref().and_then(parse_price_monthly);

    let facilities = api.facilities.into_iter().map(|f| f.name).collect();

    Listing {
        id: api.id,
        title: api.title,
        price: api.price,
        price_monthly,
        bedrooms: api.num_bedrooms,
        property_type: api.property_type,
        category: api.category,
        ber_rating: api.ber.and_then(|b| b.rating),
        publish_date: api.publish_date,
        seller_name,
        seller_phone,
        lat,
        lng,
        url,
        images,
        facilities,
    }
}

/// Fetch the per-listing HTML page and extract fields not present in the
/// search API (description and the share-specific "Property Overview" list).
pub async fn fetch_detail(client: &Client, listing_url: &str) -> Result<DetailExtras> {
    let html = client
        .get(listing_url)
        .header("accept", "text/html")
        .send()
        .await
        .context("GET listing page failed")?
        .error_for_status()
        .context("listing page returned non-2xx")?
        .text()
        .await
        .context("reading listing page body")?;

    let json_str = extract_next_data(&html)?;
    let v: serde_json::Value =
        serde_json::from_str(json_str).context("parsing __NEXT_DATA__ JSON")?;

    let listing = v
        .pointer("/props/pageProps/listing")
        .ok_or_else(|| anyhow::anyhow!("no listing in __NEXT_DATA__"))?;

    let date_listed_ms = listing
        .get("firstPublishDate")
        .and_then(|x| x.as_i64())
        .or_else(|| listing.get("publishDate").and_then(|x| x.as_i64()));

    let views = listing
        .get("listingViews")
        .and_then(|x| x.as_u64());

    let property_overview: Vec<OverviewItem> = listing
        .get("propertyOverview")
        .and_then(|x| x.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|item| {
                    let label = item.get("label")?.as_str()?.to_string();
                    let text = item.get("text")?.as_str()?.to_string();
                    Some(OverviewItem { label, text })
                })
                .collect()
        })
        .unwrap_or_default();

    Ok(DetailExtras {
        date_listed_ms,
        views,
        property_overview,
    })
}

fn extract_next_data(html: &str) -> Result<&str> {
    let needle = r#"id="__NEXT_DATA__""#;
    let id_pos = html
        .find(needle)
        .ok_or_else(|| anyhow::anyhow!("__NEXT_DATA__ tag not found in HTML"))?;
    let gt_pos = html[id_pos..]
        .find('>')
        .ok_or_else(|| anyhow::anyhow!("malformed __NEXT_DATA__ open tag"))?
        + id_pos;
    let after_open = gt_pos + 1;
    let end_rel = html[after_open..]
        .find("</script>")
        .ok_or_else(|| anyhow::anyhow!("__NEXT_DATA__ closing tag not found"))?;
    Ok(&html[after_open..after_open + end_rel])
}
