mod bot;
mod daft;
mod routing;
mod sleep;
mod staticmap;
mod state;

use anyhow::{Context, Result};
use clap::{Parser, Subcommand};
use reqwest::Client;
use std::path::PathBuf;
use std::sync::Arc;
use teloxide::prelude::*;
use teloxide::utils::command::BotCommands;
use tokio::sync::Mutex;

use crate::state::State;

#[derive(Parser)]
#[command(about = "daft.ie sharing scraper + Telegram bot")]
struct Args {
    #[command(subcommand)]
    cmd: Option<Cmd>,
}

#[derive(Subcommand)]
enum Cmd {
    /// Run the Telegram bot (default)
    Bot,
    /// One-off scrape, write to JSON
    Scrape {
        #[arg(default_value = "https://www.daft.ie/sharing/midleton-cork?radius=3000")]
        url: String,
        #[arg(short, long, default_value = "listings.json")]
        out: PathBuf,
        #[arg(long, default_value_t = 0)]
        max_pages: u32,
    },
}

#[tokio::main]
async fn main() -> Result<()> {
    let _ = dotenvy::dotenv();

    env_logger::Builder::from_env(
        env_logger::Env::default()
            .default_filter_or("info,daftie=debug,reqwest=warn,hyper=warn,teloxide=info"),
    )
    .format_timestamp_secs()
    .init();

    let args = Args::parse();

    match args.cmd.unwrap_or(Cmd::Bot) {
        Cmd::Bot => run_bot().await,
        Cmd::Scrape {
            url,
            out,
            max_pages,
        } => run_scrape(url, out, max_pages).await,
    }
}

async fn run_scrape(url: String, out: PathBuf, max_pages: u32) -> Result<()> {
    let query = daft::DaftQuery::parse_url(&url)?;
    println!(
        "Searching daft.ie section={} area={} type={:?} price={:?}–{:?}",
        query.section,
        query.area_slug,
        query.property_type,
        query.rental_price_from,
        query.rental_price_to
    );

    let client = Client::builder().user_agent("daftie-rs/0.1").build()?;
    let listings = daft::fetch_all(&client, &query, max_pages).await?;

    let json = serde_json::to_string_pretty(&listings)?;
    tokio::fs::write(&out, &json)
        .await
        .with_context(|| format!("writing {}", out.display()))?;
    println!("Wrote {} listings to {}", listings.len(), out.display());

    for l in &listings {
        let price = l.price.as_deref().unwrap_or("?");
        let beds = l.bedrooms.as_deref().unwrap_or("?");
        println!("  {:<20}  {:<16}  {}", price, beds, l.title);
        println!("    {}", l.url);
    }
    Ok(())
}

async fn run_bot() -> Result<()> {
    let token = std::env::var("TELEGRAM_BOT_TOKEN")
        .map_err(|_| anyhow::anyhow!("TELEGRAM_BOT_TOKEN not set — fill it in .env"))?;
    if token.trim().is_empty() {
        anyhow::bail!("TELEGRAM_BOT_TOKEN is empty — put your bot token in .env");
    }

    log::info!(
        "token loaded (id={}, len={})",
        token.split(':').next().unwrap_or("?"),
        token.len()
    );

    let bot = Bot::new(token);
    let http = Client::builder().user_agent("daftie-rs/0.1").build()?;

    let google_key = std::env::var("GOOGLE_MAPS_API_KEY")
        .ok()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty());
    match &google_key {
        Some(k) => log::info!(
            "Google Maps key loaded (len={}, ends ...{})",
            k.len(),
            &k[k.len().saturating_sub(4)..]
        ),
        None => log::info!("Google Maps key NOT set — /work_point will save coords but no commute is calculated"),
    }
    let google_key: Arc<Option<String>> = Arc::new(google_key);

    let state_path = State::default_path();
    log::info!("state file: {}", state_path.display());
    let state = Arc::new(Mutex::new(State::load(state_path).await?));

    {
        let s = state.lock().await;
        let chats = s.chats.len();
        let watches: usize = s.chats.values().map(|c| c.watches.len()).sum();
        let paused: usize = s.chats.values().filter(|c| !c.active).count();
        let sleeping: usize = s
            .chats
            .values()
            .filter(|c| sleep::is_sleeping(c.sleep.as_deref()))
            .count();
        log::info!(
            "state loaded: {chats} chat(s), {watches} watch(es), {} seen ad(s), {paused} stopped, {sleeping} sleeping",
            s.seen_count()
        );
    }

    match bot.get_me().await {
        Ok(me) => log::info!(
            "connected to telegram as @{} (id={})",
            me.username.as_deref().unwrap_or("?"),
            me.id
        ),
        Err(e) => log::warn!("get_me failed: {e}"),
    }

    // Wipe the previously-registered command list first so stale entries
    // (e.g. /list after we renamed it to /status) don't linger in the picker.
    if let Err(e) = bot.delete_my_commands().await {
        log::warn!("delete_my_commands failed: {e}");
    }
    if let Err(e) = bot.set_my_commands(bot::Cmd::bot_commands()).await {
        log::warn!("set_my_commands failed: {e}");
    } else {
        log::info!("registered {} bot commands", bot::Cmd::bot_commands().len());
    }

    let poll_interval = 600u64;
    log::info!("starting poller (every {poll_interval}s)");

    {
        let bot_c = bot.clone();
        let state_c = state.clone();
        let http_c = http.clone();
        let key_c = google_key.clone();
        tokio::spawn(async move {
            bot::poll_loop(bot_c, state_c, http_c, key_c, poll_interval).await;
        });
    }

    log::info!("dispatcher ready; awaiting messages…");

    let handler = Update::filter_message()
        .branch(
            dptree::entry()
                .filter_command::<bot::Cmd>()
                .endpoint(bot::handle_cmd),
        )
        .branch(dptree::endpoint(bot::handle_text));

    Dispatcher::builder(bot, handler)
        .dependencies(dptree::deps![state, http, google_key])
        .enable_ctrlc_handler()
        .build()
        .dispatch()
        .await;

    Ok(())
}
