<div align="center">

# 🏠 daftie

**A [daft.ie](https://www.daft.ie) rental watcher that DMs you new listings on Telegram — the moment they appear.**

Paste a daft.ie search URL, get a rich card for every new ad: photos, price, phone, BER, and even the commute time to your office.

[![Rust](https://img.shields.io/badge/Rust-2024-000000?logo=rust&logoColor=white)](https://www.rust-lang.org/)
[![Telegram](https://img.shields.io/badge/Telegram-Bot-26A5E4?logo=telegram&logoColor=white)](https://core.telegram.org/bots)
[![License: MIT](https://img.shields.io/badge/License-MIT-yellow.svg)](LICENSE)
[![Platform](https://img.shields.io/badge/platform-Windows%20%7C%20macOS%20%7C%20Linux-lightgrey)](#-quick-start)

</div>

---

## ✨ Features

- 🔔 **Instant alerts** — polls your searches every 10 minutes and DMs only listings you haven't seen.
- 🗺️ **Any daft.ie search URL** — location slugs *and* custom map-area searches (draw-your-own polygon on the map).
- 🖼️ **Rich listing cards** — up to 3 photos plus price, bedrooms, property type, BER, seller, phone, preferences, facilities, date listed, and view count.
- 🚗 **Commute times** — set a work point and each ad shows 🚗 driving · 🚶 walking · 🚴 cycling · 🚌 transit time (via Google Distance Matrix).
- 📍 **Map overview** — `/map` renders every current listing as pins on a single static map.
- 😴 **Quiet hours** — a sleep window pauses notifications overnight.
- 📞 **Phone filter** — optionally skip ads with no phone number.
- 💾 **Persistent state** — watches, settings, and seen-ad history survive restarts.
- 🧰 **CLI scraper** — dump any search to JSON without Telegram.

---

## 🚀 Quick start

### Prerequisites

- [Rust](https://rustup.rs/) (stable, 2024 edition — `1.85+`)
- A **Telegram bot token** from [@BotFather](https://t.me/BotFather)
- *(optional)* a **Google Maps API key** with the *Distance Matrix* and *Maps Static* APIs enabled — for commute times and `/map`

### 1. Clone & configure

```bash
git clone https://github.com/gravitymir/daftie.git
cd daftie
```

Create a `.env` file in the project root:

```env
# Required — from @BotFather
TELEGRAM_BOT_TOKEN=123456:ABC-your-token-here

# Optional — commute times + /map. Leave empty to disable.
GOOGLE_MAPS_API_KEY=

# Optional — logging verbosity
RUST_LOG=info,daftie=debug
```

> 🔒 `.env` and `state.json` are git-ignored — your token and chat data stay local.

### 2. Build & run

```bash
cargo run --release
```

Then open Telegram, message your bot, and send it a daft.ie search URL 👇

```
https://www.daft.ie/property-for-rent/cork-city?rentalPrice_to=2000
```

That's it — you'll get new listings as they're posted.

---

## 💬 Bot commands

| Command | What it does |
| --- | --- |
| `/watch <url>` | Start watching a daft.ie search URL |
| `/unwatch <url \| index>` | Stop watching a URL (by link or its number in `/status`) |
| `/status` | Show current settings and all watched URLs |
| `/check` | Force an immediate check now |
| `/map` | Send a map image of all current listing locations |
| `/work_point <lat>,<lng>` | Set your office for commute times (or `off`) |
| `/sleep_time <HH:MM-HH:MM>` | Set quiet hours (or `off`) |
| `/filter_phone <true\|false>` | Only send ads that include a phone number |
| `/start_watching` · `/stop_watching` | Resume / pause periodic checks |
| `/clear_history` | Forget all sent ad IDs (next check re-sends everything) |
| `/help` | Show the command list |

> 💡 You can also just **paste a daft.ie URL** to start watching it, or **share a Location** to set your work point.

---

## 🖥️ CLI usage

`daftie` runs the Telegram bot by default, but also has a one-off scraper:

```bash
# Run the bot (default)
daftie bot

# Scrape a search to JSON, no Telegram required
daftie scrape "https://www.daft.ie/sharing/midleton-cork?radius=3000" \
  --out listings.json \
  --max-pages 5          # 0 = all pages
```

---

## ⚙️ Configuration

All configuration lives in `.env`:

| Variable | Required | Purpose |
| --- | :---: | --- |
| `TELEGRAM_BOT_TOKEN` | ✅ | Your bot token from @BotFather |
| `GOOGLE_MAPS_API_KEY` | — | Enables commute times (`/work_point`) and `/map`. Distance Matrix + Maps Static APIs must be enabled |
| `RUST_LOG` | — | Log filter, e.g. `info,daftie=debug` |

Runtime state is written to `state.json` (chat IDs, watched URLs, settings, and seen-ad history).

---

## 🧭 Supported URLs

Grab any search URL straight from daft.ie's address bar. Both styles work:

- **Location searches** — `/property-for-rent/cork-city`, `/sharing/dublin`, `/property-for-sale/galway/houses`, …
- **Custom map areas** — draw a shape on daft.ie's map and copy the `…/mapArea?…&polygon={…}` URL. daftie reads the polygon and searches exactly within it.

Price filters (`rentalPrice_from` / `rentalPrice_to`) in the URL are respected automatically.

---

## 🗂️ Project structure

```
src/
├── main.rs        # CLI entry point — bot / scrape subcommands
├── bot.rs         # Telegram commands, polling loop, message formatting
├── daft.rs        # daft.ie gateway client + URL parsing
├── routing.rs     # Google Distance Matrix commute times
├── staticmap.rs   # Google Static Maps image builder
├── state.rs       # Persistent per-chat state (state.json)
└── sleep.rs       # Quiet-hours window parsing
```

---

## 📜 License

Released under the [MIT License](LICENSE).

<div align="center">
<sub>Not affiliated with daft.ie. For personal use — please be considerate with request volume.</sub>
</div>
