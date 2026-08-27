use crate::applesauce::{compress, decompress};
use crate::model::Tool;
use crate::safety::{SkipReason, check_compression_safety, scan_open_files};
use crate::scanner::{
    build_tool_groups, claude_projects_dir, codex_sessions_dir, scan_all, scan_claude, scan_codex,
};
use anyhow::Result;
use clap::{Parser, Subcommand};
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
    let tool_groups = build_tool_groups(&sessions);

    println!(
        "{:<8} {:>8} {:>12} {:>12} {:>12}",
        "Tool", "Files", "Logical", "Disk", "Saved"
    );
    println!("{}", "-".repeat(56));

    for tg in &tool_groups {
        println!(
            "{:<8} {:>8} {:>12} {:>12} {:>12}",
            tg.tool.to_string(),
            tg.file_count(),
            format_size(tg.logical_size()),
            format_size(tg.physical_size()),
            format_size(tg.saved_size())
        );
    }

    println!();
    for tg in &tool_groups {
        println!("▼ {}", tg.tool);
        for pg in &tg.projects {
            println!(
                "  ▼ {} ({} sessions, {} → {}, Saved {})",
                pg.name,
                pg.sessions.len(),
                format_size(pg.logical_size()),
                format_size(pg.physical_size()),
                format_size(pg.saved_size())
            );

            for s in &pg.sessions {
                let time_str = format_relative_time(s.modified_at, now);
                let label = s.label();
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

                println!("      {:<40} {}", label, state_str);
            }
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
        let name = format!("{} / {} / {}", s.tool, s.project, s.label());
        match check_compression_safety(&s, &open_files, now) {
            Ok(()) => match compress(&s.path).await {
                Ok(()) => {
                    println!("✓ {}", name);
                    compressed_count += 1;
                }
                Err(err) => {
                    println!("✗ {}: {}", name, err);
                    failed_count += 1;
                }
            },
            Err(SkipReason::AlreadyCompressed) => {
                skipped_count += 1;
            }
            Err(reason) => {
                println!("✗ {}: {}", name, reason);
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
        let name = format!("{} / {} / {}", s.tool, s.project, s.label());
        if !s.compressed {
            skipped_count += 1;
            continue;
        }

        match decompress(&s.path).await {
            Ok(()) => {
                println!("✓ {}", name);
                decompressed_count += 1;
            }
            Err(err) => {
                println!("✗ {}: {}", name, err);
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
