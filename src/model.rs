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
    pub display_title: String,
    pub path: PathBuf,
    pub logical_size: u64,
    pub physical_size: u64,
    pub compressed: bool,
    pub modified_at: SystemTime,
}
