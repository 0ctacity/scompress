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
struct IndexEntry<'a> {
    id: Option<&'a str>,
    thread_name: Option<&'a str>,
    #[serde(default)]
    updated_at: Option<&'a str>,
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
            if let Ok(val) = serde_json::from_str::<IndexEntry>(&line) {
                let updated_at = val.updated_at.unwrap_or("");

                if let (Some(uuid), Some(name)) = (val.id, val.thread_name)
                    && !name.trim().is_empty()
                {
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
    if stem.len() >= 36 {
        let candidate = &stem[stem.len() - 36..];
        let bytes = candidate.as_bytes();
        if bytes[8] == b'-'
            && bytes[13] == b'-'
            && bytes[18] == b'-'
            && bytes[23] == b'-'
            && bytes.iter().enumerate().all(|(i, &b)| {
                if i == 8 || i == 13 || i == 18 || i == 23 {
                    b == b'-'
                } else {
                    b.is_ascii_hexdigit()
                }
            })
        {
            return Some(candidate.to_string());
        }
    }
    None
}

/// Parse absolute date/time fallback from a Codex rollout filename:
/// e.g. "rollout-2026-02-27T10-47-59-019542a1-cf0b-7412-a7e8-3841aee50b69.jsonl"
/// -> "2026-02-27 10:47:59 (019542a1)"
pub fn parse_codex_title(filename: &str) -> String {
    let stem = filename.strip_suffix(".jsonl").unwrap_or(filename);
    if let Some(stripped) = stem.strip_prefix("rollout-")
        && let Some((date_part, time_and_id)) = stripped.split_once('T')
    {
        let date = date_part;
        let mut time_parts = time_and_id.split('-');
        if let (Some(h), Some(m), Some(s)) =
            (time_parts.next(), time_parts.next(), time_parts.next())
        {
            let time = format!("{}:{}:{}", h, m, s);
            if let Some(id_part) = time_parts.next()
                && !id_part.is_empty()
            {
                let short_id = &id_part[0..id_part.len().min(8)];
                return format!("{} {} ({})", date, time, short_id);
            }
            return format!("{} {}", date, time);
        }
        return format!("{} {}", date, time_and_id);
    }
    stem.to_string()
}

#[derive(Deserialize)]
struct CodexPayload {
    cwd: Option<String>,
}

#[derive(Deserialize)]
struct CodexFirstLine {
    cwd: Option<String>,
    payload: Option<CodexPayload>,
}

#[derive(Deserialize)]
struct ClaudeFirstLine {
    cwd: Option<String>,
    title: Option<String>,
}

/// Parse metadata for Codex rollout files.
/// Extract project name from the first JSON line (`cwd` or `payload.cwd`),
/// and resolve the thread name from the preloaded `session_index_map`.
pub fn parse_codex_metadata(
    path: &Path,
    session_index_map: &HashMap<String, String>,
) -> (String, Option<String>, String) {
    let file_name = path
        .file_name()
        .and_then(|s| s.to_str())
        .unwrap_or("unknown");

    let display_title = parse_codex_title(file_name);

    let mut project = "unknown".to_string();
    let mut title: Option<String> = None;

    // Match thread_name from session_index lookup
    if let Some(uuid) = extract_codex_uuid(file_name)
        && let Some(indexed_title) = session_index_map.get(&uuid)
    {
        title = Some(indexed_title.clone());
    }

    // Only read the first line for project/cwd - do NOT parse the entire transcript
    if let Ok(file) = File::open(path) {
        let mut reader = BufReader::new(file);
        let mut first_line = String::new();
        if reader.read_line(&mut first_line).is_ok()
            && let Ok(val) = serde_json::from_str::<CodexFirstLine>(&first_line)
        {
            let cwd_val = val.payload.and_then(|p| p.cwd).or(val.cwd);

            if let Some(cwd) = cwd_val {
                let p = Path::new(&cwd);
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
        && (dir_name.starts_with('-') || dir_name.contains('_'))
    {
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
            && let Ok(val) = serde_json::from_str::<ClaudeFirstLine>(&first_line)
        {
            if let Some(cwd) = val.cwd {
                let p = Path::new(&cwd);
                if let Some(name) = p.file_name().and_then(|n| n.to_str())
                    && !name.is_empty()
                {
                    project = name.to_string();
                }
            }
            if let Some(t) = val.title
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

pub fn scan_codex_dir_sync(
    dir: &Path,
    session_index_map: &HashMap<String, String>,
) -> Result<Vec<SessionFile>> {
    let mut sessions = Vec::new();
    if !dir.exists() {
        return Ok(sessions);
    }

    for entry in WalkDir::new(dir)
        .into_iter()
        .filter_map(std::result::Result::ok)
    {
        let path = entry.path();
        if path.is_file()
            && let Some(ext) = path.extension()
            && ext == "jsonl"
        {
            let (is_compressed, logical_size, physical_size) = inspect_file(path);
            let (project, title, display_title) = parse_codex_metadata(path, session_index_map);
            let modified_at = entry
                .metadata()
                .ok()
                .and_then(|m| m.modified().ok())
                .unwrap_or(SystemTime::UNIX_EPOCH);

            sessions.push(SessionFile {
                tool: Tool::Codex,
                project,
                title,
                display_title,
                path: path.to_path_buf(),
                logical_size,
                physical_size,
                compressed: is_compressed,
                modified_at,
            });
        }
    }
    Ok(sessions)
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

pub fn scan_claude_dir_sync(dir: &Path) -> Result<Vec<SessionFile>> {
    let mut sessions = Vec::new();
    if !dir.exists() {
        return Ok(sessions);
    }

    for entry in WalkDir::new(dir)
        .into_iter()
        .filter_map(std::result::Result::ok)
    {
        let path = entry.path();
        if path.is_file()
            && let Some(ext) = path.extension()
            && ext == "jsonl"
        {
            let (is_compressed, logical_size, physical_size) = inspect_file(path);
            let (project, title, display_title) = parse_claude_metadata(path);
            let modified_at = entry
                .metadata()
                .ok()
                .and_then(|m| m.modified().ok())
                .unwrap_or(SystemTime::UNIX_EPOCH);

            sessions.push(SessionFile {
                tool: Tool::Claude,
                project,
                title,
                display_title,
                path: path.to_path_buf(),
                logical_size,
                physical_size,
                compressed: is_compressed,
                modified_at,
            });
        }
    }
    Ok(sessions)
}

pub async fn scan_all() -> Result<Vec<SessionFile>> {
    let mut sessions = scan_codex().await?;
    sessions.extend(scan_claude().await?);
    Ok(sessions)
}

/// Group a flat list of `SessionFile`s into `ToolGroup` -> `ProjectGroup` -> `Vec<SessionFile>`.
pub fn build_tool_groups(sessions: &[SessionFile]) -> Vec<ToolGroup> {
    let mut map: BTreeMap<Tool, BTreeMap<String, Vec<SessionFile>>> = BTreeMap::new();

    for session in sessions {
        map.entry(session.tool)
            .or_default()
            .entry(session.project.clone())
            .or_default()
            .push(session.clone());
    }

    let mut groups = Vec::new();
    for (tool, projects_map) in map {
        let mut projects = Vec::new();
        for (name, mut s_list) in projects_map {
            s_list.sort_by_key(|s| std::cmp::Reverse(s.modified_at));
            projects.push(ProjectGroup {
                name,
                sessions: s_list,
            });
        }
        projects.sort_by(|a, b| {
            let a_max = a.sessions.iter().map(|s| s.modified_at).max();
            let b_max = b.sessions.iter().map(|s| s.modified_at).max();
            b_max.cmp(&a_max)
        });

        groups.push(ToolGroup { tool, projects });
    }

    groups.sort_by(|a, b| {
        let a_max = a
            .projects
            .iter()
            .flat_map(|p| p.sessions.iter().map(|s| s.modified_at))
            .max();
        let b_max = b
            .projects
            .iter()
            .flat_map(|p| p.sessions.iter().map(|s| s.modified_at))
            .max();
        b_max.cmp(&a_max)
    });

    groups
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    #[test]
    fn test_parse_codex_title() {
        assert_eq!(
            parse_codex_title(
                "rollout-2026-02-27T10-47-59-019542a1-cf0b-7412-a7e8-3841aee50b69.jsonl"
            ),
            "2026-02-27 10:47:59 (019542a1)"
        );
        assert_eq!(
            parse_codex_title("rollout-2026-02-27T10-47-59.jsonl"),
            "2026-02-27 10:47:59"
        );
        assert_eq!(parse_codex_title("custom-session.jsonl"), "custom-session");
    }

    #[test]
    fn test_extract_codex_uuid() {
        assert_eq!(
            extract_codex_uuid(
                "rollout-2026-02-27T10-47-59-019542a1-cf0b-7412-a7e8-3841aee50b69.jsonl"
            ),
            Some("019542a1-cf0b-7412-a7e8-3841aee50b69".to_string())
        );
        assert_eq!(extract_codex_uuid("custom-session.jsonl"), None);
    }

    #[test]
    fn test_load_codex_session_index() {
        let temp_dir =
            std::env::temp_dir().join(format!("scompress_idx_test_{}", std::process::id()));
        let _ = std::fs::create_dir_all(&temp_dir);
        let index_file = temp_dir.join("session_index.jsonl");

        let mut f = File::create(&index_file).unwrap();
        writeln!(
            f,
            r#"{{"id":"uuid-1234-5678-90ab-cdef12345678","thread_name":"First Task","updated_at":"2026-01-01T00:00:00Z"}}"#
        )
        .unwrap();
        writeln!(
            f,
            r#"{{"id":"uuid-1234-5678-90ab-cdef12345678","thread_name":"Updated Task","updated_at":"2026-01-02T00:00:00Z"}}"#
        )
        .unwrap();
        writeln!(
            f,
            r#"{{"id":"uuid-9999-9999-9999-999999999999","thread_name":"","updated_at":"2026-01-01T00:00:00Z"}}"#
        )
        .unwrap();
        drop(f);

        let map = load_codex_session_index(&index_file);
        assert_eq!(
            map.get("uuid-1234-5678-90ab-cdef12345678"),
            Some(&"Updated Task".to_string())
        );
        assert_eq!(map.get("uuid-9999-9999-9999-999999999999"), None);

        let _ = std::fs::remove_dir_all(&temp_dir);
    }

    #[test]
    fn test_parse_codex_metadata_with_index() {
        let temp_dir =
            std::env::temp_dir().join(format!("scompress_meta_test_{}", std::process::id()));
        let _ = std::fs::create_dir_all(&temp_dir);
        let file_path =
            temp_dir.join("rollout-2026-02-27T10-47-59-019542a1-cf0b-7412-a7e8-3841aee50b69.jsonl");

        let mut f = File::create(&file_path).unwrap();
        writeln!(
            f,
            r#"{{"type":"session_start","payload":{{"cwd":"/Users/test/projects/my-app"}}}}"#
        )
        .unwrap();
        drop(f);

        let mut index_map = HashMap::new();
        index_map.insert(
            "019542a1-cf0b-7412-a7e8-3841aee50b69".to_string(),
            "Refactor auth logic".to_string(),
        );

        let (project, title, display_title) = parse_codex_metadata(&file_path, &index_map);
        assert_eq!(project, "my-app");
        assert_eq!(title, Some("Refactor auth logic".to_string()));
        assert_eq!(display_title, "2026-02-27 10:47:59 (019542a1)");

        let _ = std::fs::remove_dir_all(&temp_dir);
    }

    #[test]
    fn test_build_tool_groups_sorting() {
        use std::time::Duration;
        let base_time = SystemTime::UNIX_EPOCH + Duration::from_secs(1_000_000);

        let sessions = vec![
            SessionFile {
                tool: Tool::Claude,
                project: "proj-old".to_string(),
                title: None,
                display_title: "old".to_string(),
                path: PathBuf::from("/tmp/1.jsonl"),
                logical_size: 100,
                physical_size: 100,
                compressed: false,
                modified_at: base_time + Duration::from_secs(100),
            },
            SessionFile {
                tool: Tool::Codex,
                project: "proj-new".to_string(),
                title: None,
                display_title: "new".to_string(),
                path: PathBuf::from("/tmp/2.jsonl"),
                logical_size: 200,
                physical_size: 200,
                compressed: false,
                modified_at: base_time + Duration::from_secs(500),
            },
        ];

        let groups = build_tool_groups(&sessions);
        assert_eq!(groups.len(), 2);
        assert_eq!(groups[0].tool, Tool::Codex); // Newer modified session comes first
        assert_eq!(groups[1].tool, Tool::Claude);
    }
}
