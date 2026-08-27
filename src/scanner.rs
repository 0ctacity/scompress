use crate::applesauce::inspect_file;
use crate::model::{ProjectGroup, SessionFile, Tool, ToolGroup};
use anyhow::Result;
use serde::Deserialize;
use std::collections::{BTreeMap, HashMap};
use std::fs::File;
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::time::SystemTime;
use walkdir::WalkDir;

pub fn codex_sessions_dir() -> Option<PathBuf> {
    std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".codex").join("sessions"))
}

pub fn codex_session_index_path() -> Option<PathBuf> {
    std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".codex").join("session_index.jsonl"))
}

pub fn claude_projects_dir() -> Option<PathBuf> {
    std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".claude").join("projects"))
}

#[derive(Deserialize)]
struct RawIndexEntry {
    id: String,
    thread_name: String,
    updated_at: Option<String>,
}

/// Load `~/.codex/session_index.jsonl` once, returning a map from UUID to the latest thread_name.
pub fn load_codex_session_index(index_path: &Path) -> HashMap<String, String> {
    let mut map: HashMap<String, (String, Option<String>)> = HashMap::new();

    if let Ok(file) = File::open(index_path) {
        let reader = BufReader::new(file);
        for line in reader.lines().filter_map(|l| l.ok()) {
            let trimmed = line.trim();
            if trimmed.is_empty() {
                continue;
            }
            if let Ok(entry) = serde_json::from_str::<RawIndexEntry>(trimmed) {
                let id = entry.id.trim().to_string();
                let thread_name = entry.thread_name.trim().to_string();
                if id.is_empty() || thread_name.is_empty() {
                    continue;
                }

                if let Some(existing) = map.get_mut(&id) {
                    let should_replace = match (&entry.updated_at, &existing.1) {
                        (Some(new_ts), Some(old_ts)) => new_ts >= old_ts,
                        (Some(_), None) => true,
                        (None, _) => false,
                    };
                    if should_replace {
                        *existing = (thread_name, entry.updated_at);
                    }
                } else {
                    map.insert(id, (thread_name, entry.updated_at));
                }
            }
        }
    }

    map.into_iter().map(|(k, (name, _))| (k, name)).collect()
}

/// Extract session UUID from filename or stem.
/// Format: rollout-YYYY-MM-DDTHH-MM-SS-<UUID>.jsonl
pub fn extract_codex_uuid(file_name: &str) -> Option<String> {
    let stem = file_name.strip_suffix(".jsonl").unwrap_or(file_name);
    if let Some(rest) = stem.strip_prefix("rollout-") {
        // rest: 2026-03-02T23-55-02-019cb055-3c72-7182-b6ac-8449d79a0cbf
        if rest.len() >= 20 {
            let uuid_part = &rest[20..];
            if !uuid_part.is_empty() {
                return Some(uuid_part.to_string());
            }
        }
    }
    None
}

/// Extract project name, optional thread_name title, and display fallback from a Codex session.
pub fn parse_codex_metadata(
    path: &Path,
    index_map: &HashMap<String, String>,
) -> (String, Option<String>, String) {
    let file_name = path
        .file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_default();

    let mut project = "Default".to_string();
    let mut file_id = None;

    // Try reading the first line JSON to find cwd or repo or id
    if let Ok(file) = File::open(path) {
        let mut reader = BufReader::new(file);
        let mut first_line = String::new();
        if reader.read_line(&mut first_line).is_ok() {
            if let Ok(val) = serde_json::from_str::<serde_json::Value>(&first_line) {
                if let Some(id_str) = val.pointer("/payload/id").and_then(|v| v.as_str()) {
                    file_id = Some(id_str.to_string());
                }

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

    let uuid = file_id.or_else(|| extract_codex_uuid(&file_name));
    let title = uuid.as_ref().and_then(|id| index_map.get(id).cloned());

    let display_title = parse_codex_title(&file_name);

    (project, title, display_title)
}

pub fn parse_codex_title(file_name: &str) -> String {
    let stem = file_name.strip_suffix(".jsonl").unwrap_or(file_name);
    if let Some(rest) = stem.strip_prefix("rollout-") {
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
pub fn parse_claude_metadata(path: &Path) -> (String, Option<String>, String) {
    let mut project = "Default".to_string();
    let mut title = None;

    if let Some(parent) = path.parent() {
        if let Some(dir_name) = parent.file_name().and_then(|n| n.to_str()) {
            let clean_name = dir_name
                .split('_')
                .filter(|s| !s.is_empty())
                .last()
                .unwrap_or(dir_name);
            project = clean_name.to_string();
        }
    }

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
                if let Some(t) = val.get("title").and_then(|v| v.as_str()) {
                    if !t.trim().is_empty() {
                        title = Some(t.trim().to_string());
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

    let display_title = format!("Session ({})", short_id);

    (project, title, display_title)
}

pub async fn scan_codex() -> Result<Vec<SessionFile>> {
    smol::unblock(|| {
        let index_map = if let Some(idx_path) = codex_session_index_path() {
            load_codex_session_index(&idx_path)
        } else {
            HashMap::new()
        };

        if let Some(dir) = codex_sessions_dir() {
            scan_codex_dir_sync(&dir, &index_map)
        } else {
            Ok(Vec::new())
        }
    })
    .await
}

fn scan_codex_dir_sync(
    dir: &Path,
    index_map: &HashMap<String, String>,
) -> Result<Vec<SessionFile>> {
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
        if !entry.file_type().is_file() || entry.path_is_symlink() {
            continue;
        }

        let meta = match entry.metadata() {
            Ok(m) => m,
            Err(_) => continue,
        };

        let modified_at = meta.modified().unwrap_or(SystemTime::UNIX_EPOCH);
        let (compressed, logical_size, physical_size) = inspect_file(path);
        let (project, title, display_title) = parse_codex_metadata(path, index_map);

        results.push(SessionFile {
            tool: Tool::Codex,
            project,
            title,
            display_title,
            path: path.to_path_buf(),
            logical_size,
            physical_size,
            compressed,
            modified_at,
        });
    }

    results.sort_by(|a, b| b.modified_at.cmp(&a.modified_at));
    Ok(results)
}

pub async fn scan_claude() -> Result<Vec<SessionFile>> {
    smol::unblock(|| {
        if let Some(dir) = claude_projects_dir() {
            scan_claude_dir_sync(&dir)
        } else {
            Ok(Vec::new())
        }
    })
    .await
}

fn scan_claude_dir_sync(dir: &Path) -> Result<Vec<SessionFile>> {
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
        if !entry.file_type().is_file() || entry.path_is_symlink() {
            continue;
        }
        if !path.extension().map_or(false, |ext| ext == "jsonl") {
            continue;
        }

        let meta = match entry.metadata() {
            Ok(m) => m,
            Err(_) => continue,
        };

        let modified_at = meta.modified().unwrap_or(SystemTime::UNIX_EPOCH);
        let (compressed, logical_size, physical_size) = inspect_file(path);
        let (project, title, display_title) = parse_claude_metadata(path);

        results.push(SessionFile {
            tool: Tool::Claude,
            project,
            title,
            display_title,
            path: path.to_path_buf(),
            logical_size,
            physical_size,
            compressed,
            modified_at,
        });
    }

    results.sort_by(|a, b| b.modified_at.cmp(&a.modified_at));
    Ok(results)
}

pub async fn scan_all() -> Result<Vec<SessionFile>> {
    let (codex_res, claude_res) = smol::future::zip(scan_codex(), scan_claude()).await;
    let mut codex = codex_res?;
    let mut claude = claude_res?;
    codex.append(&mut claude);
    codex.sort_by(|a, b| b.modified_at.cmp(&a.modified_at));
    Ok(codex)
}

/// Convert a list of `SessionFile` into a sorted hierarchy of `ToolGroup`s.
/// Sorting rules:
/// 1. Tools: Codex first, then Claude.
/// 2. Projects inside each tool: sorted by most recently active session descending.
/// 3. Threads inside each project: newest first (modified_at descending).
pub fn build_tool_groups(sessions: &[SessionFile]) -> Vec<ToolGroup> {
    let mut tool_map: BTreeMap<Tool, BTreeMap<String, Vec<SessionFile>>> = BTreeMap::new();

    for s in sessions {
        tool_map
            .entry(s.tool)
            .or_default()
            .entry(s.project.clone())
            .or_default()
            .push(s.clone());
    }

    let mut tool_groups = Vec::new();

    // Specific order: Codex then Claude
    let tools_in_order = [Tool::Codex, Tool::Claude];

    for &tool in &tools_in_order {
        if let Some(projects_map) = tool_map.remove(&tool) {
            let mut project_groups = Vec::new();
            for (name, mut group_sessions) in projects_map {
                // Sort threads by modified_at descending (newest first)
                group_sessions.sort_by(|a, b| b.modified_at.cmp(&a.modified_at));
                project_groups.push(ProjectGroup {
                    name,
                    sessions: group_sessions,
                });
            }

            // Sort projects by most recently active session descending
            project_groups.sort_by(|a, b| b.latest_modified().cmp(&a.latest_modified()));

            tool_groups.push(ToolGroup {
                tool,
                projects: project_groups,
            });
        }
    }

    tool_groups
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use std::time::Duration;

    #[test]
    fn test_load_codex_session_index() {
        let temp_dir =
            std::env::temp_dir().join(format!("scompress_index_test_{}", std::process::id()));
        let _ = std::fs::create_dir_all(&temp_dir);
        let index_file = temp_dir.join("session_index.jsonl");

        let content = r#"
{"id":"uuid-1","thread_name":"First Title","updated_at":"2026-03-08T00:00:00Z"}
{"id":"uuid-1","thread_name":"Updated Title","updated_at":"2026-03-08T01:00:00Z"}
{"id":"uuid-2","thread_name":"Second Title","updated_at":"2026-03-08T00:30:00Z"}
"#;
        std::fs::write(&index_file, content).unwrap();

        let map = load_codex_session_index(&index_file);
        assert_eq!(map.get("uuid-1"), Some(&"Updated Title".to_string()));
        assert_eq!(map.get("uuid-2"), Some(&"Second Title".to_string()));

        let _ = std::fs::remove_dir_all(&temp_dir);
    }

    #[test]
    fn test_extract_codex_uuid() {
        let filename = "rollout-2026-03-02T23-55-02-019cb055-3c72-7182-b6ac-8449d79a0cbf.jsonl";
        assert_eq!(
            extract_codex_uuid(filename),
            Some("019cb055-3c72-7182-b6ac-8449d79a0cbf".to_string())
        );
    }

    #[test]
    fn test_parse_codex_metadata_with_index() {
        let temp_dir =
            std::env::temp_dir().join(format!("scompress_meta_test2_{}", std::process::id()));
        let _ = std::fs::create_dir_all(&temp_dir);
        let test_file =
            temp_dir.join("rollout-2026-03-02T23-55-02-019cb055-3c72-7182-b6ac-8449d79a0cbf.jsonl");

        let line = r#"{"type":"session_meta","payload":{"id":"019cb055-3c72-7182-b6ac-8449d79a0cbf","cwd":"/Users/test/workspace/my-proj"}}"#;
        let mut f = File::create(&test_file).unwrap();
        writeln!(f, "{}", line).unwrap();
        drop(f);

        let mut index_map = HashMap::new();
        index_map.insert(
            "019cb055-3c72-7182-b6ac-8449d79a0cbf".to_string(),
            "Refactor Session Architecture".to_string(),
        );

        let (project, title, display_title) = parse_codex_metadata(&test_file, &index_map);
        assert_eq!(project, "my-proj");
        assert_eq!(title, Some("Refactor Session Architecture".to_string()));
        assert_eq!(display_title, "2026-03-02 23:55:02 (019cb055)");

        let _ = std::fs::remove_dir_all(&temp_dir);
    }

    #[test]
    fn test_build_tool_groups_sorting() {
        let now = SystemTime::now();

        let s1 = SessionFile {
            tool: Tool::Codex,
            project: "proj-old".to_string(),
            title: Some("Thread Old".to_string()),
            display_title: "2026-01-01".to_string(),
            path: PathBuf::from("/tmp/1"),
            logical_size: 100,
            physical_size: 10,
            compressed: false,
            modified_at: now - Duration::from_secs(500),
        };
        let s2 = SessionFile {
            tool: Tool::Codex,
            project: "proj-new".to_string(),
            title: Some("Thread New 1".to_string()),
            display_title: "2026-02-01".to_string(),
            path: PathBuf::from("/tmp/2"),
            logical_size: 200,
            physical_size: 20,
            compressed: false,
            modified_at: now - Duration::from_secs(10),
        };
        let s3 = SessionFile {
            tool: Tool::Codex,
            project: "proj-new".to_string(),
            title: Some("Thread New 2".to_string()),
            display_title: "2026-02-02".to_string(),
            path: PathBuf::from("/tmp/3"),
            logical_size: 300,
            physical_size: 30,
            compressed: false,
            modified_at: now - Duration::from_secs(100),
        };

        let groups = build_tool_groups(&[s1, s2, s3]);
        assert_eq!(groups.len(), 1);
        assert_eq!(groups[0].tool, Tool::Codex);
        assert_eq!(groups[0].projects.len(), 2);
        // proj-new has the most recent session, should come first
        assert_eq!(groups[0].projects[0].name, "proj-new");
        assert_eq!(groups[0].projects[1].name, "proj-old");

        // inside proj-new, Thread New 1 is newer than Thread New 2
        assert_eq!(groups[0].projects[0].sessions[0].label(), "Thread New 1");
        assert_eq!(groups[0].projects[0].sessions[1].label(), "Thread New 2");
    }
}
