use anyhow::Result;
use applesauce::FileCompressor;
use applesauce::compressor::Kind;
use applesauce::progress::{Progress, SkipReason, Task};
use std::iter;
use std::os::unix::fs::MetadataExt;
use std::path::Path;

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

pub fn decompress_sync(path: &Path) -> Result<()> {
    let mut fc = FileCompressor::new();
    fc.recursive_decompress(iter::once(path), true, &SilentProgress, false);
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
}
