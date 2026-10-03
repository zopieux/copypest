use rayon::prelude::*;
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};

pub fn percent_decode(s: &str) -> String {
    let mut bytes = Vec::with_capacity(s.len());
    let mut chars = s.as_bytes().iter().copied();
    while let Some(b) = chars.next() {
        if b == b'%' {
            let h1 = chars.next();
            let h2 = chars.next();
            if let (Some(h1), Some(h2)) = (h1, h2) {
                let hex_str = [h1, h2];
                if let Ok(s) = std::str::from_utf8(&hex_str)
                    && let Ok(val) = u8::from_str_radix(s, 16)
                {
                    bytes.push(val);
                    continue;
                }
                bytes.push(b'%');
                bytes.push(h1);
                bytes.push(h2);
                continue;
            } else {
                bytes.push(b'%');
                if let Some(h1) = h1 {
                    bytes.push(h1);
                }
                continue;
            }
        }
        bytes.push(b);
    }
    String::from_utf8_lossy(&bytes).into_owned()
}

pub fn uri_to_path(uri: &str) -> String {
    let uri = uri.trim();
    let path = if let Some(stripped) = uri.strip_prefix("file://") {
        stripped
    } else {
        uri
    };
    percent_decode(path)
}

pub fn ellide_home(path_str: &str) -> String {
    if let Ok(home) = std::env::var("HOME") {
        if path_str == home {
            return "~".to_string();
        } else if let Some(stripped) = path_str.strip_prefix(&home)
            && stripped.starts_with('/')
        {
            return format!("~{stripped}");
        }
    }
    path_str.to_string()
}

pub fn ellide_path_with_measurer<F>(path_str: &str, max_width: f32, measure: F) -> String
where
    F: Fn(&str) -> f32,
{
    let normalized = ellide_home(path_str);

    if measure(&normalized) <= max_width {
        return normalized;
    }

    let is_home = normalized.starts_with('~');
    let trimmed = if is_home {
        normalized.strip_prefix('~').unwrap_or(&normalized)
    } else {
        &normalized
    };

    let parts: Vec<&str> = trimmed.split('/').filter(|s| !s.is_empty()).collect();
    if parts.is_empty() {
        return normalized;
    }

    let prefix = if is_home {
        "~/"
    } else if normalized.starts_with('/') {
        "/"
    } else {
        ""
    };

    let last = parts.last().unwrap();

    if parts.len() > 1 {
        // Progressively collapse intermediate directories to "…" until it fits
        for keep_from_right in (0..parts.len() - 1).rev() {
            let kept_dirs = &parts[parts.len() - 1 - keep_from_right..parts.len() - 1];
            let candidate = if keep_from_right == 0 {
                format!("{prefix}…/{last}")
            } else {
                format!("{prefix}…/{}/{last}", kept_dirs.join("/"))
            };
            if measure(&candidate) <= max_width {
                return candidate;
            }
        }
    } else {
        let candidate = format!("{prefix}{last}");
        if measure(&candidate) <= max_width {
            return candidate;
        }
    }

    // If even prefix…/filename doesn't fit, truncate filename from the right
    let base = if parts.len() == 1 {
        prefix.to_string()
    } else {
        format!("{prefix}…/")
    };
    let base_w = measure(&base);
    let ellip_w = measure("…");
    let avail_for_chars = (max_width - base_w - ellip_w).max(0.0);
    let mut fit_len = 0;
    for (idx, c) in last.char_indices() {
        let end = idx + c.len_utf8();
        let sub = &last[..end];
        if measure(sub) <= avail_for_chars {
            fit_len = end;
        } else {
            break;
        }
    }

    if fit_len == 0 {
        format!("{base}…")
    } else {
        format!("{base}{}…", &last[..fit_len])
    }
}

use crate::model::DiskStats;

pub fn compute_disk_stats(paths: &[String]) -> DiskStats {
    paths
        .par_iter()
        .map(|p| {
            let path = Path::new(p);
            compute_path_stats(path, None)
        })
        .reduce(DiskStats::default, |mut a, b| {
            a.add(b);
            a
        })
}

fn compute_path_stats(path: &Path, root_dev: Option<u64>) -> DiskStats {
    let mut stats = DiskStats::default();
    let meta = match std::fs::symlink_metadata(path) {
        Ok(m) => m,
        Err(_) => return stats,
    };

    let dev = meta.dev();
    let root_dev = root_dev.unwrap_or(dev);
    if dev != root_dev {
        // Staying in the same xdev
        return stats;
    }

    if meta.file_type().is_symlink() {
        stats.symlinks = 1;
        stats.total_bytes = meta.len();
    } else if meta.is_dir() {
        stats.dirs = 1;
        stats.total_bytes = meta.len();
        if let Ok(entries) = std::fs::read_dir(path) {
            let sub_paths: Vec<PathBuf> =
                entries.filter_map(|e| e.ok().map(|e| e.path())).collect();

            let sub_stats: DiskStats = sub_paths
                .par_iter()
                .map(|p| compute_path_stats(p, Some(root_dev)))
                .reduce(DiskStats::default, |mut a, b| {
                    a.add(b);
                    a
                });
            stats.add(sub_stats);
        }
    } else if meta.is_file() {
        stats.files = 1;
        stats.total_bytes = meta.len();
    }

    stats
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_ellide_path_measurer() {
        let char_measure = |s: &str| s.chars().count() as f32;

        // Fits completely
        assert_eq!(
            ellide_path_with_measurer("/short/path.txt", 30.0, char_measure),
            "/short/path.txt"
        );

        // Progressively collapses intermediate dirs
        let path = "/a/b/c/d/file.txt"; // 17 chars
        // keep_from_right = 2: /…/c/d/file.txt (15 chars)
        assert_eq!(
            ellide_path_with_measurer(path, 15.0, char_measure),
            "/…/c/d/file.txt"
        );
        // keep_from_right = 1: /…/d/file.txt (13 chars)
        assert_eq!(
            ellide_path_with_measurer(path, 13.0, char_measure),
            "/…/d/file.txt"
        );
        // keep_from_right = 0: /…/file.txt (11 chars)
        assert_eq!(
            ellide_path_with_measurer(path, 11.0, char_measure),
            "/…/file.txt"
        );

        // When even /…/file.txt doesn't fit, truncates filename keeping prefix
        assert_eq!(
            ellide_path_with_measurer(path, 8.0, char_measure),
            "/…/file…"
        );
    }
}
