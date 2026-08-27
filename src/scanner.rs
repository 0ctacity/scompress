use crate::applesauce::inspect_file;
use crate::model::{SessionFile, Tool};
use anyhow::Result;
use std::path::{Path, PathBuf};
use std::time::SystemTime;
use walkdir::WalkDir;

pub fn codex_sessions_dir() -> Option<PathBuf> {
    std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".codex").join("sessions"))
}

pub fn claude_projects_dir() -> Option<PathBuf> {
    std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".claude").join("projects"))
}

pub fn scan_dir_sync<F>(dir: &Path, tool: Tool, filter: F) -> Result<Vec<SessionFile>>
where
    F: Fn(&Path) -> bool,
{
    let mut results = Vec::new();
    if !dir.exists() {
        return Ok(results);
    }

    for entry in WalkDir::new(dir)
        .follow_links(false)
        .into_iter()
        .filter_map(|e| e.ok())
    {
        let path = entry.path();
        if !entry.file_type().is_file() {
            continue;
        }
        // Exclude symlinks
        if entry.path_is_symlink() {
            continue;
        }
        if !filter(path) {
            continue;
        }

        let meta = match entry.metadata() {
            Ok(m) => m,
            Err(_) => continue,
        };

        let modified_at = meta.modified().unwrap_or(SystemTime::UNIX_EPOCH);
        let (compressed, logical_size, physical_size) = inspect_file(path);

        results.push(SessionFile {
            tool,
            path: path.to_path_buf(),
            logical_size,
            physical_size,
            compressed,
            modified_at,
        });
    }

    // Sort newest modified first
    results.sort_by(|a, b| b.modified_at.cmp(&a.modified_at));

    Ok(results)
}

pub async fn scan_codex() -> Result<Vec<SessionFile>> {
    smol::unblock(|| {
        if let Some(dir) = codex_sessions_dir() {
            scan_dir_sync(&dir, Tool::Codex, |_| true)
        } else {
            Ok(Vec::new())
        }
    })
    .await
}

pub async fn scan_claude() -> Result<Vec<SessionFile>> {
    smol::unblock(|| {
        if let Some(dir) = claude_projects_dir() {
            scan_dir_sync(&dir, Tool::Claude, |p| {
                p.extension().map_or(false, |ext| ext == "jsonl")
            })
        } else {
            Ok(Vec::new())
        }
    })
    .await
}

pub async fn scan_all() -> Result<Vec<SessionFile>> {
    let (codex_res, claude_res) = smol::future::zip(scan_codex(), scan_claude()).await;
    let mut codex = codex_res?;
    let mut claude = claude_res?;
    codex.append(&mut claude);
    codex.sort_by(|a, b| b.modified_at.cmp(&a.modified_at));
    Ok(codex)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs::File;

    #[test]
    fn test_scan_dir_sync() {
        let temp_dir =
            std::env::temp_dir().join(format!("scompress_scan_test_{}", std::process::id()));
        let sub = temp_dir.join("subproject");
        let _ = std::fs::create_dir_all(&sub);

        let file1 = temp_dir.join("session1.jsonl");
        let file2 = sub.join("session2.jsonl");
        let file_txt = temp_dir.join("notes.txt");

        File::create(&file1).unwrap();
        File::create(&file2).unwrap();
        File::create(&file_txt).unwrap();

        let claude_results = scan_dir_sync(&temp_dir, Tool::Claude, |p| {
            p.extension().map_or(false, |ext| ext == "jsonl")
        })
        .unwrap();

        assert_eq!(claude_results.len(), 2);
        assert!(claude_results.iter().all(|r| r.tool == Tool::Claude));

        let codex_results = scan_dir_sync(&temp_dir, Tool::Codex, |_| true).unwrap();
        assert_eq!(codex_results.len(), 3);

        let _ = std::fs::remove_dir_all(&temp_dir);
    }
}
