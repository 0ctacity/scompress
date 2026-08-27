use crate::applesauce::{compress, decompress};
use crate::cli::{format_relative_time, format_size};
use crate::model::{SessionFile, Tool};
use crate::safety::{SkipReason, check_compression_safety, scan_open_files};
use crate::scanner::{claude_projects_dir, codex_sessions_dir, scan_all};
use anyhow::Result;
use crossterm::{
    event::{self, Event, KeyCode, KeyModifiers},
    execute,
    terminal::{EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode},
};
use ratatui::{
    Frame, Terminal,
    backend::CrosstermBackend,
    layout::{Constraint, Direction, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Paragraph, Row, Table},
};
use std::collections::{BTreeMap, HashSet};
use std::io::{Stdout, stdout};
use std::time::{Duration, Instant, SystemTime};

#[derive(Clone, Debug)]
pub enum TreeItem {
    ProjectHeader {
        tool: Tool,
        project: String,
        count: usize,
        logical_size: u64,
        physical_size: u64,
        is_expanded: bool,
    },
    SessionItem {
        session_index: usize,
    },
}

pub struct App {
    pub sessions: Vec<SessionFile>,
    pub collapsed_projects: HashSet<(Tool, String)>,
    pub visible_items: Vec<TreeItem>,
    pub selected_index: usize,
    pub status_message: String,
    pub is_busy: bool,
}

impl App {
    pub fn new() -> Self {
        Self {
            sessions: Vec::new(),
            collapsed_projects: HashSet::new(),
            visible_items: Vec::new(),
            selected_index: 0,
            status_message: "Press 'Space' to expand/collapse, 'c' to compress, 'd' to decompress, 'r' to refresh".to_string(),
            is_busy: false,
        }
    }

    pub fn rebuild_visible_items(&mut self) {
        let mut groups: BTreeMap<(Tool, String), Vec<usize>> = BTreeMap::new();
        for (idx, s) in self.sessions.iter().enumerate() {
            groups
                .entry((s.tool, s.project.clone()))
                .or_default()
                .push(idx);
        }

        let mut items = Vec::new();
        for ((tool, project), session_indices) in groups {
            let is_expanded = !self.collapsed_projects.contains(&(tool, project.clone()));
            let count = session_indices.len();
            let logical_size: u64 = session_indices
                .iter()
                .map(|&i| self.sessions[i].logical_size)
                .sum();
            let physical_size: u64 = session_indices
                .iter()
                .map(|&i| self.sessions[i].physical_size)
                .sum();

            items.push(TreeItem::ProjectHeader {
                tool,
                project: project.clone(),
                count,
                logical_size,
                physical_size,
                is_expanded,
            });

            if is_expanded {
                for idx in session_indices {
                    items.push(TreeItem::SessionItem { session_index: idx });
                }
            }
        }

        self.visible_items = items;
        if self.selected_index >= self.visible_items.len() && !self.visible_items.is_empty() {
            self.selected_index = self.visible_items.len() - 1;
        }
    }

    pub async fn refresh(&mut self) -> Result<()> {
        self.is_busy = true;
        self.status_message = "Scanning session files...".to_string();
        match scan_all().await {
            Ok(sessions) => {
                self.sessions = sessions;
                self.rebuild_visible_items();
                self.status_message = format!("Discovered {} session files", self.sessions.len());
            }
            Err(e) => {
                self.status_message = format!("Scan error: {}", e);
            }
        }
        self.is_busy = false;
        Ok(())
    }

    pub fn toggle_current_expand(&mut self) {
        if self.visible_items.is_empty() {
            return;
        }

        match &self.visible_items[self.selected_index] {
            TreeItem::ProjectHeader { tool, project, .. } => {
                let key = (*tool, project.clone());
                if self.collapsed_projects.contains(&key) {
                    self.collapsed_projects.remove(&key);
                } else {
                    self.collapsed_projects.insert(key);
                }
                self.rebuild_visible_items();
            }
            TreeItem::SessionItem { session_index } => {
                let s = &self.sessions[*session_index];
                let key = (s.tool, s.project.clone());
                self.collapsed_projects.insert(key);
                self.rebuild_visible_items();
            }
        }
    }

    pub fn toggle_expand_all(&mut self) {
        if self.collapsed_projects.is_empty() {
            // Collapse all
            for s in &self.sessions {
                self.collapsed_projects.insert((s.tool, s.project.clone()));
            }
        } else {
            // Expand all
            self.collapsed_projects.clear();
        }
        self.rebuild_visible_items();
    }

    pub async fn compress_all(&mut self) -> Result<()> {
        if self.is_busy {
            return Ok(());
        }
        self.is_busy = true;

        let mut roots = Vec::new();
        if let Some(d) = codex_sessions_dir() {
            roots.push(d);
        }
        if let Some(d) = claude_projects_dir() {
            roots.push(d);
        }

        let open_files = smol::unblock(move || scan_open_files(&roots)).await;
        let now = SystemTime::now();

        let mut compressed = 0;
        let mut skipped = 0;
        let mut failed = 0;

        for s in &self.sessions {
            match check_compression_safety(s, &open_files, now) {
                Ok(()) => match compress(&s.path).await {
                    Ok(()) => compressed += 1,
                    Err(_) => failed += 1,
                },
                Err(SkipReason::AlreadyCompressed) => {
                    skipped += 1;
                }
                Err(_) => {
                    skipped += 1;
                }
            }
        }

        // Refresh after compressing
        if let Ok(sessions) = scan_all().await {
            self.sessions = sessions;
            self.rebuild_visible_items();
        }

        self.status_message = format!(
            "Compression complete: {} compressed, {} skipped, {} failed",
            compressed, skipped, failed
        );
        self.is_busy = false;
        Ok(())
    }

    pub async fn decompress_all(&mut self) -> Result<()> {
        if self.is_busy {
            return Ok(());
        }
        self.is_busy = true;

        let mut decompressed = 0;
        let mut skipped = 0;
        let mut failed = 0;

        for s in &self.sessions {
            if !s.compressed {
                skipped += 1;
                continue;
            }

            match decompress(&s.path).await {
                Ok(()) => decompressed += 1,
                Err(_) => failed += 1,
            }
        }

        // Refresh after decompressing
        if let Ok(sessions) = scan_all().await {
            self.sessions = sessions;
            self.rebuild_visible_items();
        }

        self.status_message = format!(
            "Decompression complete: {} decompressed, {} skipped, {} failed",
            decompressed, skipped, failed
        );
        self.is_busy = false;
        Ok(())
    }

    pub fn next(&mut self) {
        if !self.visible_items.is_empty() && self.selected_index + 1 < self.visible_items.len() {
            self.selected_index += 1;
        }
    }

    pub fn previous(&mut self) {
        if !self.visible_items.is_empty() && self.selected_index > 0 {
            self.selected_index -= 1;
        }
    }
}

pub async fn run_tui() -> Result<()> {
    enable_raw_mode()?;
    let mut stdout = stdout();
    execute!(stdout, EnterAlternateScreen)?;
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;

    let mut app = App::new();
    app.refresh().await?;

    let res = run_app_loop(&mut terminal, &mut app).await;

    disable_raw_mode()?;
    execute!(terminal.backend_mut(), LeaveAlternateScreen)?;
    terminal.show_cursor()?;

    res
}

async fn run_app_loop(
    terminal: &mut Terminal<CrosstermBackend<Stdout>>,
    app: &mut App,
) -> Result<()> {
    let tick_rate = Duration::from_millis(50);
    let mut last_tick = Instant::now();

    loop {
        terminal.draw(|f| ui(f, app))?;

        let timeout = tick_rate
            .checked_sub(last_tick.elapsed())
            .unwrap_or_else(|| Duration::from_secs(0));

        if event::poll(timeout)? {
            if let Event::Key(key) = event::read()? {
                if key.modifiers.contains(KeyModifiers::CONTROL) && key.code == KeyCode::Char('c') {
                    return Ok(());
                }

                match key.code {
                    KeyCode::Char('q') | KeyCode::Esc => return Ok(()),
                    KeyCode::Char('r') => {
                        app.refresh().await?;
                    }
                    KeyCode::Char('c') => {
                        app.compress_all().await?;
                    }
                    KeyCode::Char('d') => {
                        app.decompress_all().await?;
                    }
                    KeyCode::Char(' ') | KeyCode::Enter => {
                        app.toggle_current_expand();
                    }
                    KeyCode::Char('e') | KeyCode::Tab => {
                        app.toggle_expand_all();
                    }
                    KeyCode::Down | KeyCode::Char('j') => {
                        app.next();
                    }
                    KeyCode::Up | KeyCode::Char('k') => {
                        app.previous();
                    }
                    _ => {}
                }
            }
        }

        if last_tick.elapsed() >= tick_rate {
            last_tick = Instant::now();
        }
    }
}

fn ui(f: &mut Frame, app: &App) {
    let size = f.area();

    let main_block = Block::default()
        .borders(Borders::ALL)
        .title(" scompress ")
        .style(Style::default().fg(Color::Cyan));

    let inner_area = main_block.inner(size);
    f.render_widget(main_block, size);

    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(7), // Summary Stats
            Constraint::Min(5),    // Hierarchical session tree list
            Constraint::Length(3), // Footer & Status
        ])
        .split(inner_area);

    render_summary(f, chunks[0], app);
    render_tree_list(f, chunks[1], app);
    render_footer(f, chunks[2], app);
}

fn render_summary(f: &mut Frame, area: Rect, app: &App) {
    let mut codex_count = 0;
    let mut codex_logical = 0;
    let mut codex_physical = 0;

    let mut claude_count = 0;
    let mut claude_logical = 0;
    let mut claude_physical = 0;

    for s in &app.sessions {
        match s.tool {
            Tool::Codex => {
                codex_count += 1;
                codex_logical += s.logical_size;
                codex_physical += s.physical_size;
            }
            Tool::Claude => {
                claude_count += 1;
                claude_logical += s.logical_size;
                claude_physical += s.physical_size;
            }
        }
    }

    let codex_saved = codex_logical.saturating_sub(codex_physical);
    let claude_saved = claude_logical.saturating_sub(claude_physical);

    let codex_count_str = codex_count.to_string();
    let codex_logical_str = format_size(codex_logical);
    let codex_physical_str = format_size(codex_physical);
    let codex_saved_str = format_size(codex_saved);

    let claude_count_str = claude_count.to_string();
    let claude_logical_str = format_size(claude_logical);
    let claude_physical_str = format_size(claude_physical);
    let claude_saved_str = format_size(claude_saved);

    let rows = vec![
        Row::new(vec![
            "Codex",
            &codex_count_str,
            &codex_logical_str,
            &codex_physical_str,
            &codex_saved_str,
        ]),
        Row::new(vec![
            "Claude",
            &claude_count_str,
            &claude_logical_str,
            &claude_physical_str,
            &claude_saved_str,
        ]),
    ];

    let header = Row::new(vec!["Tool", "Files", "Logical", "Disk", "Saved"]).style(
        Style::default()
            .fg(Color::Yellow)
            .add_modifier(Modifier::BOLD),
    );

    let table = Table::new(
        rows,
        [
            Constraint::Length(10),
            Constraint::Length(8),
            Constraint::Length(14),
            Constraint::Length(14),
            Constraint::Length(14),
        ],
    )
    .header(header)
    .block(Block::default().borders(Borders::BOTTOM));

    f.render_widget(table, area);
}

fn render_tree_list(f: &mut Frame, area: Rect, app: &App) {
    let now = SystemTime::now();

    if app.visible_items.is_empty() {
        let empty_msg =
            Paragraph::new("No session files found.").style(Style::default().fg(Color::DarkGray));
        f.render_widget(empty_msg, area);
        return;
    }

    // Determine scrolling window
    let visible_rows = area.height as usize;
    let start_index = if app.selected_index >= visible_rows {
        app.selected_index - visible_rows + 1
    } else {
        0
    };

    let mut lines = Vec::new();
    for (i, item) in app
        .visible_items
        .iter()
        .enumerate()
        .skip(start_index)
        .take(visible_rows)
    {
        let is_selected = i == app.selected_index;
        let line_style = if is_selected {
            Style::default().bg(Color::Rgb(30, 45, 70))
        } else {
            Style::default()
        };

        match item {
            TreeItem::ProjectHeader {
                tool,
                project,
                count,
                logical_size,
                physical_size,
                is_expanded,
            } => {
                let arrow = if *is_expanded { "▼ " } else { "▶ " };
                let prefix = if is_selected { "> " } else { "  " };

                let saved = logical_size.saturating_sub(*physical_size);
                let info_str = format!(
                    "({} sessions, {} → {}, Saved {})",
                    count,
                    format_size(*logical_size),
                    format_size(*physical_size),
                    format_size(saved)
                );

                let line_spans = vec![
                    Span::raw(prefix),
                    Span::styled(
                        arrow,
                        Style::default()
                            .fg(Color::Yellow)
                            .add_modifier(Modifier::BOLD),
                    ),
                    Span::styled(
                        format!("{}/{} ", tool, project),
                        Style::default()
                            .fg(Color::Cyan)
                            .add_modifier(Modifier::BOLD),
                    ),
                    Span::styled(info_str, Style::default().fg(Color::DarkGray)),
                ];
                lines.push(Line::from(line_spans).style(line_style));
            }
            TreeItem::SessionItem { session_index } => {
                let session = &app.sessions[*session_index];
                let prefix = if is_selected { "  > " } else { "    " };

                let time_str = format_relative_time(session.modified_at, now);
                let title = &session.display_title;

                let mut spans = vec![
                    Span::raw(prefix),
                    Span::styled(
                        format!("{:<34}", truncate_str(title, 34)),
                        if is_selected {
                            Style::default()
                                .fg(Color::White)
                                .add_modifier(Modifier::BOLD)
                        } else {
                            Style::default().fg(Color::Gray)
                        },
                    ),
                    Span::raw(" "),
                ];

                if session.compressed {
                    spans.push(Span::styled(
                        "◉ compressed ",
                        Style::default().fg(Color::Green),
                    ));
                    spans.push(Span::styled(
                        format!(
                            "{:>7} → {:<7}",
                            format_size(session.logical_size),
                            format_size(session.physical_size)
                        ),
                        Style::default().fg(Color::Green),
                    ));
                } else {
                    spans.push(Span::styled(
                        "● normal     ",
                        Style::default().fg(Color::Blue),
                    ));
                    spans.push(Span::styled(
                        format!(
                            "{:>7}   {:<12}",
                            format_size(session.logical_size),
                            time_str
                        ),
                        Style::default().fg(Color::DarkGray),
                    ));
                }

                lines.push(Line::from(spans).style(line_style));
            }
        }
    }

    let list_widget = Paragraph::new(lines);
    f.render_widget(list_widget, area);
}

fn render_footer(f: &mut Frame, area: Rect, app: &App) {
    let keybindings = Line::from(vec![
        Span::styled(
            "Space/Enter ",
            Style::default()
                .fg(Color::Yellow)
                .add_modifier(Modifier::BOLD),
        ),
        Span::raw("Toggle   "),
        Span::styled(
            "e ",
            Style::default()
                .fg(Color::Yellow)
                .add_modifier(Modifier::BOLD),
        ),
        Span::raw("Toggle All   "),
        Span::styled(
            "c ",
            Style::default()
                .fg(Color::Yellow)
                .add_modifier(Modifier::BOLD),
        ),
        Span::raw("Compress   "),
        Span::styled(
            "d ",
            Style::default()
                .fg(Color::Yellow)
                .add_modifier(Modifier::BOLD),
        ),
        Span::raw("Decompress   "),
        Span::styled(
            "r ",
            Style::default()
                .fg(Color::Yellow)
                .add_modifier(Modifier::BOLD),
        ),
        Span::raw("Refresh   "),
        Span::styled(
            "q ",
            Style::default()
                .fg(Color::Yellow)
                .add_modifier(Modifier::BOLD),
        ),
        Span::raw("Quit"),
    ]);

    let status_style = if app.is_busy {
        Style::default().fg(Color::Yellow)
    } else {
        Style::default().fg(Color::Cyan)
    };

    let status = Line::from(vec![Span::styled(&app.status_message, status_style)]);

    let paragraph =
        Paragraph::new(vec![status, keybindings]).block(Block::default().borders(Borders::TOP));

    f.render_widget(paragraph, area);
}

fn truncate_str(s: &str, max_len: usize) -> String {
    if s.chars().count() > max_len {
        let mut res: String = s.chars().take(max_len.saturating_sub(3)).collect();
        res.push_str("...");
        res
    } else {
        s.to_string()
    }
}
