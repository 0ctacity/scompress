use crate::applesauce::inspect_file;
use crate::model::{SessionFile, Tool};
use anyhow::Result;
use std::fs::File;
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::time::SystemTime;
use walkdir::WalkDir;

pub fn codex_sessions_dir() -> Option<PathBuf> {
    std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".codex").join("sessions"))
}

pub fn claude_projects_dir() -> Option<PathBuf> {
    std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".claude").join("projects"))
}

/// Extract project name and title from a Codex session JSONL file.
pub fn parse_codex_metadata(path: &Path) -> (String, String) {
    let file_name = path
        .file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_default();

    let mut project = "Default".to_string();

    // Try reading the first line JSON to find cwd or repo
    if let Ok(file) = File::open(path) {
        let mut reader = BufReader::new(file);
        let mut first_line = String::new();
        if reader.read_line(&mut first_line).is_ok() {
            if let Ok(val) = serde_json::from_str::<serde_json::Value>(&first_line) {
                if let Some(cwd) = val.pointer("/payload/cwd").and_then(|v| v.as_str()) {
                    let p = Path::new(cwd);
                    if let Some(name) = p.file_name().and_then(|n| n.to_str()) {
                        if !name.is_empty() {
                            project = name.to_string();
                        }
                    }
                } else if let Some(repo_url) = val
                    .pointer("/payload/git/repository_url")
                    .and_then(|v| v.as_str())
                {
                    let trimmed = repo_url.trim_end_matches(".git");
                    if let Some(name) = trimmed.split('/').last() {
                        if !name.is_empty() {
                            project = name.to_string();
                        }
                    }
                }
            }
        }
    }

    // Parse display title from rollout filename
    // Format: rollout-2026-03-02T23-55-02-019cb055-3c72-7182-b6ac-8449d79a0cbf.jsonl
    let display_title = parse_codex_title(&file_name);

    (project, display_title)
}

fn parse_codex_title(file_name: &str) -> String {
    let stem = file_name.strip_suffix(".jsonl").unwrap_or(file_name);
    if let Some(rest) = stem.strip_prefix("rollout-") {
        // rest is 2026-03-02T23-55-02-019cb055-...
        if rest.len() >= 19 {
            let date = &rest[0..10]; // 2026-03-02
            let time_raw = &rest[11..19]; // 23-55-02
            let time = time_raw.replace('-', ":");
            let short_id = if rest.len() > 20 {
                let after_time = &rest[20..];
                after_time.split('-').next().unwrap_or("")
            } else {
                ""
            };

            if !short_id.is_empty() {
                return format!("{} {} ({})", date, time, short_id);
            } else {
                return format!("{} {}", date, time);
            }
        }
    }
    stem.to_string()
}

/// Extract project name and title for Claude Code session JSONL.
pub fn parse_claude_metadata(path: &Path) -> (String, String) {
    let mut project = "Default".to_string();

    // In Claude Code, path is ~/.claude/projects/<project-slug>/<session-id>.jsonl
    if let Some(parent) = path.parent() {
        if let Some(dir_name) = parent.file_name().and_then(|n| n.to_str()) {
            // Slug might be _Users_username_Desktop_VsCode_project
            let clean_name = dir_name
                .split('_')
                .filter(|s| !s.is_empty())
                .last()
                .unwrap_or(dir_name);
            project = clean_name.to_string();
        }
    }

    // Try reading first line of Claude session for project/cwd metadata if available
    if let Ok(file) = File::open(path) {
        let mut reader = BufReader::new(file);
        let mut first_line = String::new();
        if reader.read_line(&mut first_line).is_ok() {
            if let Ok(val) = serde_json::from_str::<serde_json::Value>(&first_line) {
                if let Some(cwd) = val.get("cwd").and_then(|v| v.as_str()) {
                    let p = Path::new(cwd);
                    if let Some(name) = p.file_name().and_then(|n| n.to_str()) {
                        if !name.is_empty() {
                            project = name.to_string();
                        }
                    }
                }
            }
        }
    }

    let file_stem = path
        .file_stem()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_else(|| "Session".to_string());

    let short_id = if file_stem.len() > 8 {
        &file_stem[0..8]
    } else {
        &file_stem
    };

    let title = format!("Session ({})", short_id);

    (project, title)
}

fn scan_dir_sync<F>(dir: &Path, tool: Tool, filter: F) -> Result<Vec<SessionFile>>
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

        let (project, display_title) = match tool {
            Tool::Codex => parse_codex_metadata(path),
            Tool::Claude => parse_claude_metadata(path),
        };

        results.push(SessionFile {
            tool,
            project,
            display_title,
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
    use std::io::Write;

    #[test]
    fn test_parse_codex_title() {
        let name = "rollout-2026-03-02T23-55-02-019cb055-3c72-7182-b6ac-8449d79a0cbf.jsonl";
        assert_eq!(parse_codex_title(name), "2026-03-02 23:55:02 (019cb055)");
    }

    #[test]
    fn test_parse_codex_metadata_with_cwd() {
        let temp_dir =
            std::env::temp_dir().join(format!("scompress_meta_test_{}", std::process::id()));
        let _ = std::fs::create_dir_all(&temp_dir);
        let test_file = temp_dir.join("rollout-2026-03-02T23-55-02-019cb055-3c72.jsonl");

        let line = r#"{"type":"session_meta","payload":{"cwd":"/Users/test/workspace/my-awesome-project"}}"#;
        let mut f = File::create(&test_file).unwrap();
        writeln!(f, "{}", line).unwrap();
        drop(f);

        let (project, title) = parse_codex_metadata(&test_file);
        assert_eq!(project, "my-awesome-project");
        assert_eq!(title, "2026-03-02 23:55:02 (019cb055)");

        let _ = std::fs::remove_dir_all(&temp_dir);
    }

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
