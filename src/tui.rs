use crate::applesauce::{compress, decompress};
use crate::cli::{format_relative_time, format_size};
use crate::model::{SessionFile, Tool, ToolGroup};
use crate::safety::{SkipReason, check_compression_safety, scan_open_files};
use crate::scanner::{build_tool_groups, claude_projects_dir, codex_sessions_dir, scan_all};
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
use std::collections::HashSet;
use std::io::{Stdout, stdout};
use std::path::PathBuf;
use std::time::{Duration, Instant, SystemTime};

#[derive(Clone, Debug)]
pub enum TreeItem {
    ToolHeader {
        tool: Tool,
        is_expanded: bool,
    },
    ProjectHeader {
        tool: Tool,
        project: String,
        is_expanded: bool,
    },
    SessionItem {
        session: SessionFile,
    },
}

pub struct App {
    pub tool_groups: Vec<ToolGroup>,
    pub collapsed_tools: HashSet<Tool>,
    pub collapsed_projects: HashSet<(Tool, String)>,
    pub selected_sessions: HashSet<PathBuf>,
    pub last_selected_index: Option<usize>,
    pub visible_items: Vec<TreeItem>,
    pub selected_index: usize,
    pub status_message: String,
    pub is_busy: bool,
}

impl App {
    pub fn new() -> Self {
        Self {
            tool_groups: Vec::new(),
            collapsed_tools: HashSet::new(),
            collapsed_projects: HashSet::new(),
            selected_sessions: HashSet::new(),
            last_selected_index: None,
            visible_items: Vec::new(),
            selected_index: 0,
            status_message: "s: Select | S: Range select | c: Compress | d: Decompress | Space: Toggle node | r: Refresh".to_string(),
            is_busy: false,
        }
    }

    pub fn set_sessions(&mut self, sessions: Vec<SessionFile>) {
        self.tool_groups = build_tool_groups(&sessions);
        self.rebuild_visible_items();
    }

    pub fn all_sessions(&self) -> Vec<SessionFile> {
        let mut list = Vec::new();
        for tg in &self.tool_groups {
            for pg in &tg.projects {
                list.extend(pg.sessions.clone());
            }
        }
        list
    }

    pub fn rebuild_visible_items(&mut self) {
        let mut items = Vec::new();

        for tg in &self.tool_groups {
            let is_tool_expanded = !self.collapsed_tools.contains(&tg.tool);
            items.push(TreeItem::ToolHeader {
                tool: tg.tool,
                is_expanded: is_tool_expanded,
            });

            if is_tool_expanded {
                for pg in &tg.projects {
                    let is_proj_expanded = !self
                        .collapsed_projects
                        .contains(&(tg.tool, pg.name.clone()));
                    items.push(TreeItem::ProjectHeader {
                        tool: tg.tool,
                        project: pg.name.clone(),
                        is_expanded: is_proj_expanded,
                    });

                    if is_proj_expanded {
                        for s in &pg.sessions {
                            items.push(TreeItem::SessionItem { session: s.clone() });
                        }
                    }
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
                let count = sessions.len();
                self.set_sessions(sessions);
                if self.selected_sessions.is_empty() {
                    self.status_message = format!(
                        "Discovered {} sessions across {} tools",
                        count,
                        self.tool_groups.len()
                    );
                } else {
                    self.status_message = format!(
                        "{} sessions selected (out of {} total)",
                        self.selected_sessions.len(),
                        count
                    );
                }
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
            TreeItem::ToolHeader { tool, .. } => {
                if self.collapsed_tools.contains(tool) {
                    self.collapsed_tools.remove(tool);
                } else {
                    self.collapsed_tools.insert(*tool);
                }
                self.rebuild_visible_items();
            }
            TreeItem::ProjectHeader { tool, project, .. } => {
                let key = (*tool, project.clone());
                if self.collapsed_projects.contains(&key) {
                    self.collapsed_projects.remove(&key);
                } else {
                    self.collapsed_projects.insert(key);
                }
                self.rebuild_visible_items();
            }
            TreeItem::SessionItem { session } => {
                let key = (session.tool, session.project.clone());
                self.collapsed_projects.insert(key);
                self.rebuild_visible_items();
            }
        }
    }

    pub fn toggle_expand_all(&mut self) {
        if self.collapsed_tools.is_empty() && self.collapsed_projects.is_empty() {
            for tg in &self.tool_groups {
                self.collapsed_tools.insert(tg.tool);
                for pg in &tg.projects {
                    self.collapsed_projects.insert((tg.tool, pg.name.clone()));
                }
            }
        } else {
            self.collapsed_tools.clear();
            self.collapsed_projects.clear();
        }
        self.rebuild_visible_items();
    }

    /// Toggle selection of the current node ('s')
    pub fn toggle_select_current(&mut self) {
        if self.visible_items.is_empty() {
            return;
        }

        self.last_selected_index = Some(self.selected_index);

        match &self.visible_items[self.selected_index] {
            TreeItem::ToolHeader { tool, .. } => {
                let sessions: Vec<PathBuf> = self
                    .tool_groups
                    .iter()
                    .filter(|tg| tg.tool == *tool)
                    .flat_map(|tg| {
                        tg.projects
                            .iter()
                            .flat_map(|pg| pg.sessions.iter().map(|s| s.path.clone()))
                    })
                    .collect();

                let all_selected = !sessions.is_empty()
                    && sessions.iter().all(|p| self.selected_sessions.contains(p));
                if all_selected {
                    for p in &sessions {
                        self.selected_sessions.remove(p);
                    }
                } else {
                    for p in sessions {
                        self.selected_sessions.insert(p);
                    }
                }
            }
            TreeItem::ProjectHeader { tool, project, .. } => {
                let sessions: Vec<PathBuf> = self
                    .tool_groups
                    .iter()
                    .filter(|tg| tg.tool == *tool)
                    .flat_map(|tg| tg.projects.iter())
                    .filter(|pg| pg.name == *project)
                    .flat_map(|pg| pg.sessions.iter().map(|s| s.path.clone()))
                    .collect();

                let all_selected = !sessions.is_empty()
                    && sessions.iter().all(|p| self.selected_sessions.contains(p));
                if all_selected {
                    for p in &sessions {
                        self.selected_sessions.remove(p);
                    }
                } else {
                    for p in sessions {
                        self.selected_sessions.insert(p);
                    }
                }
            }
            TreeItem::SessionItem { session } => {
                if self.selected_sessions.contains(&session.path) {
                    self.selected_sessions.remove(&session.path);
                } else {
                    self.selected_sessions.insert(session.path.clone());
                }
            }
        }

        self.update_selection_status();
    }

    /// Range selection from anchor to current item (Shift + 's' / 'S')
    pub fn select_range_to_current(&mut self) {
        if self.visible_items.is_empty() {
            return;
        }

        let start = self
            .last_selected_index
            .unwrap_or(0)
            .min(self.selected_index);
        let end = self
            .last_selected_index
            .unwrap_or(0)
            .max(self.selected_index);

        for idx in start..=end {
            if let Some(item) = self.visible_items.get(idx) {
                match item {
                    TreeItem::ToolHeader { tool, .. } => {
                        let sessions: Vec<PathBuf> = self
                            .tool_groups
                            .iter()
                            .filter(|tg| tg.tool == *tool)
                            .flat_map(|tg| {
                                tg.projects
                                    .iter()
                                    .flat_map(|pg| pg.sessions.iter().map(|s| s.path.clone()))
                            })
                            .collect();
                        for p in sessions {
                            self.selected_sessions.insert(p);
                        }
                    }
                    TreeItem::ProjectHeader { tool, project, .. } => {
                        let sessions: Vec<PathBuf> = self
                            .tool_groups
                            .iter()
                            .filter(|tg| tg.tool == *tool)
                            .flat_map(|tg| tg.projects.iter())
                            .filter(|pg| pg.name == *project)
                            .flat_map(|pg| pg.sessions.iter().map(|s| s.path.clone()))
                            .collect();
                        for p in sessions {
                            self.selected_sessions.insert(p);
                        }
                    }
                    TreeItem::SessionItem { session } => {
                        self.selected_sessions.insert(session.path.clone());
                    }
                }
            }
        }

        self.last_selected_index = Some(self.selected_index);
        self.update_selection_status();
    }

    pub fn clear_selection(&mut self) {
        self.selected_sessions.clear();
        self.last_selected_index = None;
        self.status_message = "Selection cleared".to_string();
    }

    fn update_selection_status(&mut self) {
        let count = self.selected_sessions.len();
        if count == 0 {
            self.status_message = "No items selected".to_string();
        } else {
            self.status_message = format!(
                "{} session{} selected (Press 'c' to compress, 'd' to decompress)",
                count,
                if count == 1 { "" } else { "s" }
            );
        }
    }

    /// Determine target sessions based on selection mode or currently highlighted node.
    pub fn resolve_targets(&self) -> (String, Vec<SessionFile>) {
        if !self.selected_sessions.is_empty() {
            let all = self.all_sessions();
            let targets: Vec<SessionFile> = all
                .into_iter()
                .filter(|s| self.selected_sessions.contains(&s.path))
                .collect();
            return (
                format!(
                    "{} selected session{}",
                    targets.len(),
                    if targets.len() == 1 { "" } else { "s" }
                ),
                targets,
            );
        }

        if self.visible_items.is_empty() {
            return ("all".to_string(), self.all_sessions());
        }

        match &self.visible_items[self.selected_index] {
            TreeItem::ToolHeader { tool, .. } => {
                let sessions: Vec<SessionFile> = self
                    .tool_groups
                    .iter()
                    .filter(|tg| tg.tool == *tool)
                    .flat_map(|tg| tg.projects.iter().flat_map(|pg| pg.sessions.clone()))
                    .collect();
                (format!("{} ({} sessions)", tool, sessions.len()), sessions)
            }
            TreeItem::ProjectHeader { tool, project, .. } => {
                let sessions: Vec<SessionFile> = self
                    .tool_groups
                    .iter()
                    .filter(|tg| tg.tool == *tool)
                    .flat_map(|tg| tg.projects.iter())
                    .filter(|pg| pg.name == *project)
                    .flat_map(|pg| pg.sessions.clone())
                    .collect();
                (
                    format!("{}/{} ({} sessions)", tool, project, sessions.len()),
                    sessions,
                )
            }
            TreeItem::SessionItem { session } => (
                format!("session '{}'", session.label()),
                vec![session.clone()],
            ),
        }
    }

    pub async fn compress_targets(&mut self) -> Result<()> {
        if self.is_busy {
            return Ok(());
        }
        let (target_label, targets) = self.resolve_targets();
        if targets.is_empty() {
            self.status_message = "No sessions to compress".to_string();
            return Ok(());
        }

        self.is_busy = true;
        self.status_message = format!("Compressing {}...", target_label);

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

        for s in &targets {
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

        if let Ok(sessions) = scan_all().await {
            self.set_sessions(sessions);
        }

        self.status_message = format!(
            "Compressed {}: {} ok, {} skipped, {} failed",
            target_label, compressed, skipped, failed
        );
        self.is_busy = false;
        Ok(())
    }

    pub async fn decompress_targets(&mut self) -> Result<()> {
        if self.is_busy {
            return Ok(());
        }
        let (target_label, targets) = self.resolve_targets();
        if targets.is_empty() {
            self.status_message = "No sessions to decompress".to_string();
            return Ok(());
        }

        self.is_busy = true;
        self.status_message = format!("Decompressing {}...", target_label);

        let mut decompressed = 0;
        let mut skipped = 0;
        let mut failed = 0;

        for s in &targets {
            if !s.compressed {
                skipped += 1;
                continue;
            }

            match decompress(&s.path).await {
                Ok(()) => decompressed += 1,
                Err(_) => failed += 1,
            }
        }

        if let Ok(sessions) = scan_all().await {
            self.set_sessions(sessions);
        }

        self.status_message = format!(
            "Decompressed {}: {} ok, {} skipped, {} failed",
            target_label, decompressed, skipped, failed
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
                    KeyCode::Char('q') => return Ok(()),
                    KeyCode::Esc => {
                        if !app.selected_sessions.is_empty() {
                            app.clear_selection();
                        } else {
                            return Ok(());
                        }
                    }
                    KeyCode::Char('s') => {
                        app.toggle_select_current();
                    }
                    KeyCode::Char('S') => {
                        app.select_range_to_current();
                    }
                    KeyCode::Char('x') => {
                        app.clear_selection();
                    }
                    KeyCode::Char('r') => {
                        app.refresh().await?;
                    }
                    KeyCode::Char('c') => {
                        app.compress_targets().await?;
                    }
                    KeyCode::Char('d') => {
                        app.decompress_targets().await?;
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
            Constraint::Min(5),    // Hierarchical expandable session tree
            Constraint::Length(3), // Footer & Status
        ])
        .split(inner_area);

    render_summary(f, chunks[0], app);
    render_tree_list(f, chunks[1], app);
    render_footer(f, chunks[2], app);
}

fn render_summary(f: &mut Frame, area: Rect, app: &App) {
    let mut rows = Vec::new();

    for tg in &app.tool_groups {
        let count_str = tg.file_count().to_string();
        let log_str = format_size(tg.logical_size());
        let phy_str = format_size(tg.physical_size());
        let sav_str = format_size(tg.saved_size());

        rows.push(Row::new(vec![
            tg.tool.to_string(),
            count_str,
            log_str,
            phy_str,
            sav_str,
        ]));
    }

    if rows.is_empty() {
        rows.push(Row::new(vec![
            "No sessions".to_string(),
            "0".to_string(),
            "0 B".to_string(),
            "0 B".to_string(),
            "0 B".to_string(),
        ]));
    }

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

    let visible_rows = area.height as usize;
    let start_index = if app.selected_index >= visible_rows {
        app.selected_index - visible_rows + 1
    } else {
        0
    };

    let total_width = area.width as usize;

    let mut lines = Vec::new();
    for (i, item) in app
        .visible_items
        .iter()
        .enumerate()
        .skip(start_index)
        .take(visible_rows)
    {
        let is_cursor = i == app.selected_index;
        let line_style = if is_cursor {
            Style::default().bg(Color::Rgb(30, 45, 70))
        } else {
            Style::default()
        };

        match item {
            TreeItem::ToolHeader { tool, is_expanded } => {
                let arrow = if *is_expanded { "▼ " } else { "▶ " };
                let cursor_prefix = if is_cursor { "> " } else { "  " };

                let tg_opt = app.tool_groups.iter().find(|g| g.tool == *tool);
                let check_str = if let Some(tg) = tg_opt {
                    let paths: Vec<&PathBuf> = tg
                        .projects
                        .iter()
                        .flat_map(|pg| pg.sessions.iter().map(|s| &s.path))
                        .collect();
                    if paths.is_empty() {
                        "[ ] "
                    } else if paths.iter().all(|p| app.selected_sessions.contains(*p)) {
                        "[✓] "
                    } else if paths.iter().any(|p| app.selected_sessions.contains(*p)) {
                        "[-] "
                    } else {
                        "[ ] "
                    }
                } else {
                    "[ ] "
                };

                let check_style = if check_str == "[✓] " {
                    Style::default()
                        .fg(Color::Green)
                        .add_modifier(Modifier::BOLD)
                } else if check_str == "[-] " {
                    Style::default().fg(Color::Yellow)
                } else {
                    Style::default().fg(Color::DarkGray)
                };

                let info_str = if let Some(tg) = tg_opt {
                    format!(
                        " ({} files, {} → {}, Saved {})",
                        tg.file_count(),
                        format_size(tg.logical_size()),
                        format_size(tg.physical_size()),
                        format_size(tg.saved_size())
                    )
                } else {
                    "".to_string()
                };

                let spans = vec![
                    Span::raw(cursor_prefix),
                    Span::styled(check_str, check_style),
                    Span::styled(
                        arrow,
                        Style::default()
                            .fg(Color::Yellow)
                            .add_modifier(Modifier::BOLD),
                    ),
                    Span::styled(
                        tool.to_string(),
                        Style::default()
                            .fg(Color::Cyan)
                            .add_modifier(Modifier::BOLD),
                    ),
                    Span::styled(info_str, Style::default().fg(Color::DarkGray)),
                ];
                lines.push(Line::from(spans).style(line_style));
            }
            TreeItem::ProjectHeader {
                tool,
                project,
                is_expanded,
            } => {
                let arrow = if *is_expanded { "▼ " } else { "▶ " };
                let cursor_prefix = if is_cursor { "  > " } else { "    " };

                let pg_opt = app
                    .tool_groups
                    .iter()
                    .find(|g| g.tool == *tool)
                    .and_then(|tg| tg.projects.iter().find(|p| p.name == *project));

                let check_str = if let Some(pg) = pg_opt {
                    if pg.sessions.is_empty() {
                        "[ ] "
                    } else if pg
                        .sessions
                        .iter()
                        .all(|s| app.selected_sessions.contains(&s.path))
                    {
                        "[✓] "
                    } else if pg
                        .sessions
                        .iter()
                        .any(|s| app.selected_sessions.contains(&s.path))
                    {
                        "[-] "
                    } else {
                        "[ ] "
                    }
                } else {
                    "[ ] "
                };

                let check_style = if check_str == "[✓] " {
                    Style::default()
                        .fg(Color::Green)
                        .add_modifier(Modifier::BOLD)
                } else if check_str == "[-] " {
                    Style::default().fg(Color::Yellow)
                } else {
                    Style::default().fg(Color::DarkGray)
                };

                let info_str = if let Some(pg) = pg_opt {
                    format!(
                        " ({} sessions, {} → {}, Saved {})",
                        pg.sessions.len(),
                        format_size(pg.logical_size()),
                        format_size(pg.physical_size()),
                        format_size(pg.saved_size())
                    )
                } else {
                    "".to_string()
                };

                let spans = vec![
                    Span::raw(cursor_prefix),
                    Span::styled(check_str, check_style),
                    Span::styled(
                        arrow,
                        Style::default()
                            .fg(Color::Yellow)
                            .add_modifier(Modifier::BOLD),
                    ),
                    Span::styled(
                        project.clone(),
                        Style::default()
                            .fg(Color::White)
                            .add_modifier(Modifier::BOLD),
                    ),
                    Span::styled(info_str, Style::default().fg(Color::DarkGray)),
                ];
                lines.push(Line::from(spans).style(line_style));
            }
            TreeItem::SessionItem { session } => {
                let cursor_prefix = if is_cursor { "    > " } else { "      " };
                let is_checked = app.selected_sessions.contains(&session.path);
                let check_str = if is_checked { "[✓] " } else { "[ ] " };
                let check_style = if is_checked {
                    Style::default()
                        .fg(Color::Green)
                        .add_modifier(Modifier::BOLD)
                } else {
                    Style::default().fg(Color::DarkGray)
                };

                let time_str = format_relative_time(session.modified_at, now);
                let label = session.label();

                let right_block_len = 42;
                let left_indent = 10;
                let available_title_len = total_width
                    .saturating_sub(right_block_len + left_indent)
                    .max(20);

                let display_title = truncate_str(label, available_title_len);

                let mut spans = vec![
                    Span::raw(cursor_prefix),
                    Span::styled(check_str, check_style),
                    Span::styled(
                        format!("{:<width$}", display_title, width = available_title_len),
                        if is_cursor {
                            Style::default()
                                .fg(Color::White)
                                .add_modifier(Modifier::BOLD)
                        } else if is_checked {
                            Style::default().fg(Color::Cyan)
                        } else {
                            Style::default().fg(Color::Gray)
                        },
                    ),
                    Span::raw("  "),
                ];

                if session.compressed {
                    spans.push(Span::styled(
                        "◉ compressed ",
                        Style::default().fg(Color::Green),
                    ));
                    spans.push(Span::styled(
                        format!(
                            "{:>8} → {:<8}",
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
                            "{:>8}   {:<12}",
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
    let sel_count = app.selected_sessions.len();

    let mut key_spans = vec![
        Span::styled(
            "s ",
            Style::default()
                .fg(Color::Yellow)
                .add_modifier(Modifier::BOLD),
        ),
        Span::raw("Select  "),
        Span::styled(
            "S ",
            Style::default()
                .fg(Color::Yellow)
                .add_modifier(Modifier::BOLD),
        ),
        Span::raw("Range  "),
        Span::styled(
            "Space ",
            Style::default()
                .fg(Color::Yellow)
                .add_modifier(Modifier::BOLD),
        ),
        Span::raw("Toggle  "),
    ];

    if sel_count > 0 {
        key_spans.extend(vec![
            Span::styled(
                "c ",
                Style::default()
                    .fg(Color::Green)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::styled(
                format!("Compress ({})  ", sel_count),
                Style::default().fg(Color::Green),
            ),
            Span::styled(
                "d ",
                Style::default()
                    .fg(Color::Green)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::styled(
                format!("Decompress ({})  ", sel_count),
                Style::default().fg(Color::Green),
            ),
            Span::styled(
                "Esc/x ",
                Style::default()
                    .fg(Color::Yellow)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::raw("Clear  "),
        ]);
    } else {
        key_spans.extend(vec![
            Span::styled(
                "c ",
                Style::default()
                    .fg(Color::Yellow)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::raw("Compress Node  "),
            Span::styled(
                "d ",
                Style::default()
                    .fg(Color::Yellow)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::raw("Decompress Node  "),
        ]);
    }

    key_spans.extend(vec![
        Span::styled(
            "r ",
            Style::default()
                .fg(Color::Yellow)
                .add_modifier(Modifier::BOLD),
        ),
        Span::raw("Refresh  "),
        Span::styled(
            "q ",
            Style::default()
                .fg(Color::Yellow)
                .add_modifier(Modifier::BOLD),
        ),
        Span::raw("Quit"),
    ]);

    let keybindings = Line::from(key_spans);

    let status_style = if app.is_busy {
        Style::default().fg(Color::Yellow)
    } else if sel_count > 0 {
        Style::default()
            .fg(Color::Green)
            .add_modifier(Modifier::BOLD)
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{SessionFile, Tool};
    use std::path::PathBuf;
    use std::time::SystemTime;

    fn sample_app() -> App {
        let mut app = App::new();
        let sessions = vec![
            SessionFile {
                tool: Tool::Codex,
                project: "proj-a".to_string(),
                title: Some("Session A1".to_string()),
                display_title: "A1".to_string(),
                path: PathBuf::from("/tmp/a1.jsonl"),
                logical_size: 100,
                physical_size: 10,
                compressed: false,
                modified_at: SystemTime::now(),
            },
            SessionFile {
                tool: Tool::Codex,
                project: "proj-a".to_string(),
                title: Some("Session A2".to_string()),
                display_title: "A2".to_string(),
                path: PathBuf::from("/tmp/a2.jsonl"),
                logical_size: 200,
                physical_size: 20,
                compressed: false,
                modified_at: SystemTime::now(),
            },
            SessionFile {
                tool: Tool::Codex,
                project: "proj-b".to_string(),
                title: Some("Session B1".to_string()),
                display_title: "B1".to_string(),
                path: PathBuf::from("/tmp/b1.jsonl"),
                logical_size: 300,
                physical_size: 30,
                compressed: false,
                modified_at: SystemTime::now(),
            },
        ];
        app.set_sessions(sessions);
        app
    }

    #[test]
    fn test_single_selection_and_clear() {
        let mut app = sample_app();
        let a1_idx = app
            .visible_items
            .iter()
            .position(|item| match item {
                TreeItem::SessionItem { session } => session.path == PathBuf::from("/tmp/a1.jsonl"),
                _ => false,
            })
            .expect("Session A1 not found in visible items");

        app.selected_index = a1_idx;
        app.toggle_select_current();
        assert!(
            app.selected_sessions
                .contains(&PathBuf::from("/tmp/a1.jsonl"))
        );
        assert_eq!(app.selected_sessions.len(), 1);

        app.clear_selection();
        assert!(app.selected_sessions.is_empty());
    }

    #[test]
    fn test_project_selection_toggles_all_children() {
        let mut app = sample_app();
        let proj_a_idx = app
            .visible_items
            .iter()
            .position(|item| match item {
                TreeItem::ProjectHeader { project, .. } => project == "proj-a",
                _ => false,
            })
            .expect("Project proj-a not found in visible items");

        app.selected_index = proj_a_idx;
        app.toggle_select_current();

        assert!(
            app.selected_sessions
                .contains(&PathBuf::from("/tmp/a1.jsonl"))
        );
        assert!(
            app.selected_sessions
                .contains(&PathBuf::from("/tmp/a2.jsonl"))
        );
        assert_eq!(app.selected_sessions.len(), 2);

        // Toggle again to deselect
        app.toggle_select_current();
        assert!(app.selected_sessions.is_empty());
    }

    #[test]
    fn test_range_selection() {
        let mut app = sample_app();
        let a1_idx = app
            .visible_items
            .iter()
            .position(|item| match item {
                TreeItem::SessionItem { session } => session.path == PathBuf::from("/tmp/a1.jsonl"),
                _ => false,
            })
            .unwrap();

        let b1_idx = app
            .visible_items
            .iter()
            .position(|item| match item {
                TreeItem::SessionItem { session } => session.path == PathBuf::from("/tmp/b1.jsonl"),
                _ => false,
            })
            .unwrap();

        app.selected_index = a1_idx;
        app.toggle_select_current();

        app.selected_index = b1_idx;
        app.select_range_to_current();

        assert!(
            app.selected_sessions
                .contains(&PathBuf::from("/tmp/a1.jsonl"))
        );
        assert!(
            app.selected_sessions
                .contains(&PathBuf::from("/tmp/b1.jsonl"))
        );
    }
}
