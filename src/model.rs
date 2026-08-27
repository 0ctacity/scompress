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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SortField {
    Date,
    Size,
    Name,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SortDirection {
    Asc,
    Desc,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SortConfig {
    pub field: SortField,
    pub direction: SortDirection,
}

impl SortConfig {
    pub const DEFAULT: Self = Self {
        field: SortField::Date,
        direction: SortDirection::Desc,
    };

    pub fn next(&self) -> Self {
        match (self.field, self.direction) {
            (SortField::Date, SortDirection::Desc) => Self {
                field: SortField::Date,
                direction: SortDirection::Asc,
            },
            (SortField::Date, SortDirection::Asc) => Self {
                field: SortField::Size,
                direction: SortDirection::Desc,
            },
            (SortField::Size, SortDirection::Desc) => Self {
                field: SortField::Size,
                direction: SortDirection::Asc,
            },
            (SortField::Size, SortDirection::Asc) => Self {
                field: SortField::Name,
                direction: SortDirection::Asc,
            },
            (SortField::Name, SortDirection::Asc) => Self {
                field: SortField::Name,
                direction: SortDirection::Desc,
            },
            (SortField::Name, SortDirection::Desc) => Self {
                field: SortField::Date,
                direction: SortDirection::Desc,
            },
        }
    }

    pub fn label(&self) -> &'static str {
        match (self.field, self.direction) {
            (SortField::Date, SortDirection::Desc) => "Date ↓ (newest first)",
            (SortField::Date, SortDirection::Asc) => "Date ↑ (oldest first)",
            (SortField::Size, SortDirection::Desc) => "Size ↓ (largest first)",
            (SortField::Size, SortDirection::Asc) => "Size ↑ (smallest first)",
            (SortField::Name, SortDirection::Asc) => "Name ↑ (A-Z)",
            (SortField::Name, SortDirection::Desc) => "Name ↓ (Z-A)",
        }
    }

    pub fn short_label(&self) -> &'static str {
        match (self.field, self.direction) {
            (SortField::Date, SortDirection::Desc) => "Date ↓",
            (SortField::Date, SortDirection::Asc) => "Date ↑",
            (SortField::Size, SortDirection::Desc) => "Size ↓",
            (SortField::Size, SortDirection::Asc) => "Size ↑",
            (SortField::Name, SortDirection::Asc) => "Name ↑",
            (SortField::Name, SortDirection::Desc) => "Name ↓",
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
        if let Some(ref t) = self.title
            && !t.trim().is_empty()
        {
            return t.trim();
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

    pub fn sort_sessions(&mut self, config: SortConfig) {
        match (config.field, config.direction) {
            (SortField::Date, SortDirection::Desc) => {
                self.sessions
                    .sort_by_key(|b| std::cmp::Reverse(b.modified_at));
            }
            (SortField::Date, SortDirection::Asc) => {
                self.sessions.sort_by_key(|a| a.modified_at);
            }
            (SortField::Size, SortDirection::Desc) => {
                self.sessions
                    .sort_by_key(|b| std::cmp::Reverse(b.logical_size));
            }
            (SortField::Size, SortDirection::Asc) => {
                self.sessions.sort_by_key(|a| a.logical_size);
            }
            (SortField::Name, SortDirection::Asc) => {
                self.sessions.sort_by_key(|a| a.label().to_lowercase());
            }
            (SortField::Name, SortDirection::Desc) => {
                self.sessions
                    .sort_by_key(|b| std::cmp::Reverse(b.label().to_lowercase()));
            }
        }
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

    pub fn sort_projects(&mut self, config: SortConfig) {
        match (config.field, config.direction) {
            (SortField::Date, SortDirection::Desc) => {
                self.projects
                    .sort_by_key(|b| std::cmp::Reverse(b.latest_modified()));
            }
            (SortField::Date, SortDirection::Asc) => {
                self.projects.sort_by_key(|a| a.latest_modified());
            }
            (SortField::Size, SortDirection::Desc) => {
                self.projects
                    .sort_by_key(|b| std::cmp::Reverse(b.logical_size()));
            }
            (SortField::Size, SortDirection::Asc) => {
                self.projects.sort_by_key(|a| a.logical_size());
            }
            (SortField::Name, SortDirection::Asc) => {
                self.projects.sort_by_key(|a| a.name.to_lowercase());
            }
            (SortField::Name, SortDirection::Desc) => {
                self.projects
                    .sort_by_key(|b| std::cmp::Reverse(b.name.to_lowercase()));
            }
        }
    }
}
