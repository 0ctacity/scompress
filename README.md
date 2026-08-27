# scompress

`scompress` is a lightweight macOS utility that transparently compresses **Codex** and **Claude Code** session files using Apple File System Compression (APFS / `decmpfs`) with LZFSE.

Compressed session files remain fully readable by Codex and Claude without patches, daemon processes, or session interception. If either tool writes to a compressed session, macOS automatically materializes it back to an uncompressed file; running `scompress c` later compresses it again.

---

## Features

- **Transparent Filesystem Compression**: Uses native macOS kernel-level `decmpfs` compression (LZFSE algorithm) powered by [`applesauce`](https://crates.io/crates/applesauce).
- **Interactive TUI**: Built with [Ratatui](https://ratatui.rs) and [smol](https://github.com/smol-rs/smol) for browsing session storage savings in real time.
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
┌─ scompress ─────────────────────────────────────────┐
│ Tool      Files     Logical      Disk      Saved   │
│ Codex       142      3.8 GB     240 MB     3.6 GB │
│ Claude       61      1.4 GB      96 MB     1.3 GB │
├────────────────────────────────────────────────────┤
│ > Codex   rollout-2026-07-06...  ● normal   35.5 MB  │
│   Claude  project-xyz.jsonl      ◉ compressed 91 MB → 4 MB │
│                                                    │
│ c Compress   d Decompress   r Refresh   q Quit     │
└────────────────────────────────────────────────────┘
```

#### Keybindings

| Key | Action |
| --- | --- |
| `c` | Compress all eligible session files |
| `d` | Decompress all compressed session files |
| `r` | Rescan / refresh session list |
| `↑` / `k`, `↓` / `j` | Browse sessions list |
| `q` / `Esc` | Quit |

---

### Command Line Interface

#### 1. List Sessions

List discovered session files, physical disk usage, and compression states:

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
├── tui.rs         # Ratatui TUI rendering and event loop
├── scanner.rs     # Codex & Claude session discovery
├── applesauce.rs  # applesauce crate integration for LZFSE compression
├── safety.rs      # lsof process inspection & safety rules
└── model.rs       # Tool and SessionFile data models
```

---

## License

MIT OR Apache-2.0
