use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use tokio::fs;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct State {
    #[serde(default)]
    pub chats: HashMap<String, ChatState>,

    /// Global set of ad IDs already sent to any chat. Replaces the old
    /// `old_ads_ids.txt` file.
    #[serde(default)]
    pub seen_ids: HashSet<u64>,

    #[serde(skip)]
    file_path: PathBuf,
}

impl Default for State {
    fn default() -> Self {
        Self {
            chats: HashMap::new(),
            seen_ids: HashSet::new(),
            file_path: PathBuf::new(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChatState {
    #[serde(default)]
    pub watches: Vec<Watch>,

    /// Whether periodic polling delivers new listings for this chat.
    /// Toggled by /start_watching and /stop_watching.
    #[serde(default = "default_true")]
    pub active: bool,

    /// Optional sleep window in canonical `HH:MM-HH:MM` form.
    /// During this window the bot does not fetch or send for this chat.
    #[serde(default)]
    pub sleep: Option<String>,

    /// When true, skip listings whose seller has no phone number.
    #[serde(default)]
    pub filter_phone: bool,

    /// Optional "home base" the user commutes to (e.g. office).
    /// Stored as `[lat, lng]`. Used by Google Distance Matrix to attach
    /// drive/walk/bike/transit times to each listing.
    #[serde(default)]
    pub work_point: Option<[f64; 2]>,
}

fn default_true() -> bool {
    true
}

impl Default for ChatState {
    fn default() -> Self {
        Self {
            watches: Vec::new(),
            active: true,
            sleep: None,
            filter_phone: false,
            work_point: None,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Watch {
    pub url: String,
    #[serde(default)]
    pub last_check: i64,
}

impl State {
    /// `state.json` next to the daftie executable.
    pub fn default_path() -> PathBuf {
        if let Ok(exe) = std::env::current_exe() {
            if let Some(parent) = exe.parent() {
                return parent.join("state.json");
            }
        }
        PathBuf::from("state.json")
    }

    pub async fn load(path: PathBuf) -> Result<Self> {
        // One-time migration: if the canonical path is empty but a state.json
        // lives in cwd (from older versions), move it over.
        let cwd_state = PathBuf::from("state.json");
        if !path.exists() && cwd_state.exists() {
            log::info!(
                "migrating state.json from cwd to {}",
                path.display()
            );
            if let Err(e) = fs::rename(&cwd_state, &path).await {
                log::warn!("rename failed ({e}); copying instead");
                if let Ok(content) = fs::read_to_string(&cwd_state).await {
                    let _ = fs::write(&path, content).await;
                    let bak = cwd_state.with_extension("json.migrated");
                    let _ = fs::rename(&cwd_state, &bak).await;
                }
            }
        }

        let mut state: State = if path.exists() {
            let content = fs::read_to_string(&path)
                .await
                .context("reading state file")?;
            if content.trim().is_empty() {
                Self::default()
            } else {
                serde_json::from_str(&content).context("parsing state file")?
            }
        } else {
            Self::default()
        };
        state.file_path = path;

        // Migrate any IDs that used to live in old_ads_ids.txt.
        state.migrate_old_txt().await;

        Ok(state)
    }

    async fn migrate_old_txt(&mut self) {
        let exe_dir = std::env::current_exe()
            .ok()
            .and_then(|p| p.parent().map(|d| d.to_path_buf()))
            .unwrap_or_else(|| PathBuf::from("."));
        let old_path = exe_dir.join("old_ads_ids.txt");
        if !old_path.exists() {
            return;
        }
        match fs::read_to_string(&old_path).await {
            Ok(content) => {
                let mut migrated = 0u32;
                for line in content.lines() {
                    let trimmed = line.trim();
                    if trimmed.is_empty() || trimmed.starts_with('#') {
                        continue;
                    }
                    let first = trimmed.split_whitespace().next().unwrap_or("");
                    if let Ok(id) = first.parse::<u64>() {
                        if self.seen_ids.insert(id) {
                            migrated += 1;
                        }
                    }
                }
                log::info!(
                    "migrated {} ad ID(s) from {}",
                    migrated,
                    old_path.display()
                );
                if migrated > 0 {
                    if let Err(e) = self.save().await {
                        log::warn!("could not save migrated state: {e}");
                    }
                }
                let bak = old_path.with_file_name("old_ads_ids.txt.migrated");
                if let Err(e) = fs::rename(&old_path, &bak).await {
                    log::warn!(
                        "could not rename {} to .migrated: {e}",
                        old_path.display()
                    );
                }
            }
            Err(e) => log::warn!("could not read old_ads_ids.txt for migration: {e}"),
        }
    }

    /// Atomic write: tmp file then rename.
    pub async fn save(&self) -> Result<()> {
        let content = serde_json::to_string_pretty(self)?;
        let tmp = self.file_path.with_extension("json.tmp");
        fs::write(&tmp, content)
            .await
            .context("writing tmp state file")?;
        fs::rename(&tmp, &self.file_path)
            .await
            .context("renaming tmp into place")?;
        Ok(())
    }

    pub fn chat_mut(&mut self, chat_id: i64) -> &mut ChatState {
        self.chats.entry(chat_id.to_string()).or_default()
    }

    pub fn chat_get(&self, chat_id: i64) -> Option<&ChatState> {
        self.chats.get(&chat_id.to_string())
    }

    /// Returns true if newly added, false if already present.
    pub fn add_watch(&mut self, chat_id: i64, url: String) -> bool {
        let chat = self.chat_mut(chat_id);
        if chat.watches.iter().any(|w| w.url == url) {
            return false;
        }
        chat.watches.push(Watch {
            url,
            last_check: 0,
        });
        true
    }

    /// Remove by URL or by 1-based index. Returns the removed URL.
    pub fn remove_watch(&mut self, chat_id: i64, key: &str) -> Option<String> {
        let chat = self.chat_mut(chat_id);
        if let Ok(idx) = key.parse::<usize>() {
            if idx >= 1 && idx <= chat.watches.len() {
                return Some(chat.watches.remove(idx - 1).url);
            }
        }
        if let Some(pos) = chat.watches.iter().position(|w| w.url == key) {
            return Some(chat.watches.remove(pos).url);
        }
        None
    }

    pub fn is_seen(&self, id: u64) -> bool {
        self.seen_ids.contains(&id)
    }

    /// Returns true if the id was newly inserted.
    pub fn mark_seen(&mut self, id: u64) -> bool {
        self.seen_ids.insert(id)
    }

    pub fn clear_seen(&mut self) -> usize {
        let n = self.seen_ids.len();
        self.seen_ids.clear();
        n
    }

    pub fn seen_count(&self) -> usize {
        self.seen_ids.len()
    }
}
