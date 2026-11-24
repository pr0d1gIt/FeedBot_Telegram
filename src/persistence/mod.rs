//! Lightweight, file-backed persistence layer.

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use tokio::fs;
use tokio::sync::RwLock;
use tracing::{debug, info, warn};

const MAX_SENT_IDS: usize = 5000;

#[derive(Debug, Default, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct UserProfile {
    #[serde(default)]
    pub username: Option<String>,
    #[serde(default)]
    pub first_name: Option<String>,
}

impl UserProfile {
    pub fn display_label(&self, chat_id: i64) -> String {
        match (&self.username, &self.first_name) {
            (Some(u), Some(f)) => format!("{f} (@{u}) [{chat_id}]"),
            (Some(u), None) => format!("@{u} [{chat_id}]"),
            (None, Some(f)) => format!("{f} [{chat_id}]"),
            (None, None) => format!("ID {chat_id}"),
        }
    }
}

#[derive(Debug, Default, Clone, Serialize, Deserialize)]
struct StateFile {
    #[serde(default)]
    subscribers: Vec<i64>,
    #[serde(default)]
    banned_users: Vec<i64>,
    #[serde(default)]
    user_profiles: HashMap<i64, UserProfile>,
    #[serde(default)]
    sent_ids: Vec<String>,
}

#[derive(Debug)]
struct StateInner {
    subscribers: HashSet<i64>,
    banned_users: HashSet<i64>,
    user_profiles: HashMap<i64, UserProfile>,
    sent_ids: Vec<String>,
    sent_set: HashSet<String>,
}

impl StateInner {
    fn from_file(file: StateFile) -> Self {
        let sent_set: HashSet<String> = file.sent_ids.iter().cloned().collect();
        Self {
            subscribers: file.subscribers.into_iter().collect(),
            banned_users: file.banned_users.into_iter().collect(),
            user_profiles: file.user_profiles,
            sent_ids: file.sent_ids,
            sent_set,
        }
    }

    fn to_file(&self) -> StateFile {
        let mut subscribers: Vec<i64> = self.subscribers.iter().copied().collect();
        subscribers.sort_unstable();

        let mut banned_users: Vec<i64> = self.banned_users.iter().copied().collect();
        banned_users.sort_unstable();

        StateFile {
            subscribers,
            banned_users,
            user_profiles: self.user_profiles.clone(),
            sent_ids: self.sent_ids.clone(),
        }
    }
}

/// Cheap-to-clone, thread-safe handle to the bot's persisted state.
#[derive(Debug, Clone)]
pub struct State {
    inner: Arc<RwLock<StateInner>>,
    path: PathBuf,
}

impl State {
    /// Load state from `path`, creating an empty file if it does not exist.
    pub async fn load(path: impl Into<PathBuf>) -> Result<Self> {
        let path = path.into();
        let file = match fs::read(&path).await {
            Ok(bytes) if bytes.is_empty() => StateFile::default(),
            Ok(bytes) => match serde_json::from_slice::<StateFile>(&bytes) {
                Ok(f) => f,
                Err(e) => {
                    warn!(
                        path = %path.display(),
                        error = %e,
                        "State file is corrupt; starting from empty state and backing up."
                    );
                    let backup = path.with_extension("json.bak");
                    let _ = fs::rename(&path, &backup).await;
                    StateFile::default()
                }
            },
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                info!(path = %path.display(), "State file not found, creating new one.");
                StateFile::default()
            }
            Err(e) => {
                return Err(e)
                    .with_context(|| format!("Failed to read state file {}", path.display()))
            }
        };

        let state = Self {
            inner: Arc::new(RwLock::new(StateInner::from_file(file))),
            path,
        };
        state.flush().await?;
        Ok(state)
    }

    /// Record or update basic user metadata when they interact with the bot.
    pub async fn upsert_profile(
        &self,
        chat_id: i64,
        username: Option<String>,
        first_name: Option<String>,
    ) -> Result<()> {
        let new_profile = UserProfile {
            username,
            first_name,
        };
        let changed = {
            let mut inner = self.inner.write().await;
            match inner.user_profiles.get(&chat_id) {
                Some(existing) if existing == &new_profile => false,
                _ => {
                    inner.user_profiles.insert(chat_id, new_profile);
                    true
                }
            }
        };
        if changed {
            self.flush().await?;
        }
        Ok(())
    }

    /// Get a formatted label for a user ID using stored profile metadata if available.
    pub async fn user_label(&self, chat_id: i64) -> String {
        let inner = self.inner.read().await;
        inner
            .user_profiles
            .get(&chat_id)
            .map(|p| p.display_label(chat_id))
            .unwrap_or_else(|| format!("ID {chat_id}"))
    }

    /// Return the current list of subscriber chat ids.
    pub async fn subscribers(&self) -> Vec<i64> {
        let inner = self.inner.read().await;
        let mut list: Vec<i64> = inner.subscribers.iter().copied().collect();
        list.sort_unstable();
        list
    }

    /// Add a subscriber. Returns `true` if the subscriber was newly added.
    pub async fn add_subscriber(&self, chat_id: i64) -> Result<bool> {
        let added = {
            let mut inner = self.inner.write().await;
            inner.subscribers.insert(chat_id)
        };
        if added {
            self.flush().await?;
        }
        Ok(added)
    }

    /// Remove a subscriber. Returns `true` if a subscriber was actually removed.
    pub async fn remove_subscriber(&self, chat_id: i64) -> Result<bool> {
        let removed = {
            let mut inner = self.inner.write().await;
            inner.subscribers.remove(&chat_id)
        };
        if removed {
            self.flush().await?;
        }
        Ok(removed)
    }

    /// Check whether a user is banned.
    pub async fn is_banned(&self, chat_id: i64) -> bool {
        let inner = self.inner.read().await;
        inner.banned_users.contains(&chat_id)
    }

    /// Return all banned user IDs sorted.
    pub async fn banned_users(&self) -> Vec<i64> {
        let inner = self.inner.read().await;
        let mut list: Vec<i64> = inner.banned_users.iter().copied().collect();
        list.sort_unstable();
        list
    }

    /// Ban a user and remove them from active subscribers. Returns `true` if newly banned.
    pub async fn ban_user(&self, chat_id: i64) -> Result<bool> {
        let changed = {
            let mut inner = self.inner.write().await;
            let unsub = inner.subscribers.remove(&chat_id);
            let banned = inner.banned_users.insert(chat_id);
            unsub || banned
        };
        if changed {
            self.flush().await?;
        }
        Ok(changed)
    }

    /// Remove a user from the banned list. Returns `true` if they were banned.
    pub async fn unban_user(&self, chat_id: i64) -> Result<bool> {
        let removed = {
            let mut inner = self.inner.write().await;
            inner.banned_users.remove(&chat_id)
        };
        if removed {
            self.flush().await?;
        }
        Ok(removed)
    }

    /// Return true if `id` has already been delivered.
    pub async fn was_sent(&self, id: &str) -> bool {
        let inner = self.inner.read().await;
        inner.sent_set.contains(id)
    }

    /// Mark the supplied IDs as delivered.
    pub async fn mark_sent_batch<I>(&self, ids: I) -> Result<()>
    where
        I: IntoIterator<Item = String>,
    {
        let mut any_added = false;
        {
            let mut inner = self.inner.write().await;
            for id in ids {
                if inner.sent_set.insert(id.clone()) {
                    inner.sent_ids.push(id);
                    any_added = true;
                }
            }
            while inner.sent_ids.len() > MAX_SENT_IDS {
                let removed = inner.sent_ids.remove(0);
                inner.sent_set.remove(&removed);
            }
        }
        if any_added {
            self.flush().await?;
        }
        Ok(())
    }

    async fn flush(&self) -> Result<()> {
        let snapshot = {
            let inner = self.inner.read().await;
            inner.to_file()
        };
        let bytes = serde_json::to_vec_pretty(&snapshot).context("Failed to serialise state")?;
        write_atomic(&self.path, &bytes)
            .await
            .with_context(|| format!("Failed to write state file {}", self.path.display()))?;
        debug!(path = %self.path.display(), "State flushed");
        Ok(())
    }
}

async fn write_atomic(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    if let Some(parent) = path.parent() {
        if !parent.as_os_str().is_empty() {
            fs::create_dir_all(parent).await?;
        }
    }
    let tmp = path.with_extension("json.tmp");
    fs::write(&tmp, bytes).await?;
    if let Err(e) = fs::rename(&tmp, path).await {
        warn!(error = %e, "Atomic rename failed, falling back to direct write.");
        fs::write(path, bytes).await?;
        let _ = fs::remove_file(&tmp).await;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn add_and_remove_subscriber_roundtrips_through_disk() {
        let dir = std::env::temp_dir().join(format!("tg-bot-test-{}", std::process::id()));
        let path = dir.join("state.json");
        let _ = fs::remove_file(&path).await;

        let state = State::load(&path).await.unwrap();
        assert!(state.add_subscriber(42).await.unwrap());
        assert!(!state.add_subscriber(42).await.unwrap());
        assert_eq!(state.subscribers().await, vec![42]);

        let reloaded = State::load(&path).await.unwrap();
        assert_eq!(reloaded.subscribers().await, vec![42]);

        assert!(reloaded.ban_user(42).await.unwrap());
        assert!(reloaded.is_banned(42).await);
        assert!(reloaded.subscribers().await.is_empty());

        assert!(reloaded.unban_user(42).await.unwrap());
        assert!(!reloaded.is_banned(42).await);

        let _ = fs::remove_file(&path).await;
        let _ = fs::remove_dir(&dir).await;
    }

    #[tokio::test]
    async fn dedup_marks_and_recalls_ids() {
        let dir = std::env::temp_dir().join(format!("tg-bot-test-dedup-{}", std::process::id()));
        let path = dir.join("state.json");
        let _ = fs::remove_file(&path).await;

        let state = State::load(&path).await.unwrap();
        assert!(!state.was_sent("a").await);
        state
            .mark_sent_batch(["a".to_string(), "b".to_string()])
            .await
            .unwrap();
        assert!(state.was_sent("a").await);
        assert!(state.was_sent("b").await);
        assert!(!state.was_sent("c").await);

        let _ = fs::remove_file(&path).await;
        let _ = fs::remove_dir(&dir).await;
    }
}