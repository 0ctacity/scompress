use crate::applesauce::inspect_file;
use crate::model::{ProjectGroup, SessionFile, Tool, ToolGroup};
use anyhow::Result;
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

/// Load `~/.codex/session_index.jsonl` once and build a lookup: session_id -> thread_name.
/// If multiple entries exist for the same UUID, keep the one with the newest `updated_at`.
pub fn load_codex_session_index(index_path: &Path) -> HashMap<String, String> {
    let mut index_map: HashMap<String, (String, String)> = HashMap::new();

    if let Ok(file) = File::open(index_path) {
        let reader = BufReader::new(file);
        for line in reader.lines().map_while(Result::ok) {
            if line.trim().is_empty() {
                continue;
            }
            if let Ok(val) = serde_json::from_str::<serde_json::Value>(&line) {
                let id = val.get("id").and_then(|v| v.as_str());
                let thread_name = val.get("thread_name").and_then(|v| v.as_str());
                let updated_at = val.get("updated_at").and_then(|v| v.as_str()).unwrap_or("");

                if let (Some(uuid), Some(name)) = (id, thread_name)
                    && !name.trim().is_empty() {
                        let should_insert = match index_map.get(uuid) {
                            Some((_, existing_updated)) => updated_at >= existing_updated.as_str(),
                            None => true,
                        };
                        if should_insert {
                            index_map.insert(
                                uuid.to_string(),
                                (name.trim().to_string(), updated_at.to_string()),
                            );
                        }
                    }
            }
        }
    }

    index_map
        .into_iter()
        .map(|(k, (name, _))| (k, name))
        .collect()
}

/// Extract UUID from rollout filename format:
/// e.g. "rollout-2026-02-27T10-47-59-019542a1-cf0b-7412-a7e8-3841aee50b69.jsonl"
/// returns "019542a1-cf0b-7412-a7e8-3841aee50b69"
pub fn extract_codex_uuid(filename: &str) -> Option<String> {
    let stem = filename.strip_suffix(".jsonl").unwrap_or(filename);
    let parts: Vec<&str> = stem.split('-').collect();
    if parts.len() >= 5 {
        // A UUID has 5 hyphenated segments: 8-4-4-4-12 hex chars
        let uuid_parts = &parts[parts.len() - 5..];
        if uuid_parts[0].len() == 8
            && uuid_parts[1].len() == 4
            && uuid_parts[2].len() == 4
            && uuid_parts[3].len() == 4
            && uuid_parts[4].len() == 12
        {
            return Some(uuid_parts.join("-"));
        }
    }
    None
}

/// Parse metadata for Codex rollout files.
/// Extract project name from the first JSON line (`cwd` or `payload.cwd`),
/// and resolve the thread name from the preloaded `session_index_map`.
pub fn parse_codex_metadata(
    path: &Path,
    session_index_map: &HashMap<String, String>,
) -> (String, Option<String>, String) {
    let file_stem = path
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("unknown");

    // Display title fallback: timestamp portion or clean rollout stem
    let display_title = if let Some(stripped) = file_stem.strip_prefix("rollout-") {
        if let Some((ts, _uuid)) = stripped.split_once('-') {
            ts.replace('T', " ")
        } else {
            stripped.to_string()
        }
    } else {
        file_stem.to_string()
    };

    let mut project = "unknown".to_string();
    let mut title: Option<String> = None;

    // Match thread_name from session_index lookup
    if let Some(file_name) = path.file_name().and_then(|n| n.to_str())
        && let Some(uuid) = extract_codex_uuid(file_name)
            && let Some(indexed_title) = session_index_map.get(&uuid) {
                title = Some(indexed_title.clone());
            }

    // Only read the first line for project/cwd - do NOT parse the entire transcript
    if let Ok(file) = File::open(path) {
        let mut reader = BufReader::new(file);
        let mut first_line = String::new();
        if reader.read_line(&mut first_line).is_ok()
            && let Ok(val) = serde_json::from_str::<serde_json::Value>(&first_line)
        {
            let cwd_val = val
                .get("payload")
                .and_then(|p| p.get("cwd"))
                .or_else(|| val.get("cwd"))
                .and_then(|v| v.as_str());

            if let Some(cwd) = cwd_val {
                let p = Path::new(cwd);
                if let Some(name) = p.file_name().and_then(|n| n.to_str())
                    && !name.is_empty()
                {
                    project = name.to_string();
                }
            }
        }
    }

    (project, title, display_title)
}

/// Parse metadata for Claude Code session files.
/// Derives project from directory name and optional title from the first JSON line.
pub fn parse_claude_metadata(path: &Path) -> (String, Option<String>, String) {
    let file_stem = path
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("unknown");

    let display_title = if file_stem.len() > 12 {
        file_stem[..12].to_string()
    } else {
        file_stem.to_string()
    };

    // Directory is usually ~/.claude/projects/<project_hash_or_name>/<session>.jsonl
    let mut project = path
        .parent()
        .and_then(|p| p.file_name())
        .and_then(|n| n.to_str())
        .unwrap_or("unknown")
        .to_string();

    // Clean up project name if it looks like `-Users-username-projects-foo`
    if let Some(dir_name) = path
        .parent()
        .and_then(|p| p.file_name())
        .and_then(|n| n.to_str())
        && (dir_name.starts_with('-') || dir_name.contains('_')) {
            let clean_name = dir_name
                .split('_')
                .rfind(|s| !s.is_empty())
                .unwrap_or(dir_name);
            let clean_name = clean_name
                .split('-')
                .rfind(|s| !s.is_empty())
                .unwrap_or(clean_name);
            if !clean_name.is_empty() {
                project = clean_name.to_string();
            }
        }

    let mut title = None;

    if let Ok(file) = File::open(path) {
        let mut reader = BufReader::new(file);
        let mut first_line = String::new();
        if reader.read_line(&mut first_line).is_ok()
            && let Ok(val) = serde_json::from_str::<serde_json::Value>(&first_line)
        {
            if let Some(cwd) = val.get("cwd").and_then(|v| v.as_str()) {
                let p = Path::new(cwd);
                if let Some(name) = p.file_name().and_then(|n| n.to_str())
                    && !name.is_empty()
                {
                    project = name.to_string();
                }
            }
            if let Some(t) = val.get("title").and_then(|v| v.as_str())
                && !t.trim().is_empty()
            {
                title = Some(t.trim().to_string());
            }
        }
    }

    (project, title, display_title)
}

pub async fn scan_codex() -> Result<Vec<SessionFile>> {
    smol::unblock(|| {
        if let Some(dir) = codex_sessions_dir() {
            let index_path = codex_session_index_path();
            let index_map = index_path
                .as_deref()
                .map(load_codex_session_index)
                .unwrap_or_default();
            scan_codex_dir_sync(&dir, &index_map)
        } else {
            Ok(Vec::new())
        }
    })
    .await
}

fn scan_codex_dir_sync(
    dir: &Path,
    session_index_map: &HashMap<String, String>,
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

        let file_name = match path.file_name().and_then(|n| n.to_str()) {
            Some(n) => n,
            None => continue,
        };

        if !file_name.starts_with("rollout-") || !file_name.ends_with(".jsonl") {
            continue;
        }

        let meta = match entry.metadata() {
            Ok(m) => m,
            Err(_) => continue,
        };

        let modified_at = meta.modified().unwrap_or(SystemTime::UNIX_EPOCH);
        let (compressed, logical_size, physical_size) = inspect_file(path);
        let (project, title, display_title) = parse_codex_metadata(path, session_index_map);

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

    results.sort_by_key(|b| std::cmp::Reverse(b.modified_at));
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
        if path.extension().is_none_or(|ext| ext != "jsonl") {
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

    results.sort_by_key(|b| std::cmp::Reverse(b.modified_at));
    Ok(results)
}

pub async fn scan_all() -> Result<Vec<SessionFile>> {
    let (codex_res, claude_res) = smol::future::zip(scan_codex(), scan_claude()).await;
    let mut codex = codex_res?;
    let mut claude = claude_res?;
    codex.append(&mut claude);
    codex.sort_by_key(|b| std::cmp::Reverse(b.modified_at));
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
                group_sessions.sort_by_key(|b| std::cmp::Reverse(b.modified_at));
                project_groups.push(ProjectGroup {
                    name,
                    sessions: group_sessions,
                });
            }

            // Sort projects by most recently active session descending
            project_groups.sort_by_key(|b| std::cmp::Reverse(b.latest_modified()));

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
    fn test_extract_codex_uuid() {
        let name = "rollout-2026-02-27T10-47-59-019542a1-cf0b-7412-a7e8-3841aee50b69.jsonl";
        assert_eq!(
            extract_codex_uuid(name),
            Some("019542a1-cf0b-7412-a7e8-3841aee50b69".to_string())
        );

        let non_rollout = "session_meta.jsonl";
        assert_eq!(extract_codex_uuid(non_rollout), None);
    }

    #[test]
    fn test_load_codex_session_index() {
        let temp_dir =
            std::env::temp_dir().join(format!("scompress_index_test_{}", std::process::id()));
        let _ = std::fs::create_dir_all(&temp_dir);
        let index_file = temp_dir.join("session_index.jsonl");

        let content = r#"{"id":"uuid-1","thread_name":"First Title","updated_at":"2026-02-27T10:00:00Z"}
{"id":"uuid-2","thread_name":"Second Title","updated_at":"2026-02-27T11:00:00Z"}
{"id":"uuid-1","thread_name":"Updated First Title","updated_at":"2026-02-27T12:00:00Z"}
"#;
        std::fs::write(&index_file, content).unwrap();

        let map = load_codex_session_index(&index_file);
        assert_eq!(map.get("uuid-1"), Some(&"Updated First Title".to_string()));
        assert_eq!(map.get("uuid-2"), Some(&"Second Title".to_string()));

        let _ = std::fs::remove_dir_all(&temp_dir);
    }

    #[test]
    fn test_parse_codex_metadata_with_index() {
        let temp_dir =
            std::env::temp_dir().join(format!("scompress_meta_test_{}", std::process::id()));
        let _ = std::fs::create_dir_all(&temp_dir);
        let file_path =
            temp_dir.join("rollout-2026-02-27T10-47-59-019542a1-cf0b-7412-a7e8-3841aee50b69.jsonl");

        let mut index_map = HashMap::new();
        index_map.insert(
            "019542a1-cf0b-7412-a7e8-3841aee50b69".to_string(),
            "My Indexed Thread".to_string(),
        );

        let mut f = File::create(&file_path).unwrap();
        writeln!(
            f,
            "{{\"type\":\"session_meta\",\"payload\":{{\"id\":\"uuid-test-1234\",\"cwd\":\"/Users/test/workspace/my-cool-project\"}}}}"
        )
        .unwrap();
        drop(f);

        let (project, title, _display) = parse_codex_metadata(&file_path, &index_map);
        assert_eq!(project, "my-cool-project");
        assert_eq!(title, Some("My Indexed Thread".to_string()));

        let _ = std::fs::remove_dir_all(&temp_dir);
    }

    #[test]
    fn test_build_tool_groups_sorting() {
        let now = SystemTime::now();
        let sessions = vec![
            SessionFile {
                tool: Tool::Claude,
                project: "claude-proj".to_string(),
                title: None,
                display_title: "c1".to_string(),
                path: PathBuf::from("/tmp/c1.jsonl"),
                logical_size: 100,
                physical_size: 10,
                compressed: false,
                modified_at: now,
            },
            SessionFile {
                tool: Tool::Codex,
                project: "older-proj".to_string(),
                title: None,
                display_title: "o1".to_string(),
                path: PathBuf::from("/tmp/o1.jsonl"),
                logical_size: 100,
                physical_size: 10,
                compressed: false,
                modified_at: now - Duration::from_secs(500),
            },
            SessionFile {
                tool: Tool::Codex,
                project: "newer-proj".to_string(),
                title: None,
                display_title: "n1".to_string(),
                path: PathBuf::from("/tmp/n1.jsonl"),
                logical_size: 100,
                physical_size: 10,
                compressed: false,
                modified_at: now - Duration::from_secs(10),
            },
        ];

        let groups = build_tool_groups(&sessions);
        assert_eq!(groups.len(), 2);
        assert_eq!(groups[0].tool, Tool::Codex);
        assert_eq!(groups[1].tool, Tool::Claude);

        // Under Codex, newer-proj should come before older-proj
        assert_eq!(groups[0].projects[0].name, "newer-proj");
        assert_eq!(groups[0].projects[1].name, "older-proj");
    }
}
