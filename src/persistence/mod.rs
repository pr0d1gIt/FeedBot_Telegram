//! Lightweight, file-backed persistence layer.
//!
//! The bot intentionally avoids a heavyweight database for a local
//! deployment. State is kept in memory behind a [`tokio::sync::RwLock`] and
//! flushed atomically to disk after every mutation.
//!
//! The JSON schema is intentionally simple and forward-compatible:
//!
//! ```json
//! {
//!     "subscribers": [123456789, 987654321],
//!     "sent_ids": ["https://...", "https://..."]
//! }
//! ```

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use tokio::fs;
use tokio::sync::RwLock;
use tracing::{debug, info, warn};

/// Upper bound on how many news IDs we remember to deduplicate. Old IDs are
/// evicted FIFO when this limit is exceeded.
const MAX_SENT_IDS: usize = 5000;

#[derive(Debug, Default, Clone, Serialize, Deserialize)]
struct StateFile {
    #[serde(default)]
    subscribers: Vec<i64>,
    #[serde(default)]
    sent_ids: Vec<String>,
}

#[derive(Debug)]
struct StateInner {
    subscribers: HashSet<i64>,
    /// Order-preserving FIFO of sent IDs (oldest first).
    sent_ids: Vec<String>,
    /// Membership index over `sent_ids` for O(1) `contains` checks.
    sent_set: HashSet<String>,
}

impl StateInner {
    fn from_file(file: StateFile) -> Self {
        let sent_set: HashSet<String> = file.sent_ids.iter().cloned().collect();
        Self {
            subscribers: file.subscribers.into_iter().collect(),
            sent_ids: file.sent_ids,
            sent_set,
        }
    }

    fn to_file(&self) -> StateFile {
        let mut subscribers: Vec<i64> = self.subscribers.iter().copied().collect();
        subscribers.sort_unstable();
        StateFile {
            subscribers,
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
        // Persist immediately so we know the path is writable.
        state.flush().await?;
        Ok(state)
    }

    /// Return the current list of subscriber chat ids.
    pub async fn subscribers(&self) -> Vec<i64> {
        let inner = self.inner.read().await;
        inner.subscribers.iter().copied().collect()
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

    /// Return true if `id` has already been delivered.
    pub async fn was_sent(&self, id: &str) -> bool {
        let inner = self.inner.read().await;
        inner.sent_set.contains(id)
    }

    /// Mark the supplied IDs as delivered. The on-disk state is flushed once
    /// after the batch is applied.
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
            // FIFO eviction.
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

    /// Atomically write the current state to disk.
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

/// Write `bytes` to `path` atomically by writing to a sibling temp file and
/// renaming. Falls back to a non-atomic write if rename fails (e.g. on
/// cross-device temp dirs).
async fn write_atomic(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    if let Some(parent) = path.parent() {
        if !parent.as_os_str().is_empty() {
            fs::create_dir_all(parent).await?;
        }
    }
    let tmp = path.with_extension("json.tmp");
    fs::write(&tmp, bytes).await?;
    if let Err(e) = fs::rename(&tmp, path).await {
        // Fallback: best-effort direct write.
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

        // Reload from disk.
        let reloaded = State::load(&path).await.unwrap();
        assert_eq!(reloaded.subscribers().await, vec![42]);

        assert!(reloaded.remove_subscriber(42).await.unwrap());
        assert!(reloaded.subscribers().await.is_empty());

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