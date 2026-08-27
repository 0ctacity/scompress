use clap::ValueEnum;
use std::path::PathBuf;
use std::time::SystemTime;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, ValueEnum)]
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
    pub path: PathBuf,
    pub logical_size: u64,
    pub physical_size: u64,
    pub compressed: bool,
    pub modified_at: SystemTime,
}

impl SessionFile {
    pub fn display_name(&self) -> String {
        self.path
            .file_name()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_else(|| self.path.to_string_lossy().to_string())
    }
}
