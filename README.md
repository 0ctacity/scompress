# scompress

`scompress` is a lightweight macOS utility that transparently compresses **Codex** and **Claude Code** session files using Apple File System Compression (APFS / `decmpfs`) with LZFSE.

Compressed session files remain fully readable by Codex and Claude without patches, daemon processes, or session interception. If either tool writes to a compressed session, macOS automatically materializes it back to an uncompressed file; running `scompress c` later compresses it again.

---

## Features

- **Transparent Filesystem Compression**: Uses native macOS kernel-level `decmpfs` compression (LZFSE algorithm) powered by [`applesauce`](https://crates.io/crates/applesauce).
- **Hierarchical Project & Session Tree**: Groups sessions by `Tool → Project → Thread` with real thread names indexed from `~/.codex/session_index.jsonl`.
- **Interactive TUI with Selection Mode**: Built with [Ratatui](https://ratatui.rs) and [smol](https://github.com/smol-rs/smol).
  - Select individual nodes with `s` or ranges with `Shift + S`.
  - Batch compress or decompress selected items with `c` and `d`.
- **Safety First**:
  - Automatically skips actively open files via batch `lsof` inspection.
  - Skips symlinks and non-regular files.
  - Skips files modified very recently (< 30 seconds ago).
  - Skips already compressed files.
  - Non-destructive: no files are moved, renamed, or modified in content.
- **Supported Coding Assistants**:
  - **Codex**: `~/.codex/sessions/**`
  - **Claude Code**: `~/.claude/projects/**/*.jsonl`

---

## Installation

### Prerequisites

- macOS (APFS or HFS+ with compression support)
- Rust toolchain (2024 edition / stable)

### Build from Source

```bash
git clone https://github.com/your-username/scompress.git
cd scompress
cargo build --release
```

The binary will be available at `target/release/scompress`.

---

## Usage

### Interactive TUI

Launch the interactive terminal interface:

```bash
scompress
```

```text
┌─ scompress ──────────────────────────────────────────┐
│ Tool      Files     Logical      Disk      Saved     │
│ Codex       229      6.4 GB    2.1 GB     4.3 GB     │
│ Claude       61      1.4 GB     96 MB     1.3 GB     │
├──────────────────────────────────────────────────────┤
│ ▼ Codex (229 files, 6.4 GB → 2.1 GB, Saved 4.3 GB)   │
│   ▼ scompress (12 sessions, 450 MB → 30 MB)          │
│       [✓] Fix session tree rendering  ◉ compressed   │
│       [✓] Add Applesauce backend      ◉ compressed   │
│   ▶ rchat (35 sessions, 850 MB → 210 MB)             │
│                                                      │
│ s Select  S Range  Space Toggle  c Compress  q Quit  │
└──────────────────────────────────────────────────────┘
```

#### TUI Keybindings

| Key | Action |
| --- | --- |
| `s` | Toggle selection for highlighted node (Session / Project / Tool) |
| `S` (`Shift + S`) | Select range from anchor to cursor |
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
# Compress all
scompress compress
# or shorthand:
scompress c

# Filter by tool
scompress c codex
scompress c claude
```

#### 3. Decompress Sessions

Restore compressed files back to their uncompressed on-disk state:

```bash
# Decompress all
scompress decompress
# or shorthand:
scompress dc

# Filter by tool
scompress dc codex
scompress dc claude
```

---

## Development & Testing

Run unit and integration tests using [`cargo-nextest`](https://nexte.st/):

```bash
cargo nextest run
```

---

## Architecture

```text
src/
├── main.rs        # CLI routing & async entrypoint
├── cli.rs         # Clap command definitions & CLI handlers
├── tui.rs         # Ratatui TUI rendering, selection mode & tree event loop
├── scanner.rs     # Codex & Claude session discovery & thread metadata
├── applesauce.rs  # applesauce crate integration for LZFSE compression
├── safety.rs      # lsof process inspection & safety rules
└── model.rs       # Tool, ProjectGroup, ToolGroup & SessionFile models
```

---

## License

MIT OR Apache-2.0
