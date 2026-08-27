use crate::applesauce::{compress, decompress};
use crate::model::{SessionFile, Tool};
use crate::safety::{SkipReason, check_compression_safety, scan_open_files};
use crate::scanner::{claude_projects_dir, codex_sessions_dir, scan_all, scan_claude, scan_codex};
use anyhow::Result;
use clap::{Parser, Subcommand};
use std::collections::BTreeMap;
use std::time::SystemTime;

#[derive(Parser, Debug)]
#[command(
    name = "scompress",
    version,
    about = "Transparent APFS session compression for Codex and Claude Code"
)]
pub struct Cli {
    #[command(subcommand)]
    pub command: Option<Commands>,
}

#[derive(Subcommand, Debug)]
pub enum Commands {
    /// List discovered session files and compression state
    List {
        #[arg(value_enum)]
        tool: Option<Tool>,
    },
    /// Compress eligible session files
    #[command(alias = "c")]
    Compress {
        #[arg(value_enum)]
        tool: Option<Tool>,
    },
    /// Decompress compressed session files
    #[command(alias = "dc")]
    Decompress {
        #[arg(value_enum)]
        tool: Option<Tool>,
    },
}

pub fn format_size(bytes: u64) -> String {
    const KB: u64 = 1024;
    const MB: u64 = 1024 * KB;
    const GB: u64 = 1024 * MB;

    if bytes >= GB {
        format!("{:.1} GB", bytes as f64 / GB as f64)
    } else if bytes >= MB {
        format!("{:.1} MB", bytes as f64 / MB as f64)
    } else if bytes >= KB {
        format!("{:.1} KB", bytes as f64 / KB as f64)
    } else {
        format!("{} B", bytes)
    }
}

pub fn format_relative_time(modified_at: SystemTime, now: SystemTime) -> String {
    let elapsed = match now.duration_since(modified_at) {
        Ok(d) => d,
        Err(_) => return "just now".to_string(),
    };

    let secs = elapsed.as_secs();
    if secs < 60 {
        format!("{}s ago", secs)
    } else if secs < 3600 {
        let mins = secs / 60;
        if mins == 1 {
            "1 min ago".to_string()
        } else {
            format!("{} mins ago", mins)
        }
    } else if secs < 86400 {
        let hours = secs / 3600;
        if hours == 1 {
            "1 hr ago".to_string()
        } else {
            format!("{} hrs ago", hours)
        }
    } else {
        let days = secs / 86400;
        if days == 1 {
            "1 day ago".to_string()
        } else {
            format!("{} days ago", days)
        }
    }
}

pub async fn run_list(tool_filter: Option<Tool>) -> Result<()> {
    let sessions = match tool_filter {
        Some(Tool::Codex) => scan_codex().await?,
        Some(Tool::Claude) => scan_claude().await?,
        None => scan_all().await?,
    };

    if sessions.is_empty() {
        println!("No session files found.");
        return Ok(());
    }

    let now = SystemTime::now();

    // Summary stats
    let mut codex_count = 0;
    let mut codex_logical = 0;
    let mut codex_physical = 0;

    let mut claude_count = 0;
    let mut claude_logical = 0;
    let mut claude_physical = 0;

    for s in &sessions {
        match s.tool {
            Tool::Codex => {
                codex_count += 1;
                codex_logical += s.logical_size;
                codex_physical += s.physical_size;
            }
            Tool::Claude => {
                claude_count += 1;
                claude_logical += s.logical_size;
                claude_physical += s.physical_size;
            }
        }
    }

    println!(
        "{:<8} {:>8} {:>12} {:>12} {:>12}",
        "Tool", "Files", "Logical", "Disk", "Saved"
    );
    println!("{}", "-".repeat(56));

    if codex_count > 0 || tool_filter == Some(Tool::Codex) {
        let saved = codex_logical.saturating_sub(codex_physical);
        println!(
            "{:<8} {:>8} {:>12} {:>12} {:>12}",
            "Codex",
            codex_count,
            format_size(codex_logical),
            format_size(codex_physical),
            format_size(saved)
        );
    }
    if claude_count > 0 || tool_filter == Some(Tool::Claude) {
        let saved = claude_logical.saturating_sub(claude_physical);
        println!(
            "{:<8} {:>8} {:>12} {:>12} {:>12}",
            "Claude",
            claude_count,
            format_size(claude_logical),
            format_size(claude_physical),
            format_size(saved)
        );
    }

    // Group sessions by tool & project
    let mut groups: BTreeMap<(String, String), Vec<&SessionFile>> = BTreeMap::new();
    for s in &sessions {
        groups
            .entry((s.tool.to_string(), s.project.clone()))
            .or_default()
            .push(s);
    }

    println!();
    for ((tool_name, project_name), group_sessions) in groups {
        let group_logical: u64 = group_sessions.iter().map(|s| s.logical_size).sum();
        let group_physical: u64 = group_sessions.iter().map(|s| s.physical_size).sum();
        let group_saved = group_logical.saturating_sub(group_physical);

        println!(
            "▼ {} / {} ({} sessions, {} → {}, Saved {})",
            tool_name,
            project_name,
            group_sessions.len(),
            format_size(group_logical),
            format_size(group_physical),
            format_size(group_saved)
        );

        for s in group_sessions {
            let time_str = format_relative_time(s.modified_at, now);
            let name = &s.display_title;
            let state_str = if s.compressed {
                format!(
                    "◉ compressed {:>8} → {:<8}",
                    format_size(s.logical_size),
                    format_size(s.physical_size)
                )
            } else {
                format!(
                    "● normal     {:>8}   {:<12}",
                    format_size(s.logical_size),
                    time_str
                )
            };

            println!("    {:<35} {}", name, state_str);
        }
        println!();
    }

    Ok(())
}

pub async fn run_compress(tool_filter: Option<Tool>) -> Result<()> {
    let sessions = match tool_filter {
        Some(Tool::Codex) => scan_codex().await?,
        Some(Tool::Claude) => scan_claude().await?,
        None => scan_all().await?,
    };

    let mut roots = Vec::new();
    if let Some(d) = codex_sessions_dir() {
        roots.push(d);
    }
    if let Some(d) = claude_projects_dir() {
        roots.push(d);
    }

    let open_files = smol::unblock(move || scan_open_files(&roots)).await;
    let now = SystemTime::now();

    let mut compressed_count = 0;
    let mut skipped_count = 0;
    let mut failed_count = 0;

    for s in sessions {
        let name = format!("{} / {}", s.project, s.display_title);
        match check_compression_safety(&s, &open_files, now) {
            Ok(()) => match compress(&s.path).await {
                Ok(()) => {
                    println!("✓ {} session {}", s.tool, name);
                    compressed_count += 1;
                }
                Err(err) => {
                    println!("✗ {} session {}: {}", s.tool, name, err);
                    failed_count += 1;
                }
            },
            Err(SkipReason::AlreadyCompressed) => {
                skipped_count += 1;
            }
            Err(reason) => {
                println!("✗ {} session {}: {}", s.tool, name, reason);
                skipped_count += 1;
            }
        }
    }

    println!();
    println!("Compressed {}", compressed_count);
    println!("Skipped {}", skipped_count);
    println!("Failed {}", failed_count);

    Ok(())
}

pub async fn run_decompress(tool_filter: Option<Tool>) -> Result<()> {
    let sessions = match tool_filter {
        Some(Tool::Codex) => scan_codex().await?,
        Some(Tool::Claude) => scan_claude().await?,
        None => scan_all().await?,
    };

    let mut decompressed_count = 0;
    let mut skipped_count = 0;
    let mut failed_count = 0;

    for s in sessions {
        let name = format!("{} / {}", s.project, s.display_title);
        if !s.compressed {
            skipped_count += 1;
            continue;
        }

        match decompress(&s.path).await {
            Ok(()) => {
                println!("✓ {} session {}", s.tool, name);
                decompressed_count += 1;
            }
            Err(err) => {
                println!("✗ {} session {}: {}", s.tool, name, err);
                failed_count += 1;
            }
        }
    }

    println!();
    println!("Decompressed {}", decompressed_count);
    println!("Skipped {}", skipped_count);
    println!("Failed {}", failed_count);

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[test]
    fn test_format_size() {
        assert_eq!(format_size(500), "500 B");
        assert_eq!(format_size(1024), "1.0 KB");
        assert_eq!(format_size(1536), "1.5 KB");
        assert_eq!(format_size(1024 * 1024 * 5), "5.0 MB");
        assert_eq!(format_size(1024 * 1024 * 1024 * 3), "3.0 GB");
    }

    #[test]
    fn test_format_relative_time() {
        let now = SystemTime::now();
        assert_eq!(
            format_relative_time(now - Duration::from_secs(10), now),
            "10s ago"
        );
        assert_eq!(
            format_relative_time(now - Duration::from_secs(60), now),
            "1 min ago"
        );
        assert_eq!(
            format_relative_time(now - Duration::from_secs(240), now),
            "4 mins ago"
        );
        assert_eq!(
            format_relative_time(now - Duration::from_secs(3600), now),
            "1 hr ago"
        );
        assert_eq!(
            format_relative_time(now - Duration::from_secs(7200), now),
            "2 hrs ago"
        );
        assert_eq!(
            format_relative_time(now - Duration::from_secs(86400), now),
            "1 day ago"
        );
    }
}
