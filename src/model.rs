use std::collections::HashMap;
use std::sync::mpsc::Sender;
use std::sync::{Arc, Mutex};

pub use crate::image_utils::Thumbnail;

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum ItemKind {
    PlainText,
    Image,
    UriList,
    RichText,
}

use crate::format_utils::format_bytes;

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct DiskStats {
    pub files: u64,
    pub dirs: u64,
    pub symlinks: u64,
    pub total_bytes: u64,
}

impl DiskStats {
    pub fn add(&mut self, other: DiskStats) {
        self.files += other.files;
        self.dirs += other.dirs;
        self.symlinks += other.symlinks;
        self.total_bytes += other.total_bytes;
    }

    pub fn format_report(&self) -> String {
        let mut parts = Vec::new();

        if self.files > 0 {
            if self.files == 1 {
                parts.push("1 file".to_string());
            } else {
                parts.push(format!("{} files", self.files));
            }
        }

        if self.dirs > 0 {
            if self.dirs == 1 {
                parts.push("1 dir".to_string());
            } else {
                parts.push(format!("{} dirs", self.dirs));
            }
        }

        if self.symlinks > 0 {
            if self.symlinks == 1 {
                parts.push("1 link".to_string());
            } else {
                parts.push(format!("{} links", self.symlinks));
            }
        }

        let size_str = format_bytes(self.total_bytes as usize);
        if parts.is_empty() {
            size_str
        } else {
            format!("{} • {}", parts.join(", "), size_str)
        }
    }
}

mod disk_stats_serde {
    use super::DiskStats;
    use serde::{Deserialize, Deserializer, Serialize, Serializer};
    use std::sync::{Arc, Mutex};

    pub fn serialize<S>(val: &Arc<Mutex<Option<DiskStats>>>, s: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        val.lock().unwrap().serialize(s)
    }

    pub fn deserialize<'de, D>(d: D) -> Result<Arc<Mutex<Option<DiskStats>>>, D::Error>
    where
        D: Deserializer<'de>,
    {
        let opt = Option::<DiskStats>::deserialize(d)?;
        Ok(Arc::new(Mutex::new(opt)))
    }
}

pub fn is_password_hint(name: &str) -> bool {
    let lower = name.to_ascii_lowercase();
    lower == "x-kde-passwordmanagerhint"
        || lower == "application/x-password-manager"
        || lower == "application/x-keepassxc-password"
        || lower == "application/x-bitwarden-secret"
        || lower == "clipboard_manager_nopickup"
        || lower == "x-kde-promptssecret"
        || lower == "x-kde-secret"
        || lower == "application/x-secret"
        || lower == "text/x-secret"
        || lower == "x-special/password"
        || lower.contains("passwordmanagerhint")
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct HistoryItem {
    pub id: u64,
    pub kind: ItemKind,
    pub byte_size: usize,
    pub mime_types: Vec<String>,
    pub thumbnail: Option<Thumbnail>,
    pub image_dimensions: Option<(u32, u32)>,
    pub plain_text: Option<String>,
    pub uri_paths: Option<Vec<String>>,
    pub is_rich_text: bool,
    #[serde(default)]
    pub is_password: bool,
    #[serde(default, skip)]
    pub targets: HashMap<String, Vec<u8>>,
    #[serde(default, skip)]
    pub raw_primary_payload: Vec<u8>,
    #[serde(with = "disk_stats_serde")]
    pub disk_stats: Arc<Mutex<Option<DiskStats>>>,
}

pub enum ClipboardCommand {
    RestoreNative(u64),
    RestorePlainText(u64),
    RestorePathText(u64),
    DeleteItem(u64),
}

pub struct SharedClipboardState {
    pub history: Vec<HistoryItem>,
    pub last_copied_id: Option<u64>,
    pub notify_tx: Vec<Sender<()>>,
}

impl SharedClipboardState {
    pub fn new() -> Self {
        Self {
            history: Vec::new(),
            last_copied_id: None,
            notify_tx: Vec::new(),
        }
    }

    pub fn notify_subscribers(&mut self) {
        self.notify_tx.retain(|tx| tx.send(()).is_ok());
    }
}

#[derive(Clone)]
pub struct ClipboardHandle {
    pub state: Arc<Mutex<SharedClipboardState>>,
    pub tx: Sender<ClipboardCommand>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_is_password_hint() {
        assert!(is_password_hint("x-kde-passwordManagerHint"));
        assert!(is_password_hint("X-KDE-PASSWORDMANAGERHINT"));
        assert!(is_password_hint("application/x-password-manager"));
        assert!(is_password_hint("application/x-keepassxc-password"));
        assert!(is_password_hint("application/x-bitwarden-secret"));
        assert!(is_password_hint("clipboard_manager_nopickup"));
        assert!(is_password_hint("CLIPBOARD_MANAGER_NOPICKUP"));
        assert!(is_password_hint("x-kde-promptsSecret"));
        assert!(is_password_hint("x-kde-secret"));

        assert!(!is_password_hint("text/plain"));
        assert!(!is_password_hint("text/html"));
        assert!(!is_password_hint("image/png"));
        assert!(!is_password_hint("text/uri-list"));
    }

    #[test]
    fn test_format_report() {
        let stats = DiskStats {
            files: 3,
            dirs: 0,
            symlinks: 0,
            total_bytes: 1024,
        };
        assert_eq!(stats.format_report(), "3 files • 1.0 KB");

        let single_file = DiskStats {
            files: 1,
            dirs: 1,
            symlinks: 0,
            total_bytes: 500,
        };
        assert_eq!(single_file.format_report(), "1 file, 1 dir • 500 B");
    }
}
