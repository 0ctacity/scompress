use crate::model::SessionFile;
use std::collections::HashSet;
use std::path::PathBuf;
use std::process::Command;
use std::time::{Duration, SystemTime};

pub const RECENT_MODIFICATION_THRESHOLD: Duration = Duration::from_secs(30);

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SkipReason {
    AlreadyCompressed,
    NotRegularFile,
    IsSymlink,
    CurrentlyOpen,
    RecentlyModified,
}

impl std::fmt::Display for SkipReason {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            SkipReason::AlreadyCompressed => write!(f, "already compressed"),
            SkipReason::NotRegularFile => write!(f, "not a regular file"),
            SkipReason::IsSymlink => write!(f, "symlink"),
            SkipReason::CurrentlyOpen => write!(f, "file is open by another process"),
            SkipReason::RecentlyModified => {
                write!(
                    f,
                    "modified recently (< {}s ago)",
                    RECENT_MODIFICATION_THRESHOLD.as_secs()
                )
            }
        }
    }
}

/// Run a single `lsof` call to find all open files under the provided directories or paths.
pub fn scan_open_files(search_roots: &[PathBuf]) -> HashSet<PathBuf> {
    let mut open_files = HashSet::new();
    if search_roots.is_empty() {
        return open_files;
    }

    let mut cmd = Command::new("lsof");
    cmd.arg("-F").arg("n");

    let mut has_existing_root = false;
    for root in search_roots {
        if root.exists() {
            cmd.arg("+D").arg(root);
            has_existing_root = true;
        }
    }

    if !has_existing_root {
        return open_files;
    }

    if let Ok(output) = cmd.output() {
        let stdout = String::from_utf8_lossy(&output.stdout);
        for line in stdout.lines() {
            if let Some(path_str) = line.strip_prefix('n') {
                let trimmed = path_str.trim();
                if !trimmed.is_empty() {
                    open_files.insert(PathBuf::from(trimmed));
                }
            }
        }
    }

    open_files
}

/// Check whether a session file is safe to compress.
pub fn check_compression_safety(
    session: &SessionFile,
    open_files: &HashSet<PathBuf>,
    now: SystemTime,
) -> Result<(), SkipReason> {
    if session.compressed {
        return Err(SkipReason::AlreadyCompressed);
    }

    // Check symlink
    if let Ok(symlink_meta) = std::fs::symlink_metadata(&session.path) {
        if symlink_meta.file_type().is_symlink() {
            return Err(SkipReason::IsSymlink);
        }
        if !symlink_meta.is_file() {
            return Err(SkipReason::NotRegularFile);
        }
    } else {
        return Err(SkipReason::NotRegularFile);
    }

    // Check if open by another process
    if open_files.contains(&session.path) {
        return Err(SkipReason::CurrentlyOpen);
    }

    // Check recent modification
    if let Ok(elapsed) = now.duration_since(session.modified_at) {
        if elapsed < RECENT_MODIFICATION_THRESHOLD {
            return Err(SkipReason::RecentlyModified);
        }
    } else {
        // If modified_at is in the future, treat as recently modified for safety
        return Err(SkipReason::RecentlyModified);
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::Tool;
    use std::fs::File;
    use std::os::unix::fs::symlink;
    use std::time::Duration;

    #[test]
    fn test_skip_already_compressed() {
        let session = SessionFile {
            tool: Tool::Codex,
            project: "test-proj".to_string(),
            title: None,
            display_title: "Session 1".to_string(),
            path: PathBuf::from("/tmp/dummy"),
            logical_size: 100,
            physical_size: 20,
            compressed: true,
            modified_at: SystemTime::now() - Duration::from_secs(100),
        };
        let open_files = HashSet::new();
        assert_eq!(
            check_compression_safety(&session, &open_files, SystemTime::now()),
            Err(SkipReason::AlreadyCompressed)
        );
    }

    #[test]
    fn test_skip_recently_modified() {
        let dir = std::env::temp_dir().join(format!("scompress_test_{}", std::process::id()));
        let _ = std::fs::create_dir_all(&dir);
        let file_path = dir.join("recent.jsonl");
        File::create(&file_path).unwrap();

        let session = SessionFile {
            tool: Tool::Claude,
            project: "test-proj".to_string(),
            title: None,
            display_title: "Session 1".to_string(),
            path: file_path.clone(),
            logical_size: 100,
            physical_size: 100,
            compressed: false,
            modified_at: SystemTime::now() - Duration::from_secs(5), // 5 seconds ago
        };
        let open_files = HashSet::new();
        assert_eq!(
            check_compression_safety(&session, &open_files, SystemTime::now()),
            Err(SkipReason::RecentlyModified)
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn test_skip_symlink() {
        let dir =
            std::env::temp_dir().join(format!("scompress_symlink_test_{}", std::process::id()));
        let _ = std::fs::create_dir_all(&dir);
        let target_file = dir.join("target.json");
        File::create(&target_file).unwrap();
        let link_file = dir.join("link.json");
        symlink(&target_file, &link_file).unwrap();

        let session = SessionFile {
            tool: Tool::Codex,
            project: "test-proj".to_string(),
            title: None,
            display_title: "Session 1".to_string(),
            path: link_file.clone(),
            logical_size: 100,
            physical_size: 100,
            compressed: false,
            modified_at: SystemTime::now() - Duration::from_secs(100),
        };
        let open_files = HashSet::new();
        assert_eq!(
            check_compression_safety(&session, &open_files, SystemTime::now()),
            Err(SkipReason::IsSymlink)
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn test_skip_open_file() {
        let dir = std::env::temp_dir().join(format!("scompress_open_test_{}", std::process::id()));
        let _ = std::fs::create_dir_all(&dir);
        let file_path = dir.join("open.json");
        File::create(&file_path).unwrap();

        let session = SessionFile {
            tool: Tool::Codex,
            project: "test-proj".to_string(),
            title: None,
            display_title: "Session 1".to_string(),
            path: file_path.clone(),
            logical_size: 100,
            physical_size: 100,
            compressed: false,
            modified_at: SystemTime::now() - Duration::from_secs(100),
        };
        let mut open_files = HashSet::new();
        open_files.insert(file_path.clone());

        assert_eq!(
            check_compression_safety(&session, &open_files, SystemTime::now()),
            Err(SkipReason::CurrentlyOpen)
        );

        let _ = std::fs::remove_dir_all(&dir);
    }
}
