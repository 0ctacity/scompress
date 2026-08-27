use crate::applesauce::{
    OpType, ProgressEvent, compress_sync_with_progress, decompress_sync_with_progress, inspect_file,
};
use crate::cli::{format_relative_time, format_size};
use crate::model::{SessionFile, SortConfig, Tool, ToolGroup};
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
    layout::{Alignment, Constraint, Direction, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Paragraph, Row, Table},
};
use std::collections::{HashMap, HashSet};
use std::io::{Stdout, stdout};
use std::path::PathBuf;
use std::time::{Duration, Instant, SystemTime};
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

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

#[derive(Clone, Debug)]
pub enum ActiveOp {
    Queued {
        op: OpType,
    },
    Running {
        op: OpType,
        bytes_done: u64,
        total_bytes: u64,
    },
    Finished {
        op: OpType,
        success: bool,
        logical_size: u64,
        physical_size: u64,
    },
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum BatchStatus {
    Idle,
    Running {
        op: OpType,
        completed: usize,
        total: usize,
    },
    Finished {
        op: OpType,
        total: usize,
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
    pub batch_status: BatchStatus,
    pub row_ops: HashMap<PathBuf, ActiveOp>,
    pub progress_tx: smol::channel::Sender<ProgressEvent>,
    pub progress_rx: smol::channel::Receiver<ProgressEvent>,
    pub tool_sorts: HashMap<Tool, SortConfig>,
    pub project_sorts: HashMap<(Tool, String), SortConfig>,
}

impl App {
    pub fn new() -> Self {
        let (tx, rx) = smol::channel::unbounded();
        Self {
            tool_groups: Vec::new(),
            collapsed_tools: HashSet::new(),
            collapsed_projects: HashSet::new(),
            selected_sessions: HashSet::new(),
            last_selected_index: None,
            visible_items: Vec::new(),
            selected_index: 0,
            status_message: "s: Select | S: Range select | o: Sort | c: Compress | d: Decompress | Space: Toggle node | r: Refresh".to_string(),
            is_busy: false,
            batch_status: BatchStatus::Idle,
            row_ops: HashMap::new(),
            progress_tx: tx,
            progress_rx: rx,
            tool_sorts: HashMap::new(),
            project_sorts: HashMap::new(),
        }
    }

    pub fn set_sessions(&mut self, sessions: Vec<SessionFile>) {
        self.tool_groups = build_tool_groups(&sessions);
        self.apply_sorting();
        self.rebuild_visible_items();
    }

    pub fn apply_sorting(&mut self) {
        for tg in &mut self.tool_groups {
            let tool_sort = self
                .tool_sorts
                .get(&tg.tool)
                .copied()
                .unwrap_or(SortConfig::DEFAULT);
            tg.sort_projects(tool_sort);

            for pg in &mut tg.projects {
                let session_sort = if let Some(ts) = self.tool_sorts.get(&tg.tool) {
                    *ts
                } else if let Some(ps) = self.project_sorts.get(&(tg.tool, pg.name.clone())) {
                    *ps
                } else {
                    SortConfig::DEFAULT
                };
                pg.sort_sessions(session_sort);
            }
        }
    }

    pub fn cycle_sort_current(&mut self) {
        if self.visible_items.is_empty() {
            return;
        }

        match &self.visible_items[self.selected_index] {
            TreeItem::ToolHeader { tool, .. } => {
                let current = self
                    .tool_sorts
                    .get(tool)
                    .copied()
                    .unwrap_or(SortConfig::DEFAULT);
                let next = current.next();
                self.tool_sorts.insert(*tool, next);
                // Higher hierarchy overrides project-wise: clear individual project overrides under this tool
                self.project_sorts.retain(|(t, _), _| t != tool);
                self.status_message = format!("Sorted {} by {}", tool, next.label());
            }
            TreeItem::ProjectHeader { tool, project, .. } => {
                let key = (*tool, project.clone());
                let current = self
                    .project_sorts
                    .get(&key)
                    .or_else(|| self.tool_sorts.get(tool))
                    .copied()
                    .unwrap_or(SortConfig::DEFAULT);
                let next = current.next();
                // Clear tool-level override so this specific project can be custom-sorted
                self.tool_sorts.remove(tool);
                self.project_sorts.insert(key, next);
                self.status_message = format!("Sorted project '{}' by {}", project, next.label());
            }
            TreeItem::SessionItem { session } => {
                let key = (session.tool, session.project.clone());
                let current = self
                    .project_sorts
                    .get(&key)
                    .or_else(|| self.tool_sorts.get(&session.tool))
                    .copied()
                    .unwrap_or(SortConfig::DEFAULT);
                let next = current.next();
                self.tool_sorts.remove(&session.tool);
                self.project_sorts.insert(key, next);
                self.status_message =
                    format!("Sorted project '{}' by {}", session.project, next.label());
            }
        }

        self.apply_sorting();
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

    pub fn handle_progress_event(&mut self, event: ProgressEvent) {
        match event {
            ProgressEvent::Started {
                path,
                op,
                total_bytes,
            } => {
                self.row_ops.insert(
                    path,
                    ActiveOp::Running {
                        op,
                        bytes_done: 0,
                        total_bytes,
                    },
                );
            }
            ProgressEvent::Progress {
                path,
                op,
                bytes_done,
                total_bytes,
            } => {
                self.row_ops.insert(
                    path,
                    ActiveOp::Running {
                        op,
                        bytes_done,
                        total_bytes,
                    },
                );
            }
            ProgressEvent::Completed { path, op, success } => {
                let (compressed, logical_size, physical_size) = inspect_file(&path);
                for tg in &mut self.tool_groups {
                    for pg in &mut tg.projects {
                        for s in &mut pg.sessions {
                            if s.path == path {
                                s.compressed = compressed;
                                s.logical_size = logical_size;
                                s.physical_size = physical_size;
                            }
                        }
                    }
                }
                for item in &mut self.visible_items {
                    if let TreeItem::SessionItem { session } = item
                        && session.path == path
                    {
                        session.compressed = compressed;
                        session.logical_size = logical_size;
                        session.physical_size = physical_size;
                    }
                }
                self.row_ops.insert(
                    path,
                    ActiveOp::Finished {
                        op,
                        success,
                        logical_size,
                        physical_size,
                    },
                );

                if let BatchStatus::Running { completed, .. } = &mut self.batch_status {
                    *completed += 1;
                }

                let still_running = self.row_ops.values().any(|op_state| match op_state {
                    ActiveOp::Queued { .. } | ActiveOp::Running { .. } => true,
                    ActiveOp::Finished { .. } => false,
                });

                if !still_running && self.is_busy {
                    self.is_busy = false;
                    let count = self.row_ops.len();
                    self.batch_status = BatchStatus::Finished { op, total: count };
                    self.status_message = format!(
                        "Finished processing {} session{}",
                        count,
                        if count == 1 { "" } else { "s" }
                    );
                    self.apply_sorting();
                    self.rebuild_visible_items();
                }
            }
        }
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

    pub fn toggle_select_current(&mut self) {
        if self.visible_items.is_empty() {
            return;
        }

        self.last_selected_index = Some(self.selected_index);

        match &self.visible_items[self.selected_index] {
            TreeItem::ToolHeader { tool, .. } => {
                let all_selected = self
                    .tool_groups
                    .iter()
                    .filter(|tg| tg.tool == *tool)
                    .flat_map(|tg| tg.projects.iter().flat_map(|pg| pg.sessions.iter()))
                    .all(|s| self.selected_sessions.contains(&s.path))
                    && self
                        .tool_groups
                        .iter()
                        .filter(|tg| tg.tool == *tool)
                        .flat_map(|tg| tg.projects.iter().flat_map(|pg| pg.sessions.iter()))
                        .next()
                        .is_some();

                if all_selected {
                    for tg in self.tool_groups.iter().filter(|tg| tg.tool == *tool) {
                        for pg in &tg.projects {
                            for s in &pg.sessions {
                                self.selected_sessions.remove(&s.path);
                            }
                        }
                    }
                } else {
                    for tg in self.tool_groups.iter().filter(|tg| tg.tool == *tool) {
                        for pg in &tg.projects {
                            for s in &pg.sessions {
                                self.selected_sessions.insert(s.path.clone());
                            }
                        }
                    }
                }
            }
            TreeItem::ProjectHeader { tool, project, .. } => {
                let all_selected = self
                    .tool_groups
                    .iter()
                    .filter(|tg| tg.tool == *tool)
                    .flat_map(|tg| tg.projects.iter())
                    .filter(|pg| pg.name == *project)
                    .flat_map(|pg| pg.sessions.iter())
                    .all(|s| self.selected_sessions.contains(&s.path))
                    && self
                        .tool_groups
                        .iter()
                        .filter(|tg| tg.tool == *tool)
                        .flat_map(|tg| tg.projects.iter())
                        .filter(|pg| pg.name == *project)
                        .flat_map(|pg| pg.sessions.iter())
                        .next()
                        .is_some();

                if all_selected {
                    for tg in self.tool_groups.iter().filter(|tg| tg.tool == *tool) {
                        for pg in tg.projects.iter().filter(|p| p.name == *project) {
                            for s in &pg.sessions {
                                self.selected_sessions.remove(&s.path);
                            }
                        }
                    }
                } else {
                    for tg in self.tool_groups.iter().filter(|tg| tg.tool == *tool) {
                        for pg in tg.projects.iter().filter(|p| p.name == *project) {
                            for s in &pg.sessions {
                                self.selected_sessions.insert(s.path.clone());
                            }
                        }
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
                        for tg in self.tool_groups.iter().filter(|tg| tg.tool == *tool) {
                            for pg in &tg.projects {
                                for s in &pg.sessions {
                                    self.selected_sessions.insert(s.path.clone());
                                }
                            }
                        }
                    }
                    TreeItem::ProjectHeader { tool, project, .. } => {
                        for tg in self.tool_groups.iter().filter(|tg| tg.tool == *tool) {
                            for pg in tg.projects.iter().filter(|p| p.name == *project) {
                                for s in &pg.sessions {
                                    self.selected_sessions.insert(s.path.clone());
                                }
                            }
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

    pub fn start_batch_operation(&mut self, op: OpType) {
        if self.is_busy {
            return;
        }

        let (target_label, targets) = self.resolve_targets();
        if targets.is_empty() {
            self.status_message = format!("No sessions to {}", op);
            return;
        }

        let total = targets.len();
        self.is_busy = true;
        self.batch_status = BatchStatus::Running {
            op,
            completed: 0,
            total,
        };
        self.status_message = format!("Starting {} for {}...", op, target_label);
        self.row_ops.clear();

        for s in &targets {
            self.row_ops.insert(s.path.clone(), ActiveOp::Queued { op });
        }

        let tx = self.progress_tx.clone();
        let targets_clone = targets.clone();

        std::thread::spawn(move || {
            let mut roots = Vec::new();
            if let Some(d) = codex_sessions_dir() {
                roots.push(d);
            }
            if let Some(d) = claude_projects_dir() {
                roots.push(d);
            }

            let open_files = scan_open_files(&roots);
            let now = SystemTime::now();

            for s in targets_clone {
                match op {
                    OpType::Compressing => match check_compression_safety(&s, &open_files, now) {
                        Ok(()) => {
                            let _ = compress_sync_with_progress(&s.path, tx.clone());
                        }
                        Err(SkipReason::AlreadyCompressed) => {
                            let _ = tx.try_send(ProgressEvent::Completed {
                                path: s.path,
                                op,
                                success: true,
                            });
                        }
                        Err(_) => {
                            let _ = tx.try_send(ProgressEvent::Completed {
                                path: s.path,
                                op,
                                success: false,
                            });
                        }
                    },
                    OpType::Decompressing => {
                        if !s.compressed {
                            let _ = tx.try_send(ProgressEvent::Completed {
                                path: s.path,
                                op,
                                success: true,
                            });
                        } else {
                            let _ = decompress_sync_with_progress(&s.path, tx.clone());
                        }
                    }
                }
            }
        });
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
    let tick_rate = Duration::from_millis(30);
    let mut last_tick = Instant::now();

    loop {
        while let Ok(event) = app.progress_rx.try_recv() {
            app.handle_progress_event(event);
        }

        terminal.draw(|f| ui(f, app))?;

        let timeout = tick_rate
            .checked_sub(last_tick.elapsed())
            .unwrap_or_else(|| Duration::from_secs(0));

        if event::poll(timeout)?
            && let Event::Key(key) = event::read()?
        {
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
                KeyCode::Char('o') | KeyCode::Char('O') => {
                    app.cycle_sort_current();
                }
                KeyCode::Char('r') => {
                    app.refresh().await?;
                }
                KeyCode::Char('c') => {
                    app.start_batch_operation(OpType::Compressing);
                }
                KeyCode::Char('d') => {
                    app.start_batch_operation(OpType::Decompressing);
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

        if last_tick.elapsed() >= tick_rate {
            last_tick = Instant::now();
        }
    }
}

fn ui(f: &mut Frame, app: &App) {
    let size = f.area();

    let mut main_block = Block::default()
        .borders(Borders::ALL)
        .title(" scompress ")
        .style(Style::default().fg(Color::Cyan));

    match &app.batch_status {
        BatchStatus::Running {
            op,
            completed,
            total,
        } => {
            let current = (*completed + 1).min(*total);
            let op_name = match op {
                OpType::Compressing => "Compressing",
                OpType::Decompressing => "Decompressing",
            };
            let title_text = format!(" {} {}/{} ", op_name, current, total);
            main_block = main_block.title_top(
                Line::from(Span::styled(
                    title_text,
                    Style::default()
                        .fg(Color::Yellow)
                        .add_modifier(Modifier::BOLD),
                ))
                .alignment(Alignment::Right),
            );
        }
        BatchStatus::Finished { op, total } => {
            let action_name = match op {
                OpType::Compressing => "Compressed",
                OpType::Decompressing => "Decompressed",
            };
            let title_text = format!(
                " {} {} session{} ",
                action_name,
                total,
                if *total == 1 { "" } else { "s" }
            );
            main_block = main_block.title_top(
                Line::from(Span::styled(
                    title_text,
                    Style::default()
                        .fg(Color::Green)
                        .add_modifier(Modifier::BOLD),
                ))
                .alignment(Alignment::Right),
            );
        }
        BatchStatus::Idle => {}
    }

    let inner_area = main_block.inner(size);
    f.render_widget(main_block, size);

    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(7),
            Constraint::Min(5),
            Constraint::Length(3),
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
                    let mut has_items = false;
                    let mut all_selected = true;
                    let mut any_selected = false;
                    for pg in &tg.projects {
                        for s in &pg.sessions {
                            has_items = true;
                            if app.selected_sessions.contains(&s.path) {
                                any_selected = true;
                            } else {
                                all_selected = false;
                            }
                        }
                    }
                    if !has_items {
                        "[ ] "
                    } else if all_selected {
                        "[✓] "
                    } else if any_selected {
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

                let sort_tag = if let Some(s) = app.tool_sorts.get(tool) {
                    format!(" [{}]", s.short_label())
                } else {
                    "".to_string()
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
                    Span::styled(sort_tag, Style::default().fg(Color::Magenta)),
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
                    } else {
                        let total = pg.sessions.len();
                        let selected = pg
                            .sessions
                            .iter()
                            .filter(|s| app.selected_sessions.contains(&s.path))
                            .count();
                        if selected == 0 {
                            "[ ] "
                        } else if selected == total {
                            "[✓] "
                        } else {
                            "[-] "
                        }
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

                let sort_tag = if let Some(s) = app.project_sorts.get(&(*tool, project.clone())) {
                    format!(" [{}]", s.short_label())
                } else {
                    "".to_string()
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
                    Span::styled(sort_tag, Style::default().fg(Color::Magenta)),
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

                let right_block_len = 48;
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

                if let Some(op_state) = app.row_ops.get(&session.path) {
                    match op_state {
                        ActiveOp::Running {
                            op,
                            bytes_done,
                            total_bytes,
                        } => {
                            let ratio = if *total_bytes > 0 {
                                (*bytes_done as f64 / *total_bytes as f64).clamp(0.0, 1.0)
                            } else {
                                0.0
                            };
                            let percent = (ratio * 100.0) as u32;
                            let filled = (ratio * 8.0).round() as usize;
                            let bar = format!("[{}{}]", "█".repeat(filled), "░".repeat(8 - filled));

                            spans.push(Span::styled(
                                format!("{} {:>3}% ", bar, percent),
                                Style::default()
                                    .fg(Color::Cyan)
                                    .add_modifier(Modifier::BOLD),
                            ));
                            spans.push(Span::styled(
                                op.to_string(),
                                Style::default()
                                    .fg(Color::Yellow)
                                    .add_modifier(Modifier::BOLD),
                            ));
                        }
                        ActiveOp::Queued { op } => {
                            spans.push(Span::styled(
                                format!("[queued] {}", op),
                                Style::default().fg(Color::DarkGray),
                            ));
                        }
                        ActiveOp::Finished {
                            op,
                            success,
                            logical_size,
                            physical_size,
                        } => {
                            if *success {
                                if *op == OpType::Compressing {
                                    spans.push(Span::styled(
                                        "✓ compressed ",
                                        Style::default().fg(Color::Green),
                                    ));
                                    spans.push(Span::styled(
                                        format!(
                                            "{:>8} → {:<8}",
                                            format_size(*logical_size),
                                            format_size(*physical_size)
                                        ),
                                        Style::default().fg(Color::Green),
                                    ));
                                } else {
                                    spans.push(Span::styled(
                                        "✓ decompressed",
                                        Style::default().fg(Color::Green),
                                    ));
                                    spans.push(Span::styled(
                                        format!("{:>18}", format_size(*logical_size)),
                                        Style::default().fg(Color::Green),
                                    ));
                                }
                                spans.push(Span::raw("  "));
                                spans.push(Span::styled(
                                    format!("{:<12}", time_str),
                                    Style::default().fg(Color::DarkGray),
                                ));
                            } else {
                                spans.push(Span::styled(
                                    "✗ failed",
                                    Style::default().fg(Color::Red),
                                ));
                            }
                        }
                    }
                } else if session.compressed {
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
                    spans.push(Span::raw("  "));
                    spans.push(Span::styled(
                        format!("{:<12}", time_str),
                        Style::default().fg(Color::DarkGray),
                    ));
                } else {
                    spans.push(Span::styled(
                        "● normal     ",
                        Style::default().fg(Color::Blue),
                    ));
                    spans.push(Span::styled(
                        format!("{:>19}", format_size(session.logical_size)),
                        Style::default().fg(Color::DarkGray),
                    ));
                    spans.push(Span::raw("  "));
                    spans.push(Span::styled(
                        format!("{:<12}", time_str),
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
            "o ",
            Style::default()
                .fg(Color::Yellow)
                .add_modifier(Modifier::BOLD),
        ),
        Span::raw("Sort  "),
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
        Style::default()
            .fg(Color::Yellow)
            .add_modifier(Modifier::BOLD)
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
    if s.width() > max_len {
        let mut width = 0;
        let mut res = String::new();
        let target_len = max_len.saturating_sub(3);
        for c in s.chars() {
            let cw = c.width().unwrap_or(0);
            if width + cw > target_len {
                break;
            }
            width += cw;
            res.push(c);
        }
        res.push_str("...");
        res
    } else {
        s.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{SessionFile, SortDirection, SortField, Tool};
    use std::path::{Path, PathBuf};
    use std::time::{Duration, SystemTime};

    #[test]
    fn test_truncate_str_ascii_and_unicode() {
        assert_eq!(truncate_str("Hello World", 20), "Hello World");
        assert_eq!(truncate_str("Hello World", 8), "Hello...");
        assert_eq!(truncate_str("こんにちは世界", 8), "こん...");
    }

    fn sample_app() -> App {
        let mut app = App::new();
        let base_time = SystemTime::UNIX_EPOCH + Duration::from_secs(1_000_000);
        let sessions = vec![
            SessionFile {
                tool: Tool::Codex,
                project: "proj-b".to_string(),
                title: Some("Session B1".to_string()),
                display_title: "B1".to_string(),
                path: PathBuf::from("/tmp/b1.jsonl"),
                logical_size: 300,
                physical_size: 30,
                compressed: false,
                modified_at: base_time + Duration::from_secs(100),
            },
            SessionFile {
                tool: Tool::Codex,
                project: "proj-a".to_string(),
                title: Some("Alpha Thread".to_string()),
                display_title: "A1".to_string(),
                path: PathBuf::from("/tmp/a1.jsonl"),
                logical_size: 500,
                physical_size: 10,
                compressed: false,
                modified_at: base_time + Duration::from_secs(300),
            },
            SessionFile {
                tool: Tool::Codex,
                project: "proj-a".to_string(),
                title: Some("Beta Thread".to_string()),
                display_title: "A2".to_string(),
                path: PathBuf::from("/tmp/a2.jsonl"),
                logical_size: 100,
                physical_size: 20,
                compressed: false,
                modified_at: base_time + Duration::from_secs(200),
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
                TreeItem::SessionItem { session } => session.path == Path::new("/tmp/a1.jsonl"),
                _ => false,
            })
            .expect("Session A1 not found in visible items");

        app.selected_index = a1_idx;
        app.toggle_select_current();
        assert!(app.selected_sessions.contains(Path::new("/tmp/a1.jsonl")));
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

        assert!(app.selected_sessions.contains(Path::new("/tmp/a1.jsonl")));
        assert!(app.selected_sessions.contains(Path::new("/tmp/a2.jsonl")));
        assert_eq!(app.selected_sessions.len(), 2);

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
                TreeItem::SessionItem { session } => session.path == Path::new("/tmp/a1.jsonl"),
                _ => false,
            })
            .unwrap();

        let b1_idx = app
            .visible_items
            .iter()
            .position(|item| match item {
                TreeItem::SessionItem { session } => session.path == Path::new("/tmp/b1.jsonl"),
                _ => false,
            })
            .unwrap();

        app.selected_index = a1_idx;
        app.toggle_select_current();

        app.selected_index = b1_idx;
        app.select_range_to_current();

        assert!(app.selected_sessions.contains(Path::new("/tmp/a1.jsonl")));
        assert!(app.selected_sessions.contains(Path::new("/tmp/b1.jsonl")));
    }

    #[test]
    fn test_sorting_and_hierarchy_override() {
        let mut app = sample_app();

        // 1. Tool-level sort by Project Name Ascending
        let codex_idx = app
            .visible_items
            .iter()
            .position(|item| {
                matches!(
                    item,
                    TreeItem::ToolHeader {
                        tool: Tool::Codex,
                        ..
                    }
                )
            })
            .unwrap();

        app.selected_index = codex_idx;
        app.tool_sorts.insert(
            Tool::Codex,
            SortConfig {
                field: SortField::Name,
                direction: SortDirection::Asc,
            },
        );
        app.apply_sorting();
        app.rebuild_visible_items();

        // Check project order under Codex: proj-a before proj-b
        let projects: Vec<String> = app
            .visible_items
            .iter()
            .filter_map(|item| match item {
                TreeItem::ProjectHeader { project, .. } => Some(project.clone()),
                _ => None,
            })
            .collect();
        assert_eq!(projects, vec!["proj-a", "proj-b"]);

        // 2. Project-level sort for proj-a by Size Ascending
        let proj_a_idx = app
            .visible_items
            .iter()
            .position(|item| match item {
                TreeItem::ProjectHeader { project, .. } => project == "proj-a",
                _ => false,
            })
            .unwrap();
        app.selected_index = proj_a_idx;
        app.tool_sorts.remove(&Tool::Codex); // Clear tool override
        app.project_sorts.insert(
            (Tool::Codex, "proj-a".to_string()),
            SortConfig {
                field: SortField::Size,
                direction: SortDirection::Asc,
            },
        );
        app.apply_sorting();
        app.rebuild_visible_items();

        let proj_a_sessions: Vec<u64> = app.tool_groups[0]
            .projects
            .iter()
            .find(|p| p.name == "proj-a")
            .unwrap()
            .sessions
            .iter()
            .map(|s| s.logical_size)
            .collect();
        assert_eq!(proj_a_sessions, vec![100, 500]); // 100 before 500

        // 3. Higher hierarchy override: Tool-level sort by Size Descending overrides proj-a
        app.tool_sorts.insert(
            Tool::Codex,
            SortConfig {
                field: SortField::Size,
                direction: SortDirection::Desc,
            },
        );
        app.apply_sorting();
        let overridden_sessions: Vec<u64> = app.tool_groups[0]
            .projects
            .iter()
            .find(|p| p.name == "proj-a")
            .unwrap()
            .sessions
            .iter()
            .map(|s| s.logical_size)
            .collect();
        assert_eq!(overridden_sessions, vec![500, 100]); // Now 500 before 100
    }

    #[test]
    fn test_handle_progress_events_and_batch_status() {
        let mut app = sample_app();
        let test_path = PathBuf::from("/tmp/a1.jsonl");

        app.batch_status = BatchStatus::Running {
            op: OpType::Compressing,
            completed: 0,
            total: 2,
        };
        app.is_busy = true;

        app.handle_progress_event(ProgressEvent::Started {
            path: test_path.clone(),
            op: OpType::Compressing,
            total_bytes: 1000,
        });

        match app.row_ops.get(&test_path) {
            Some(ActiveOp::Running {
                op,
                bytes_done,
                total_bytes,
            }) => {
                assert_eq!(*op, OpType::Compressing);
                assert_eq!(*bytes_done, 0);
                assert_eq!(*total_bytes, 1000);
            }
            _ => panic!("Expected Running state"),
        }

        app.handle_progress_event(ProgressEvent::Progress {
            path: test_path.clone(),
            op: OpType::Compressing,
            bytes_done: 600,
            total_bytes: 1000,
        });

        match app.row_ops.get(&test_path) {
            Some(ActiveOp::Running { bytes_done, .. }) => {
                assert_eq!(*bytes_done, 600);
            }
            _ => panic!("Expected Running state with updated bytes"),
        }

        app.handle_progress_event(ProgressEvent::Completed {
            path: test_path.clone(),
            op: OpType::Compressing,
            success: true,
        });

        match app.row_ops.get(&test_path) {
            Some(ActiveOp::Finished {
                op,
                success,
                logical_size,
                physical_size,
            }) => {
                assert_eq!(*op, OpType::Compressing);
                assert!(*success);
                let _ = (*logical_size, *physical_size);
            }
            _ => panic!("Expected Finished state"),
        }

        assert_eq!(
            app.batch_status,
            BatchStatus::Finished {
                op: OpType::Compressing,
                total: 1
            }
        );
        assert!(!app.is_busy);
    }
}
