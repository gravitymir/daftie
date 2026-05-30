use crate::daft::{self, DaftQuery, DetailExtras, Listing};
use crate::routing::{self, CommuteTimes, Mode};
use crate::sleep;
use crate::state::State;
use crate::staticmap;
use reqwest::Client as HttpClient;
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use teloxide::{
    prelude::*,
    types::{InputFile, InputMedia, InputMediaPhoto, ParseMode},
    utils::command::BotCommands,
};
use tokio::sync::Mutex;

pub type SharedState = Arc<Mutex<State>>;
pub type SharedKey = Arc<Option<String>>;

/// Pause between two outgoing messages to keep under Telegram's per-chat rate limit.
const SEND_GAP: Duration = Duration::from_millis(500);

#[derive(BotCommands, Clone)]
#[command(rename_rule = "snake_case", description = "Daft.ie sharing watcher.")]
pub enum Cmd {
    #[command(description = "show help")]
    Start,
    #[command(description = "show help")]
    Help,
    #[command(description = "add a daft.ie URL: /watch <url>")]
    Watch(String),
    #[command(description = "remove a watched URL: /unwatch <url-or-index>")]
    Unwatch(String),
    #[command(description = "show current settings and watched URLs")]
    Status,
    #[command(description = "force a check now")]
    Check,
    #[command(description = "resume periodic checks")]
    StartWatching,
    #[command(description = "stop periodic checks")]
    StopWatching,
    #[command(description = "set sleep window: /sleep_time HH:MM-HH:MM or H-H, or 'off'")]
    SleepTime(String),
    #[command(description = "only send ads that have a phone number: /filter_phone true|false")]
    FilterPhone(String),
    #[command(description = "set work point: /work_point <lat>,<lng>, or 'off'")]
    WorkPoint(String),
    #[command(description = "send a map image with all current ad locations")]
    Map,
    #[command(description = "forget all sent ad IDs (next check re-sends every ad)")]
    ClearHistory,
}

fn user_label(msg: &Message) -> String {
    msg.from
        .as_ref()
        .map(|u| {
            u.username
                .as_deref()
                .map(|n| format!("@{n}"))
                .unwrap_or_else(|| u.first_name.clone())
        })
        .unwrap_or_else(|| "?".into())
}

fn cmd_label(cmd: &Cmd) -> &'static str {
    match cmd {
        Cmd::Start => "/start",
        Cmd::Help => "/help",
        Cmd::Watch(_) => "/watch",
        Cmd::Unwatch(_) => "/unwatch",
        Cmd::Status => "/status",
        Cmd::Check => "/check",
        Cmd::StartWatching => "/start_watching",
        Cmd::StopWatching => "/stop_watching",
        Cmd::SleepTime(_) => "/sleep_time",
        Cmd::FilterPhone(_) => "/filter_phone",
        Cmd::WorkPoint(_) => "/work_point",
        Cmd::Map => "/map",
        Cmd::ClearHistory => "/clear_history",
    }
}

/// Multi-line state summary used in /watch, /status and /start_watching replies.
/// All output is plain text (no HTML), safe to send without parse_mode.
/// Renders an HTML-formatted state summary. Must be sent with `ParseMode::Html`.
fn render_state_summary(s: &State, chat_id: i64, current_url: Option<&str>) -> String {
    let c = s.chat_get(chat_id);
    let active = c.map(|c| c.active).unwrap_or(true);
    let sleep_cfg = c.and_then(|c| c.sleep.as_deref());
    let filter_phone = c.map(|c| c.filter_phone).unwrap_or(false);
    let watch_count = c.map(|c| c.watches.len()).unwrap_or(0);

    let mut out = String::new();
    if let Some(url) = current_url {
        out.push_str(&format!("🔗 {}\n", html_escape(url)));
    }
    out.push_str(&format!(
        "Watcher: <b>{}</b>\n",
        if active { "ACTIVE" } else { "STOPPED" }
    ));
    match sleep_cfg {
        Some(win) => {
            if sleep::is_sleeping(Some(win)) {
                let until = sleep::parse(win)
                    .map(|(_, end)| end.format("%H:%M").to_string())
                    .unwrap_or_default();
                out.push_str(&format!(
                    "😴 Sleep: <b>{}</b> (sleeping now, until {})\n",
                    html_escape(win),
                    html_escape(&until)
                ));
            } else {
                out.push_str(&format!("😴 Sleep: <b>{}</b>\n", html_escape(win)));
            }
        }
        None => out.push_str("😴 Sleep: <b>off</b>\n"),
    }
    out.push_str(&format!(
        "🕐 Bot time now: {}\n",
        chrono::Local::now().format("%H:%M:%S %z")
    ));
    out.push_str(&format!(
        "📞 Phone filter: <b>{}</b>\n",
        if filter_phone { "on" } else { "off" }
    ));
    match c.and_then(|c| c.work_point) {
        Some([lat, lng]) => out.push_str(&format!(
            "🏢 Work point: <a href=\"https://www.google.com/maps?q={lat},{lng}&amp;z=13\">{lat:.5},{lng:.5}</a>\n",
        )),
        None => out.push_str("🏢 Work point: <b>off</b>\n"),
    }
    out.push_str(&format!("Watches: <b>{watch_count}</b>\n"));
    out.push_str(&format!("Sent ads recorded: <b>{}</b>", s.seen_count()));
    out
}

fn parse_bool_arg(s: &str) -> Option<bool> {
    match s.trim().to_lowercase().as_str() {
        "true" | "on" | "1" | "yes" | "y" => Some(true),
        "false" | "off" | "0" | "no" | "n" => Some(false),
        _ => None,
    }
}

fn has_phone(l: &crate::daft::Listing) -> bool {
    l.seller_phone
        .as_deref()
        .map(|s| !s.trim().is_empty())
        .unwrap_or(false)
}

/// Parse "lat,lng" or "lat lng" (with optional whitespace) into a coordinate pair.
fn parse_coords(s: &str) -> Result<(f64, f64), String> {
    let s = s.trim().trim_matches(|c| c == '(' || c == ')');
    let parts: Vec<&str> = s
        .split(|c: char| c == ',' || c.is_whitespace())
        .filter(|p| !p.is_empty())
        .collect();
    if parts.len() != 2 {
        return Err("expected '<lat>,<lng>'".into());
    }
    let lat: f64 = parts[0]
        .parse()
        .map_err(|_| format!("can't parse latitude '{}'", parts[0]))?;
    let lng: f64 = parts[1]
        .parse()
        .map_err(|_| format!("can't parse longitude '{}'", parts[1]))?;
    if !(-90.0..=90.0).contains(&lat) || !(-180.0..=180.0).contains(&lng) {
        return Err(format!("lat/lng out of range: {lat},{lng}"));
    }
    Ok((lat, lng))
}

pub async fn handle_cmd(
    bot: Bot,
    msg: Message,
    cmd: Cmd,
    state: SharedState,
    http: HttpClient,
    google_key: SharedKey,
) -> ResponseResult<()> {
    let chat_id = msg.chat.id.0;
    log::info!(
        "[cmd] chat={chat_id} user={} -> {}",
        user_label(&msg),
        cmd_label(&cmd)
    );

    match cmd {
        Cmd::Start | Cmd::Help => {
            let help = format!(
                "{}\n\nOr just paste a daft.ie URL.\nI check every 10 minutes and DM new listings.",
                Cmd::descriptions()
            );
            bot.send_message(msg.chat.id, help).await?;
        }
        Cmd::Watch(url) => {
            let url = url.trim();
            if url.is_empty() {
                bot.send_message(msg.chat.id, "Usage: /watch <daft.ie URL>")
                    .await?;
                return Ok(());
            }
            do_watch(&bot, msg.chat.id, url, &state, &http, &google_key).await?;
        }
        Cmd::Unwatch(arg) => {
            let arg = arg.trim();
            let removed = {
                let mut s = state.lock().await;
                let r = s.remove_watch(chat_id, arg);
                if r.is_some() {
                    let _ = s.save().await;
                }
                r
            };
            let reply = match removed {
                Some(url) => format!("Stopped watching:\n{}", url),
                None => "Not found. Use /status to see your watches.".to_string(),
            };
            bot.send_message(msg.chat.id, reply).await?;
        }
        Cmd::Status => {
            let text = {
                let s = state.lock().await;
                match s.chat_get(chat_id) {
                    Some(c) if !c.watches.is_empty() => {
                        let mut out = render_state_summary(&s, chat_id, None);
                        out.push_str("\n\nURLs:\n");
                        for (i, w) in c.watches.iter().enumerate() {
                            out.push_str(&format!("{}. {}\n", i + 1, html_escape(&w.url)));
                        }
                        out
                    }
                    _ => "No watches yet. Send me a daft.ie URL.".to_string(),
                }
            };
            bot.send_message(msg.chat.id, text)
                .parse_mode(ParseMode::Html)
                .await?;
        }
        Cmd::Check => {
            // Respect sleep here too — manual check shouldn't ignore it.
            let sleeping = {
                let s = state.lock().await;
                sleep::is_sleeping(s.chat_get(chat_id).and_then(|c| c.sleep.as_deref()))
            };
            if sleeping {
                bot.send_message(msg.chat.id, "Sleeping now. Use /sleep_time off to disable.")
                    .await?;
                return Ok(());
            }

            bot.send_message(msg.chat.id, "Checking now…").await?;
            let urls: Vec<String> = {
                let s = state.lock().await;
                s.chat_get(chat_id)
                    .map(|c| c.watches.iter().map(|w| w.url.clone()).collect())
                    .unwrap_or_default()
            };
            for url in urls {
                fetch_and_send_new(&bot, ChatId(chat_id), &url, &state, &http, &google_key).await;
            }
            bot.send_message(msg.chat.id, "Done.").await?;
        }
        Cmd::StartWatching => {
            let was_active = {
                let mut s = state.lock().await;
                let chat = s.chat_mut(chat_id);
                let prev = chat.active;
                chat.active = true;
                let _ = s.save().await;
                prev
            };
            let header = if was_active {
                "Already watching"
            } else {
                "Watching resumed"
            };
            let summary = {
                let s = state.lock().await;
                render_state_summary(&s, chat_id, None)
            };
            bot.send_message(msg.chat.id, format!("{header}\n\n{summary}"))
                .parse_mode(ParseMode::Html)
                .await?;
        }
        Cmd::StopWatching => {
            let was_active = {
                let mut s = state.lock().await;
                let chat = s.chat_mut(chat_id);
                let prev = chat.active;
                chat.active = false;
                let _ = s.save().await;
                prev
            };
            let header = if was_active {
                "Watching stopped"
            } else {
                "Already stopped"
            };
            let summary = {
                let s = state.lock().await;
                render_state_summary(&s, chat_id, None)
            };
            bot.send_message(msg.chat.id, format!("{header}\n\n{summary}"))
                .parse_mode(ParseMode::Html)
                .await?;
        }
        Cmd::SleepTime(arg) => {
            let arg = arg.trim();

            // Empty: show current.
            if arg.is_empty() {
                let summary = {
                    let s = state.lock().await;
                    render_state_summary(&s, chat_id, None)
                };
                bot.send_message(
                    msg.chat.id,
                    format!(
                        "Current settings\n\n{summary}\n\nUsage:\n  /sleep_time 23:00-09:00\n  /sleep_time off"
                    ),
                )
                .parse_mode(ParseMode::Html)
                .await?;
                return Ok(());
            }

            // Disable.
            let lower = arg.to_lowercase();
            if matches!(lower.as_str(), "off" | "none" | "-" | "0" | "clear") {
                {
                    let mut s = state.lock().await;
                    s.chat_mut(chat_id).sleep = None;
                    let _ = s.save().await;
                }
                let summary = {
                    let s = state.lock().await;
                    render_state_summary(&s, chat_id, None)
                };
                bot.send_message(msg.chat.id, format!("Sleep cleared\n\n{summary}"))
                    .parse_mode(ParseMode::Html)
                    .await?;
                return Ok(());
            }

            // Set.
            match sleep::canonical(arg) {
                Ok(canon) => {
                    {
                        let mut s = state.lock().await;
                        s.chat_mut(chat_id).sleep = Some(canon.clone());
                        let _ = s.save().await;
                    }
                    let summary = {
                        let s = state.lock().await;
                        render_state_summary(&s, chat_id, None)
                    };
                    bot.send_message(
                        msg.chat.id,
                        format!("Sleep set to {canon}\n\n{summary}"),
                    )
                    .parse_mode(ParseMode::Html)
                    .await?;
                }
                Err(e) => {
                    bot.send_message(
                        msg.chat.id,
                        format!(
                            "Bad format: {e}\nExamples:\n  /sleep_time 23:00-09:00\n  /sleep_time 21:00-11:00\n  /sleep_time off"
                        ),
                    )
                    .await?;
                }
            }
        }
        Cmd::FilterPhone(arg) => {
            let arg = arg.trim();
            if arg.is_empty() {
                let summary = {
                    let s = state.lock().await;
                    render_state_summary(&s, chat_id, None)
                };
                bot.send_message(
                    msg.chat.id,
                    format!(
                        "Current settings\n\n{summary}\n\nUsage:\n  /filter_phone true\n  /filter_phone false"
                    ),
                )
                .parse_mode(ParseMode::Html)
                .await?;
                return Ok(());
            }

            let Some(value) = parse_bool_arg(arg) else {
                bot.send_message(
                    msg.chat.id,
                    "Bad value. Use /filter_phone true or /filter_phone false.",
                )
                .await?;
                return Ok(());
            };

            {
                let mut s = state.lock().await;
                s.chat_mut(chat_id).filter_phone = value;
                let _ = s.save().await;
            }
            let header = if value {
                "Phone filter ON — ads with no phone will be skipped"
            } else {
                "Phone filter OFF — every ad will be sent"
            };
            let summary = {
                let s = state.lock().await;
                render_state_summary(&s, chat_id, None)
            };
            bot.send_message(msg.chat.id, format!("{header}\n\n{summary}"))
                .parse_mode(ParseMode::Html)
                .await?;
        }
        Cmd::WorkPoint(arg) => {
            let arg = arg.trim();
            if arg.is_empty() {
                let summary = {
                    let s = state.lock().await;
                    render_state_summary(&s, chat_id, None)
                };
                bot.send_message(
                    msg.chat.id,
                    format!(
                        "Current settings\n\n{summary}\n\nUsage:\n  /work_point 51.8985,-8.4756\n  /work_point off"
                    ),
                )
                .parse_mode(ParseMode::Html)
                .await?;
                return Ok(());
            }

            let lower = arg.to_lowercase();
            if matches!(lower.as_str(), "off" | "none" | "-" | "0" | "clear") {
                {
                    let mut s = state.lock().await;
                    s.chat_mut(chat_id).work_point = None;
                    let _ = s.save().await;
                }
                let summary = {
                    let s = state.lock().await;
                    render_state_summary(&s, chat_id, None)
                };
                bot.send_message(msg.chat.id, format!("Work point cleared\n\n{summary}"))
                    .parse_mode(ParseMode::Html)
                    .await?;
                return Ok(());
            }

            match parse_coords(arg) {
                Ok((lat, lng)) => {
                    {
                        let mut s = state.lock().await;
                        s.chat_mut(chat_id).work_point = Some([lat, lng]);
                        let _ = s.save().await;
                    }
                    let warn = if google_key.is_none() {
                        "\n\n⚠ GOOGLE_MAPS_API_KEY is empty in .env — saved the point, but commute times won't be calculated."
                    } else {
                        ""
                    };
                    let summary = {
                        let s = state.lock().await;
                        render_state_summary(&s, chat_id, None)
                    };
                    bot.send_message(
                        msg.chat.id,
                        format!("Work point set to {lat:.5},{lng:.5}{warn}\n\n{summary}"),
                    )
                    .parse_mode(ParseMode::Html)
                    .await?;
                }
                Err(e) => {
                    bot.send_message(
                        msg.chat.id,
                        format!(
                            "Bad coordinates: {e}\n\nTip: right-click your office on Google Maps, click the lat,lng to copy, paste here:\n  /work_point 51.8985,-8.4756"
                        ),
                    )
                    .await?;
                }
            }
        }
        Cmd::Map => {
            handle_map(&bot, msg.chat.id, &state, &http, &google_key).await?;
        }
        Cmd::ClearHistory => {
            let removed = {
                let mut s = state.lock().await;
                let n = s.clear_seen();
                if let Err(e) = s.save().await {
                    log::warn!("[clear] save failed: {e}");
                }
                n
            };
            log::info!("[clear] chat={chat_id} removed={removed} entries");
            bot.send_message(
                msg.chat.id,
                format!(
                    "Cleared {removed} ad ID(s) from state. The next check will re-send every current listing."
                ),
            )
            .await?;
        }
    }

    Ok(())
}

pub async fn handle_text(
    bot: Bot,
    msg: Message,
    state: SharedState,
    http: HttpClient,
    google_key: SharedKey,
) -> ResponseResult<()> {
    let chat_id = msg.chat.id.0;

    // Telegram "Send Location" attachment → set as work point.
    if let Some(loc) = msg.location() {
        let lat = loc.latitude;
        let lng = loc.longitude;
        log::info!(
            "[loc] chat={chat_id} user={} -> work_point {lat},{lng}",
            user_label(&msg)
        );

        {
            let mut s = state.lock().await;
            s.chat_mut(chat_id).work_point = Some([lat, lng]);
            let _ = s.save().await;
        }
        let warn = if google_key.is_none() {
            "\n\n⚠ GOOGLE_MAPS_API_KEY is empty in .env — saved the point, but commute times won't be calculated."
        } else {
            ""
        };
        let summary = {
            let s = state.lock().await;
            render_state_summary(&s, chat_id, None)
        };
        bot.send_message(
            msg.chat.id,
            format!("Work point set from your shared location: {lat:.5},{lng:.5}{warn}\n\n{summary}"),
        )
        .parse_mode(ParseMode::Html)
        .await?;
        return Ok(());
    }

    let text = msg.text().unwrap_or("").trim();
    log::info!(
        "[msg] chat={chat_id} user={} text={:?}",
        user_label(&msg),
        text
    );

    if (text.starts_with("http://") || text.starts_with("https://")) && text.contains("daft.ie") {
        do_watch(&bot, msg.chat.id, text, &state, &http, &google_key).await?;
    } else {
        bot.send_message(
            msg.chat.id,
            "Send a daft.ie URL, share a Location (sets work point), or use /help.",
        )
        .await?;
    }
    Ok(())
}

async fn do_watch(
    bot: &Bot,
    chat: ChatId,
    url: &str,
    state: &SharedState,
    http: &HttpClient,
    google_key: &SharedKey,
) -> ResponseResult<()> {
    if let Err(e) = DaftQuery::parse_url(url) {
        log::warn!("[watch] chat={} bad url {url:?}: {e}", chat.0);
        bot.send_message(chat, format!("Bad URL: {e}")).await?;
        return Ok(());
    }

    let added = {
        let mut s = state.lock().await;
        let added = s.add_watch(chat.0, url.to_string());
        if added {
            if let Some(w) = s
                .chat_mut(chat.0)
                .watches
                .iter_mut()
                .find(|w| w.url == url)
            {
                w.last_check = now();
            }
            let _ = s.save().await;
        }
        added
    };
    {
        let q = DaftQuery::parse_url(url).ok();
        log::info!(
            "[watch] chat={} url={url} (new={added}) section={:?} type={:?}",
            chat.0,
            q.as_ref().map(|q| q.section.clone()),
            q.as_ref().and_then(|q| q.property_type.clone())
        );
    }

    let summary = {
        let s = state.lock().await;
        render_state_summary(&s, chat.0, Some(url))
    };
    bot.send_message(chat, format!("Watching is started\n\n{summary}"))
        .parse_mode(ParseMode::Html)
        .await?;

    // If we're inside the sleep window, defer the first batch.
    let sleeping = {
        let s = state.lock().await;
        sleep::is_sleeping(s.chat_get(chat.0).and_then(|c| c.sleep.as_deref()))
    };
    if sleeping {
        log::info!(
            "[watch] chat={} sleeping; deferring initial fetch",
            chat.0
        );
        return Ok(());
    }

    fetch_and_send_new(bot, chat, url, state, http, google_key).await;
    Ok(())
}

fn now() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

/// Fetch listings for a URL and DM every one whose ID is not yet in
/// `state.seen_ids`. Each successful send marks the listing seen and persists
/// state to disk. Used both for an initial `/watch` and for periodic polls.
async fn fetch_and_send_new(
    bot: &Bot,
    chat: ChatId,
    url: &str,
    state: &SharedState,
    http: &HttpClient,
    google_key: &SharedKey,
) {
    // Re-check sleep right before doing real work — the poller may have raced.
    let sleeping = {
        let s = state.lock().await;
        sleep::is_sleeping(s.chat_get(chat.0).and_then(|c| c.sleep.as_deref()))
    };
    if sleeping {
        log::info!("[send] chat={} skipping: sleep window", chat.0);
        return;
    }

    let query = match DaftQuery::parse_url(url) {
        Ok(q) => q,
        Err(e) => {
            log::warn!("[send] chat={} bad stored URL {url:?}: {e}", chat.0);
            return;
        }
    };

    let listings = match daft::fetch_all(http, &query, 10).await {
        Ok(v) => v,
        Err(e) => {
            log::error!("[send] chat={} fetch failed for {url}: {e}", chat.0);
            return;
        }
    };

    let (filter_phone, work_point) = {
        let s = state.lock().await;
        let c = s.chat_get(chat.0);
        (
            c.map(|c| c.filter_phone).unwrap_or(false),
            c.and_then(|c| c.work_point),
        )
    };

    let fetched = listings.len();
    let mut sent: u32 = 0;
    let mut skipped: u32 = 0;
    let mut filtered_no_phone: u32 = 0;
    let mut failed: u32 = 0;

    for listing in listings {
        let already = state.lock().await.is_seen(listing.id);
        if already {
            skipped += 1;
            continue;
        }

        // Phone-filter: skip ads without a phone number. Don't mark as seen so
        // toggling the filter off later still surfaces them.
        if filter_phone && !has_phone(&listing) {
            filtered_no_phone += 1;
            log::debug!(
                "[send] chat={} id={} skipped: no phone (filter on)",
                chat.0,
                listing.id
            );
            continue;
        }

        // Detail page (description fields, views, date).
        let extras = match daft::fetch_detail(http, &listing.url).await {
            Ok(d) => {
                log::debug!(
                    "[detail] id={} overview_items={} views={:?} date_ms={:?}",
                    listing.id,
                    d.property_overview.len(),
                    d.views,
                    d.date_listed_ms
                );
                Some(d)
            }
            Err(e) => {
                log::warn!("[detail] id={} fetch failed: {e}", listing.id);
                None
            }
        };

        // Commute times via Google Distance Matrix (best-effort).
        let commute = match (work_point, google_key.as_ref(), listing.lat, listing.lng) {
            (Some([wlat, wlng]), Some(key), Some(alat), Some(alng)) => {
                let times = routing::fetch_all(http, key, (alat, alng), (wlat, wlng)).await;
                if times.any() {
                    log::debug!(
                        "[routing] id={} drive={:?} walk={:?} bike={:?} transit={:?}",
                        listing.id,
                        times.driving,
                        times.walking,
                        times.bicycling,
                        times.transit
                    );
                    Some(times)
                } else {
                    None
                }
            }
            _ => None,
        };

        log::info!(
            "[send] chat={} id={} '{}' photos={}",
            chat.0,
            listing.id,
            listing.title,
            listing.images.len().min(3)
        );
        match send_listing(bot, chat, &listing, extras.as_ref(), commute.as_ref()).await {
            Ok(()) => {
                sent += 1;
                let mut s = state.lock().await;
                s.mark_seen(listing.id);
                if let Err(e) = s.save().await {
                    log::warn!("[send] save state failed: {e}");
                }
            }
            Err(e) => {
                failed += 1;
                log::error!(
                    "[send] chat={} id={} failed: {e}",
                    chat.0,
                    listing.id
                );
            }
        }
        tokio::time::sleep(SEND_GAP).await;
    }

    // Update last_check on the corresponding watch.
    {
        let mut s = state.lock().await;
        if let Some(w) = s
            .chat_mut(chat.0)
            .watches
            .iter_mut()
            .find(|w| w.url == url)
        {
            w.last_check = now();
        }
        if let Err(e) = s.save().await {
            log::warn!("[send] save state failed: {e}");
        }
    }

    log::info!(
        "[send] chat={} url={url} fetched={fetched} sent={sent} skipped={skipped} filtered_no_phone={filtered_no_phone} failed={failed}",
        chat.0
    );
}

async fn send_listing(
    bot: &Bot,
    chat: ChatId,
    l: &Listing,
    extras: Option<&DetailExtras>,
    commute: Option<&CommuteTimes>,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let caption = format_caption(l, extras, commute);

    let mut photos: Vec<InputMedia> = Vec::new();
    for (i, img_url) in l.images.iter().take(3).enumerate() {
        let Ok(parsed) = img_url.parse() else { continue };
        let mut photo = InputMediaPhoto::new(InputFile::url(parsed));
        if i == 0 {
            photo = photo.caption(caption.clone()).parse_mode(ParseMode::Html);
        }
        photos.push(InputMedia::Photo(photo));
    }

    if photos.is_empty() {
        bot.send_message(chat, caption)
            .parse_mode(ParseMode::Html)
            .await?;
    } else if photos.len() == 1 {
        let Ok(parsed) = l.images[0].parse() else {
            bot.send_message(chat, caption)
                .parse_mode(ParseMode::Html)
                .await?;
            return Ok(());
        };
        bot.send_photo(chat, InputFile::url(parsed))
            .caption(caption)
            .parse_mode(ParseMode::Html)
            .await?;
    } else {
        bot.send_media_group(chat, photos).await?;
    }

    Ok(())
}

/// Telegram allows up to 1024 chars in a media caption. Leave a small margin.
const CAPTION_LIMIT: usize = 1000;

fn format_caption(
    l: &Listing,
    extras: Option<&DetailExtras>,
    commute: Option<&CommuteTimes>,
) -> String {
    let mut head = String::new();
    let price = l.price.as_deref().unwrap_or("price unknown");
    head.push_str(&format!("<b>{}</b>", html_escape(price)));
    if let Some(beds) = &l.bedrooms {
        head.push_str(&format!(" — {}", html_escape(beds)));
    }
    head.push('\n');

    if let Some(ptype) = &l.property_type {
        head.push_str(&format!("🏠 {}", html_escape(ptype)));
        if let Some(ber) = &l.ber_rating {
            head.push_str(&format!(" • BER {}", html_escape(ber)));
        }
        head.push('\n');
    }
    head.push_str(&format!("📍 {}\n", html_escape(&l.title)));

    if let Some(name) = &l.seller_name {
        head.push_str(&format!("👤 {}\n", html_escape(name)));
    }
    if let Some(phone) = l.seller_phone.as_deref().map(str::trim).filter(|s| !s.is_empty()) {
        head.push_str(&format!("📞 <code>{}</code>\n", html_escape(phone)));
    }

    if let Some(ex) = extras {
        for item in &ex.property_overview {
            if item.label == "Preferences" {
                if let Some(emojis) = preferences_emojis(&item.text) {
                    head.push_str(&format!("{emojis}\n"));
                    continue;
                }
            }
            head.push_str(&render_overview_line(&item.label, &item.text));
        }

        if let Some(ms) = ex.date_listed_ms {
            if let Some(d) = format_date_ms(ms) {
                head.push_str(&format!("🗓 <b>{}</b>\n", html_escape(&d)));
            }
        }
        if let Some(v) = ex.views {
            head.push_str(&format!("👁 <b>{}</b>\n", format_thousands(v)));
        }
    }

    let mut facilities_block = String::new();
    if !l.facilities.is_empty() {
        facilities_block.push_str(&format!(
            "✨ {}\n",
            html_escape(&l.facilities.join(", "))
        ));
    }

    let mut links = String::new();
    links.push_str(&format!(
        "<a href=\"{}\">View on daft.ie</a>",
        html_escape(&l.url)
    ));
    if let (Some(lat), Some(lng)) = (l.lat, l.lng) {
        links.push_str(&format!(
            " · <a href=\"https://www.google.com/maps?q={lat},{lng}&amp;z=13\">Google Maps</a>"
        ));
    }

    // Commute block (work-point distance via Google Distance Matrix).
    let mut commute_block = String::new();
    if let Some(c) = commute {
        let lines: &[(Mode, &Option<u32>)] = &[
            (Mode::Driving, &c.driving),
            (Mode::Walking, &c.walking),
            (Mode::Bicycling, &c.bicycling),
            (Mode::Transit, &c.transit),
        ];
        for (mode, secs) in lines {
            if let Some(s) = secs {
                commute_block.push_str(&format!(
                    "{} {}\n",
                    mode.emoji(),
                    html_escape(&routing::format_duration(*s))
                ));
            }
        }
    }

    let mut out = String::with_capacity(
        head.len() + facilities_block.len() + commute_block.len() + links.len(),
    );
    out.push_str(&head);
    out.push_str(&facilities_block);
    out.push_str(&commute_block);
    out.push_str(&links);

    if out.chars().count() > CAPTION_LIMIT {
        out = out.chars().take(CAPTION_LIMIT - 1).collect();
        out.push('…');
    }
    out
}

/// Render one "Property Overview" line. Style depends on the original label.
fn render_overview_line(orig_label: &str, value: &str) -> String {
    let v = html_escape(value);
    match orig_label {
        "Bedrooms Available" => format!("🛏 Beds: <b>{v}</b>\n"),
        "Available From" => format!("📅 <b>{v}</b>\n"),
        "Available For" => format!("📆 <b>{v}</b>\n"),
        "Sharing with" => format!("Sharing: <b>{v}</b>\n"),
        "Owner Occupied" => format!("Owner: <b>{v}</b>\n"),
        other => format!("{}: <b>{v}</b>\n", html_escape(other)),
    }
}

/// Render the Preferences line. Male/Female become emojis; anything else
/// (e.g. "+1 Person", "Couples") is appended verbatim. Returns None when no
/// gender token was found, so the caller can fall back to the default
/// `Preferences: <value>` rendering.
fn preferences_emojis(text: &str) -> Option<String> {
    let mut parts: Vec<String> = Vec::new();
    let mut has_gender = false;
    for p in text.split('/') {
        let raw = p.trim();
        if raw.is_empty() {
            continue;
        }
        match raw.to_lowercase().as_str() {
            "female" => {
                parts.push("👱🏻\u{200d}♀\u{fe0f}".to_string());
                has_gender = true;
            }
            "male" => {
                parts.push("👱🏻\u{200d}♂\u{fe0f}".to_string());
                has_gender = true;
            }
            _ => parts.push(html_escape(raw)),
        }
    }
    if has_gender {
        Some(parts.join(" "))
    } else {
        None
    }
}

fn format_date_ms(ms: i64) -> Option<String> {
    chrono::DateTime::from_timestamp_millis(ms).map(|dt| dt.format("%d/%m/%Y").to_string())
}

fn format_thousands(n: u64) -> String {
    let s = n.to_string();
    let chars: Vec<char> = s.chars().collect();
    let total = chars.len();
    let mut out = String::with_capacity(total + total / 3);
    for (i, c) in chars.iter().enumerate() {
        let from_end = total - i;
        if i > 0 && from_end % 3 == 0 {
            out.push(',');
        }
        out.push(*c);
    }
    out
}

fn html_escape(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

pub async fn poll_loop(
    bot: Bot,
    state: SharedState,
    http: HttpClient,
    google_key: SharedKey,
    interval_secs: u64,
) {
    let mut ticker = tokio::time::interval(std::time::Duration::from_secs(interval_secs));
    ticker.tick().await; // burn the immediate-first tick
    log::info!("[poll] loop armed, interval={interval_secs}s");

    let mut tick_n: u64 = 0;
    loop {
        ticker.tick().await;
        tick_n += 1;

        let (snapshot, paused, sleeping) = {
            let s = state.lock().await;
            let mut paused = 0usize;
            let mut sleeping_n = 0usize;
            let snap: Vec<(i64, String)> = s
                .chats
                .iter()
                .filter(|(_, chat)| {
                    if !chat.active {
                        paused += 1;
                        return false;
                    }
                    if sleep::is_sleeping(chat.sleep.as_deref()) {
                        sleeping_n += 1;
                        return false;
                    }
                    true
                })
                .flat_map(|(cid_str, chat)| {
                    let cid: i64 = cid_str.parse().unwrap_or(0);
                    chat.watches.iter().map(move |w| (cid, w.url.clone()))
                })
                .collect();
            (snap, paused, sleeping_n)
        };

        if snapshot.is_empty() {
            log::debug!(
                "[poll] tick #{tick_n}: nothing to do (stopped={paused}, sleeping={sleeping})"
            );
            continue;
        }
        log::info!(
            "[poll] tick #{tick_n}: checking {} watch(es) (stopped={paused}, sleeping={sleeping})",
            snapshot.len()
        );

        for (chat_id, url) in snapshot {
            fetch_and_send_new(&bot, ChatId(chat_id), &url, &state, &http, &google_key).await;
        }
        log::info!("[poll] tick #{tick_n}: done");
    }
}

/// `/map` — for every URL the chat watches, fetch the current listings,
/// collect their coordinates, build one Google Static Maps URL and send it
/// as a photo.
async fn handle_map(
    bot: &Bot,
    chat: ChatId,
    state: &SharedState,
    http: &HttpClient,
    google_key: &SharedKey,
) -> ResponseResult<()> {
    let key = match google_key.as_ref() {
        Some(k) => k.clone(),
        None => {
            bot.send_message(
                chat,
                "/map needs GOOGLE_MAPS_API_KEY in .env (Maps Static API must be enabled on it).",
            )
            .await?;
            return Ok(());
        }
    };

    let (urls, work_point) = {
        let s = state.lock().await;
        let c = s.chat_get(chat.0);
        (
            c.map(|c| c.watches.iter().map(|w| w.url.clone()).collect::<Vec<_>>())
                .unwrap_or_default(),
            c.and_then(|c| c.work_point),
        )
    };
    if urls.is_empty() {
        bot.send_message(chat, "No watches yet. Send me a daft.ie URL first.")
            .await?;
        return Ok(());
    }

    bot.send_message(chat, format!("Fetching {} watch(es)…", urls.len()))
        .await?;

    let mut all_points: Vec<(f64, f64)> = Vec::new();
    let mut total_fetched: usize = 0;
    for url in &urls {
        let query = match DaftQuery::parse_url(url) {
            Ok(q) => q,
            Err(e) => {
                log::warn!("[map] bad stored URL {url:?}: {e}");
                continue;
            }
        };
        match daft::fetch_all(http, &query, 10).await {
            Ok(listings) => {
                total_fetched += listings.len();
                for l in listings {
                    if let (Some(lat), Some(lng)) = (l.lat, l.lng) {
                        all_points.push((lat, lng));
                    }
                }
            }
            Err(e) => log::warn!("[map] fetch failed for {url}: {e}"),
        }
    }

    if all_points.is_empty() {
        bot.send_message(chat, "No listings with coordinates were returned.")
            .await?;
        return Ok(());
    }

    let shown = all_points.len().min(50);
    let work_opt = work_point.map(|[a, b]| (a, b));
    let map_url = staticmap::build_url(&all_points, work_opt, 800, 600, &key);
    log::info!(
        "[map] chat={} listings_fetched={} points={} shown={} work_point={}",
        chat.0,
        total_fetched,
        all_points.len(),
        shown,
        work_opt.is_some()
    );

    let caption = format!(
        "{shown} of {} listing(s) on map{}",
        all_points.len(),
        if work_opt.is_some() {
            " · blue pin (W) = your work point"
        } else {
            ""
        }
    );

    match map_url.parse() {
        Ok(parsed) => {
            if let Err(e) = bot
                .send_photo(chat, InputFile::url(parsed))
                .caption(caption)
                .await
            {
                log::error!("[map] send_photo failed: {e}");
                bot.send_message(
                    chat,
                    format!("Map service failed: {e}\n(Is 'Maps Static API' enabled + allowed on your API key?)"),
                )
                .await?;
            }
        }
        Err(e) => {
            bot.send_message(chat, format!("Internal: bad map URL: {e}"))
                .await?;
        }
    }

    Ok(())
}
