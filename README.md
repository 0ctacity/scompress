# scompress

> Compress your long agentic sessions

Transparent, zero-overhead APFS compression tool for **Codex** and **Claude Code** session transcripts.

[![CI](https://github.com/0ctacity/scompress/actions/workflows/ci.yml/badge.svg)](https://github.com/0ctacity/scompress/actions)
[![License: MIT](https://img.shields.io/badge/License-MIT-blue.svg)](LICENSE)

---

## Overview

AI coding agents like **Codex** (`~/.codex/sessions`) and **Claude Code** (`~/.claude/projects`) generate detailed JSONL rollout and transcript histories. Over time, these session logs accumulate into gigabytes of disk space.

`scompress` applies transparent filesystem compression (LZFSE / APFS decmpfs) directly to your agent session files on macOS APFS:
- **Transparent Reads**: Compressed session files remain fully readable by tools, editors, and background processes without manual decompression.
- **Safety First**: Automatically detects open files (`lsof`) and recently modified active sessions (< 30s) to prevent race conditions.
- **Interactive TUI**: Real-time hierarchical browser with multi-session selection, customizable sorting, live progress bars, and batch operations.

---

## Features

- **Decmpfs APFS Transparent Compression**: High-efficiency LZFSE compression with zero runtime read overhead.
- **Hierarchical Project & Session Tree**: Grouped by Tool (Codex / Claude) $\to$ Project $\to$ Thread / Session Title.
- **Multi-Selection Mode**: Select individual sessions (`s`), select ranges (`Shift + S`), or whole project/tool trees for batch compression/decompression.
- **Flexible Ordering / Sorting (`o`)**:
  - Ascending / Descending by **Last Modified / Open Date**
  - Ascending / Descending by **Size**
  - Ascending / Descending by **Project / Thread Name**
  - Hierarchical override: Tool-level sort orders apply across all projects and threads under that tool.
- **Live Per-Row Progress Bars**: Incremental byte-by-byte visual progress indicator with active status for compression and decompression.
- **Process & Write Safety**: Inspects open file descriptors via `lsof` and skips actively running sessions.

---

## Usage

### Interactive TUI

Launch the full interactive terminal UI:

```bash
scompress
```

```text
┌── scompress ─────────────────────────────────────────────┐
│ Tool       Files       Logical         Disk        Saved │
├──────────────────────────────────────────────────────────┤
│ Codex       229      6.4 GB    2.1 GB     4.3 GB         │
│ Claude       61      1.4 GB     96 MB     1.3 GB         │
├──────────────────────────────────────────────────────────┤
│ ▼ Codex [Date ↓] (229 files, 6.4 GB → 2.1 GB, Saved 4.3 GB)
│   ▼ scompress (12 sessions, 450 MB → 30 MB)              │
│       [✓] Fix session tree rendering  ◉ compressed   45 MB → 3 MB   2 days ago │
│       [✓] Add Applesauce backend      ◉ compressed   32 MB → 2 MB   3 days ago │
│   ▶ rchat (35 sessions, 850 MB → 210 MB)                 │
│                                                          │
│ s Select  S Range  o Sort  Space Toggle  c Compress      │
└──────────────────────────────────────────────────────────┘
```

#### TUI Keybindings

| Key | Action |
| --- | --- |
| `s` | Toggle selection for highlighted node (Session / Project / Tool) |
| `S` (`Shift + S`) | Select range from anchor to cursor |
| `o` / `O` | Cycle sort order (Date ↑/↓, Size ↑/↓, Name ↑/↓). Tool-level overrides project-level. |
| `Esc` / `x` | Clear selection |
| `Space` / `Enter` | Expand / collapse highlighted project or tool node |
| `e` / `Tab` | Expand / collapse all nodes |
| `c` | Compress selection (or highlighted node if no selection) |
| `d` | Decompress selection (or highlighted node if no selection) |
| `r` | Rescan / refresh session list |
| `↑` / `k`, `↓` / `j` | Navigate up / down |
| `q` | Quit |

---

### Command Line Interface

#### 1. List Sessions

List discovered session files grouped by project with compression states:

```bash
# List all sessions
scompress list

# Filter by tool
scompress list codex
scompress list claude
```

#### 2. Compress Sessions

Compress all eligible Codex and Claude Code session files:

```bash
# Compress all eligible sessions
scompress compress

# Compress only Codex sessions
scompress compress codex

# Compress only Claude sessions
scompress compress claude
```

#### 3. Decompress Sessions

Restore sessions to standard uncompressed files:

```bash
# Decompress all
scompress decompress

# Filter by tool
scompress decompress codex
scompress decompress claude
```

---

## Safety Guarantees

1. **Active Session Detection**: Scans open file descriptors via `lsof`. If Codex or Claude is currently writing to a transcript file, it is automatically skipped.
2. **Freshness Guard**: Files modified within the last 30 seconds are skipped to avoid races with recently terminated agents.
3. **Symlink Protection**: Symlinks are never followed or modified.
4. **Idempotency**: Already compressed files are detected and untouched.

---

## Testing

Run the test suite using `nextest`:

```bash
cargo nextest run
```

---

## License

MIT License. See [LICENSE](LICENSE) for details.
