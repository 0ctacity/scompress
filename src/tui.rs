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
use std::io::{Stdout, stdout};
use std::time::{Duration, Instant, SystemTime};

pub struct App {
    pub sessions: Vec<SessionFile>,
    pub selected_index: usize,
    pub status_message: String,
    pub is_busy: bool,
}

impl App {
    pub fn new() -> Self {
        Self {
            sessions: Vec::new(),
            selected_index: 0,
            status_message: "Press 'r' to refresh, 'c' to compress, 'd' to decompress, 'q' to quit"
                .to_string(),
            is_busy: false,
        }
    }

    pub async fn refresh(&mut self) -> Result<()> {
        self.is_busy = true;
        self.status_message = "Scanning session files...".to_string();
        match scan_all().await {
            Ok(sessions) => {
                self.sessions = sessions;
                if self.selected_index >= self.sessions.len() && !self.sessions.is_empty() {
                    self.selected_index = self.sessions.len() - 1;
                }
                self.status_message = format!("Discovered {} session files", self.sessions.len());
            }
            Err(e) => {
                self.status_message = format!("Scan error: {}", e);
            }
        }
        self.is_busy = false;
        Ok(())
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
        }

        self.status_message = format!(
            "Decompression complete: {} decompressed, {} skipped, {} failed",
            decompressed, skipped, failed
        );
        self.is_busy = false;
        Ok(())
    }

    pub fn next(&mut self) {
        if !self.sessions.is_empty() {
            if self.selected_index + 1 < self.sessions.len() {
                self.selected_index += 1;
            }
        }
    }

    pub fn previous(&mut self) {
        if !self.sessions.is_empty() {
            if self.selected_index > 0 {
                self.selected_index -= 1;
            }
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
            Constraint::Min(5),    // Session list
            Constraint::Length(3), // Footer & Status
        ])
        .split(inner_area);

    render_summary(f, chunks[0], app);
    render_sessions_list(f, chunks[1], app);
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

fn render_sessions_list(f: &mut Frame, area: Rect, app: &App) {
    let now = SystemTime::now();

    if app.sessions.is_empty() {
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
    for (i, session) in app
        .sessions
        .iter()
        .enumerate()
        .skip(start_index)
        .take(visible_rows)
    {
        let is_selected = i == app.selected_index;
        let prefix = if is_selected { "> " } else { "  " };

        let time_str = format_relative_time(session.modified_at, now);
        let name = session.display_name();

        let mut spans = vec![
            Span::styled(
                format!("{:<7}", session.tool.to_string()),
                if is_selected {
                    Style::default()
                        .fg(Color::Cyan)
                        .add_modifier(Modifier::BOLD)
                } else {
                    Style::default().fg(Color::White)
                },
            ),
            Span::raw(" "),
            Span::styled(
                format!("{:<32}", truncate_str(&name, 32)),
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

        let line_style = if is_selected {
            Style::default().bg(Color::Rgb(30, 40, 60))
        } else {
            Style::default()
        };

        let mut line_spans = vec![Span::raw(prefix)];
        line_spans.extend(spans);
        lines.push(Line::from(line_spans).style(line_style));
    }

    let list_widget = Paragraph::new(lines);
    f.render_widget(list_widget, area);
}

fn render_footer(f: &mut Frame, area: Rect, app: &App) {
    let keybindings = Line::from(vec![
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
