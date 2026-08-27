use anyhow::Result;
use applesauce::FileCompressor;
use applesauce::compressor::Kind;
use applesauce::progress::{Progress, SkipReason, Task};
use std::iter;
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum OpType {
    Compressing,
    Decompressing,
}

impl std::fmt::Display for OpType {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            OpType::Compressing => write!(f, "compressing"),
            OpType::Decompressing => write!(f, "decompressing"),
        }
    }
}

#[derive(Clone, Debug)]
pub enum ProgressEvent {
    Started {
        path: PathBuf,
        op: OpType,
        total_bytes: u64,
    },
    Progress {
        path: PathBuf,
        op: OpType,
        bytes_done: u64,
        total_bytes: u64,
    },
    Completed {
        path: PathBuf,
        op: OpType,
        success: bool,
        error: Option<String>,
    },
}

impl ProgressEvent {
    pub fn error_message(&self) -> Option<&str> {
        match self {
            ProgressEvent::Completed { error, .. } => error.as_deref(),
            _ => None,
        }
    }
}

struct SilentProgress;

impl Task for SilentProgress {
    fn increment(&self, _amt: u64) {}
    fn error(&self, _message: &str) {}
}

impl Progress for SilentProgress {
    type Task = SilentProgress;

    fn error(&self, _path: &Path, _message: &str) {}
    fn file_skipped(&self, _path: &Path, _why: SkipReason) {}
    fn file_task(&self, _path: &Path, _size: u64) -> Self::Task {
        SilentProgress
    }
}

struct ChannelTask {
    path: PathBuf,
    op: OpType,
    total_bytes: u64,
    bytes_done: Arc<AtomicU64>,
    sender: smol::channel::Sender<ProgressEvent>,
}

impl Task for ChannelTask {
    fn increment(&self, amt: u64) {
        let prev = self.bytes_done.fetch_add(amt, Ordering::Relaxed);
        let current = prev + amt;
        let _ = self.sender.try_send(ProgressEvent::Progress {
            path: self.path.clone(),
            op: self.op,
            bytes_done: current,
            total_bytes: self.total_bytes,
        });
    }

    fn error(&self, message: &str) {
        let _ = self.sender.try_send(ProgressEvent::Completed {
            path: self.path.clone(),
            op: self.op,
            success: false,
            error: Some(message.to_string()),
        });
    }
}

struct ChannelProgress {
    op: OpType,
    sender: smol::channel::Sender<ProgressEvent>,
}

impl Progress for ChannelProgress {
    type Task = ChannelTask;

    fn error(&self, path: &Path, message: &str) {
        let _ = self.sender.try_send(ProgressEvent::Completed {
            path: path.to_path_buf(),
            op: self.op,
            success: false,
            error: Some(message.to_string()),
        });
    }

    fn file_skipped(&self, path: &Path, why: SkipReason) {
        let _ = self.sender.try_send(ProgressEvent::Completed {
            path: path.to_path_buf(),
            op: self.op,
            success: false,
            error: Some(format!("Skipped: {:?}", why)),
        });
    }

    fn file_task(&self, path: &Path, size: u64) -> Self::Task {
        let _ = self.sender.try_send(ProgressEvent::Started {
            path: path.to_path_buf(),
            op: self.op,
            total_bytes: size,
        });
        ChannelTask {
            path: path.to_path_buf(),
            op: self.op,
            total_bytes: size,
            bytes_done: Arc::new(AtomicU64::new(0)),
            sender: self.sender.clone(),
        }
    }
}

pub fn compress_sync(path: &Path) -> Result<()> {
    let mut fc = FileCompressor::new();
    fc.recursive_compress(
        iter::once(path),
        Kind::Lzfse,
        1.0,
        2,
        &SilentProgress,
        false,
    );
    Ok(())
}

pub fn compress_sync_with_progress(
    path: &Path,
    sender: smol::channel::Sender<ProgressEvent>,
) -> Result<()> {
    let progress = ChannelProgress {
        op: OpType::Compressing,
        sender: sender.clone(),
    };
    let mut fc = FileCompressor::new();
    fc.recursive_compress(iter::once(path), Kind::Lzfse, 1.0, 2, &progress, false);
    let _ = sender.try_send(ProgressEvent::Completed {
        path: path.to_path_buf(),
        op: OpType::Compressing,
        success: true,
        error: None,
    });
    Ok(())
}

pub fn decompress_sync(path: &Path) -> Result<()> {
    let mut fc = FileCompressor::new();
    fc.recursive_decompress(iter::once(path), true, &SilentProgress, false);
    Ok(())
}

pub fn decompress_sync_with_progress(
    path: &Path,
    sender: smol::channel::Sender<ProgressEvent>,
) -> Result<()> {
    let progress = ChannelProgress {
        op: OpType::Decompressing,
        sender: sender.clone(),
    };
    let mut fc = FileCompressor::new();
    fc.recursive_decompress(iter::once(path), true, &progress, false);
    let _ = sender.try_send(ProgressEvent::Completed {
        path: path.to_path_buf(),
        op: OpType::Decompressing,
        success: true,
        error: None,
    });
    Ok(())
}

pub async fn compress(path: &Path) -> Result<()> {
    let path = path.to_path_buf();
    smol::unblock(move || compress_sync(&path)).await
}

pub async fn decompress(path: &Path) -> Result<()> {
    let path = path.to_path_buf();
    smol::unblock(move || decompress_sync(&path)).await
}

/// Inspect a file for compression status, logical size, and physical on-disk size.
pub fn inspect_file(path: &Path) -> (bool, u64, u64) {
    if let Ok(info) = applesauce::info::get(path) {
        (info.is_compressed, info.stat_size, info.on_disk_size)
    } else if let Ok(meta) = std::fs::metadata(path) {
        (false, meta.len(), meta.blocks() * 512)
    } else {
        (false, 0, 0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    #[test]
    fn test_compress_and_decompress_file() {
        let temp_dir =
            std::env::temp_dir().join(format!("scompress_apfs_test_{}", std::process::id()));
        let _ = std::fs::create_dir_all(&temp_dir);
        let test_file = temp_dir.join("session_payload.json");

        // Write compressible data (repeating JSON text)
        let sample_data =
            r#"{"role":"user","content":"Hello world! Repeat repeat repeat repeat repeat repeat"}"#;
        let mut f = std::fs::File::create(&test_file).unwrap();
        for _ in 0..1000 {
            f.write_all(sample_data.as_bytes()).unwrap();
            f.write_all(b"\n").unwrap();
        }
        f.flush().unwrap();
        drop(f);

        let initial_content = std::fs::read(&test_file).unwrap();

        // Compress
        compress_sync(&test_file).unwrap();

        // Check inspection
        let (_is_comp, logical_size, _physical_size) = inspect_file(&test_file);
        assert_eq!(logical_size as usize, initial_content.len());
        // Transparent read should yield identical content
        let read_back = std::fs::read(&test_file).unwrap();
        assert_eq!(read_back, initial_content);

        // Decompress
        decompress_sync(&test_file).unwrap();

        let read_after_decomp = std::fs::read(&test_file).unwrap();
        assert_eq!(read_after_decomp, initial_content);

        let _ = std::fs::remove_dir_all(&temp_dir);
    }

    #[test]
    fn test_progress_channel_reporting() {
        let (tx, rx) = smol::channel::unbounded();
        let temp_dir =
            std::env::temp_dir().join(format!("scompress_prog_test_{}", std::process::id()));
        let _ = std::fs::create_dir_all(&temp_dir);
        let test_file = temp_dir.join("prog_session.json");

        let sample_data = "progress testing payload content repetition\n";
        let mut f = std::fs::File::create(&test_file).unwrap();
        for _ in 0..500 {
            f.write_all(sample_data.as_bytes()).unwrap();
        }
        f.flush().unwrap();
        drop(f);

        compress_sync_with_progress(&test_file, tx.clone()).unwrap();

        let mut events = Vec::new();
        while let Ok(ev) = rx.try_recv() {
            let _ = ev.error_message();
            events.push(ev);
        }

        assert!(!events.is_empty());
        let _ = std::fs::remove_dir_all(&temp_dir);
    }
}
