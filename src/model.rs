use clap::ValueEnum;
use std::path::PathBuf;
use std::time::SystemTime;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, ValueEnum)]
pub enum Tool {
    Codex,
    Claude,
}

impl std::fmt::Display for Tool {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Tool::Codex => write!(f, "Codex"),
            Tool::Claude => write!(f, "Claude"),
        }
    }
}

#[derive(Debug, Clone)]
pub struct SessionFile {
    pub tool: Tool,
    pub project: String,
    pub title: Option<String>,
    pub display_title: String,
    pub path: PathBuf,
    pub logical_size: u64,
    pub physical_size: u64,
    pub compressed: bool,
    pub modified_at: SystemTime,
}

impl SessionFile {
    pub fn label(&self) -> &str {
        if let Some(ref t) = self.title {
            if !t.trim().is_empty() {
                return t.trim();
            }
        }
        &self.display_title
    }
}

#[derive(Debug, Clone)]
pub struct ProjectGroup {
    pub name: String,
    pub sessions: Vec<SessionFile>,
}

impl ProjectGroup {
    pub fn logical_size(&self) -> u64 {
        self.sessions.iter().map(|s| s.logical_size).sum()
    }

    pub fn physical_size(&self) -> u64 {
        self.sessions.iter().map(|s| s.physical_size).sum()
    }

    pub fn saved_size(&self) -> u64 {
        self.logical_size().saturating_sub(self.physical_size())
    }

    pub fn latest_modified(&self) -> SystemTime {
        self.sessions
            .iter()
            .map(|s| s.modified_at)
            .max()
            .unwrap_or(SystemTime::UNIX_EPOCH)
    }
}

#[derive(Debug, Clone)]
pub struct ToolGroup {
    pub tool: Tool,
    pub projects: Vec<ProjectGroup>,
}

impl ToolGroup {
    pub fn file_count(&self) -> usize {
        self.projects.iter().map(|p| p.sessions.len()).sum()
    }

    pub fn logical_size(&self) -> u64 {
        self.projects.iter().map(|p| p.logical_size()).sum()
    }

    pub fn physical_size(&self) -> u64 {
        self.projects.iter().map(|p| p.physical_size()).sum()
    }

    pub fn saved_size(&self) -> u64 {
        self.logical_size().saturating_sub(self.physical_size())
    }
}
