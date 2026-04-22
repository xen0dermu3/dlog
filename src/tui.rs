use std::collections::HashMap;
use std::io;
use std::path::PathBuf;
use std::time::Duration;

use anyhow::Result;
use chrono::{Datelike, Local, NaiveDate, TimeZone};
use crossterm::event::{self, Event, KeyCode, KeyEvent, KeyEventKind};
use crossterm::execute;
use crossterm::terminal::{
    disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen,
};
use ratatui::backend::CrosstermBackend;
use ratatui::layout::{Alignment, Constraint, Direction, Layout};
use ratatui::style::{Color, Modifier, Style, Stylize};
use ratatui::text::{Line, Span, Text};
use ratatui::widgets::{Block, Borders, List, ListItem, ListState, Paragraph, Wrap};
use ratatui::{Frame, Terminal};

use crate::config::Config;
use crate::fuzzy::{self, FuzzyIndex};
use crate::hours::{format_hours, parse_duration};
use crate::report::{self, GroupSummary};
use crate::scanner::{self, CommitRecord};

const FUZZY_LIMIT: usize = 8;

enum View {
    Home,
    Repos,
    RepoAdd { error: Option<String> },
    Date,
    Scanning,
    Results,
    Error(String),
}

struct App {
    config: Config,
    view: View,
    selected_date: NaiveDate,
    today: NaiveDate,

    repos_state: ListState,
    input: String,
    date_cursor: NaiveDate,

    scan_records: Vec<CommitRecord>,
    scan_cache: HashMap<(NaiveDate, Vec<PathBuf>), Vec<CommitRecord>>,
    results_are_cached: bool,
    results_selected: usize,
    results_edit: Option<String>,
    hours_overrides: HashMap<String, f32>,

    fuzzy_index: Option<FuzzyIndex>,
    fuzzy_matches: Vec<(PathBuf, String)>,
    fuzzy_selected: usize,
}

fn cache_key(date: NaiveDate, repos: &[PathBuf]) -> (NaiveDate, Vec<PathBuf>) {
    let mut r = repos.to_vec();
    r.sort();
    (date, r)
}

impl App {
    fn new(config: Config) -> Self {
        let today = Local::now().date_naive();
        let mut repos_state = ListState::default();
        if !config.repos.is_empty() {
            repos_state.select(Some(0));
        }
        Self {
            config,
            view: View::Home,
            selected_date: today,
            today,
            repos_state,
            input: String::new(),
            date_cursor: today,
            scan_records: Vec::new(),
            scan_cache: HashMap::new(),
            results_are_cached: false,
            results_selected: 0,
            results_edit: None,
            hours_overrides: HashMap::new(),
            fuzzy_index: None,
            fuzzy_matches: Vec::new(),
            fuzzy_selected: 0,
        }
    }

    fn refresh_matches(&mut self) {
        if let Some(idx) = &mut self.fuzzy_index {
            let configured: std::collections::HashSet<PathBuf> = self
                .config
                .repos
                .iter()
                .map(|p| p.canonicalize().unwrap_or_else(|_| p.clone()))
                .collect();
            // Oversample so filtering still leaves ~FUZZY_LIMIT visible matches.
            let raw = idx.search(&self.input, FUZZY_LIMIT * 4);
            self.fuzzy_matches = raw
                .into_iter()
                .filter(|(p, _)| {
                    let canon = p.canonicalize().unwrap_or_else(|_| p.clone());
                    !configured.contains(&canon)
                })
                .take(FUZZY_LIMIT)
                .collect();
            if self.fuzzy_selected >= self.fuzzy_matches.len() {
                self.fuzzy_selected = self.fuzzy_matches.len().saturating_sub(1);
            }
        }
    }
}

pub fn run() -> Result<()> {
    let config = Config::load()?;
    let mut app = App::new(config);

    enable_raw_mode()?;
    let mut stdout = io::stdout();
    execute!(stdout, EnterAlternateScreen)?;

    let prev_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let _ = disable_raw_mode();
        let _ = execute!(io::stdout(), LeaveAlternateScreen);
        prev_hook(info);
    }));

    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;

    let result = event_loop(&mut terminal, &mut app);

    disable_raw_mode()?;
    execute!(terminal.backend_mut(), LeaveAlternateScreen)?;
    terminal.show_cursor()?;

    result
}

fn event_loop<B: ratatui::backend::Backend>(
    terminal: &mut Terminal<B>,
    app: &mut App,
) -> Result<()> {
    loop {
        if let Some(idx) = &mut app.fuzzy_index {
            let before = idx.len();
            idx.drain();
            let grew = idx.len() > before;
            if grew && matches!(app.view, View::RepoAdd { .. }) {
                app.refresh_matches();
            }
        }

        terminal.draw(|f| render(f, app))?;

        // If the user just asked to scan, the previous draw showed the
        // Scanning view. Now actually run the scan (which blocks) and the
        // next loop iteration draws the results.
        if matches!(app.view, View::Scanning) {
            run_scan(app);
            continue;
        }

        if event::poll(Duration::from_millis(200))? {
            if let Event::Key(key) = event::read()? {
                if key.kind != KeyEventKind::Press {
                    continue;
                }
                if handle_key(app, key)? {
                    break;
                }
            }
        }
    }
    Ok(())
}

// Returns true if the app should quit.
fn handle_key(app: &mut App, key: KeyEvent) -> Result<bool> {
    match &app.view {
        View::Home => handle_home(app, key),
        View::Repos => handle_repos(app, key),
        View::RepoAdd { .. } => handle_repo_add(app, key),
        View::Date => handle_date(app, key),
        View::Scanning => Ok(false), // no input accepted during scan
        View::Results => handle_results(app, key),
        View::Error(_) => {
            app.view = View::Home;
            Ok(false)
        }
    }
}

fn handle_home(app: &mut App, key: KeyEvent) -> Result<bool> {
    match key.code {
        KeyCode::Char('q') | KeyCode::Esc => return Ok(true),
        KeyCode::Char('r') => app.view = View::Repos,
        KeyCode::Char('d') => {
            app.date_cursor = app.selected_date;
            app.view = View::Date;
        }
        KeyCode::Char('s') => {
            if app.config.repos.is_empty() {
                app.view = View::Error("No repos configured. Press 'r' to add one.".into());
            } else {
                let key = cache_key(app.selected_date, &app.config.repos);
                if let Some(cached) = app.scan_cache.get(&key) {
                    app.scan_records = cached.clone();
                    app.results_are_cached = true;
                    app.results_selected = 0;
                    app.results_edit = None;
                    app.view = View::Results;
                } else {
                    app.view = View::Scanning;
                }
            }
        }
        KeyCode::Char('S') => {
            if app.config.repos.is_empty() {
                app.view = View::Error("No repos configured. Press 'r' to add one.".into());
            } else {
                let key = cache_key(app.selected_date, &app.config.repos);
                app.scan_cache.remove(&key);
                app.hours_overrides.clear();
                app.view = View::Scanning;
            }
        }
        _ => {}
    }
    Ok(false)
}

fn handle_repos(app: &mut App, key: KeyEvent) -> Result<bool> {
    match key.code {
        KeyCode::Esc => app.view = View::Home,
        KeyCode::Char('a') => {
            app.input.clear();
            if app.fuzzy_index.is_none() {
                app.fuzzy_index = Some(FuzzyIndex::new(fuzzy::default_roots()));
            }
            app.fuzzy_selected = 0;
            app.refresh_matches();
            app.view = View::RepoAdd { error: None };
        }
        KeyCode::Char('x') => {
            if let Some(i) = app.repos_state.selected() {
                if i < app.config.repos.len() {
                    app.config.repos.remove(i);
                    if let Err(e) = app.config.save() {
                        app.view = View::Error(format!("save failed: {e:#}"));
                        return Ok(false);
                    }
                    if app.config.repos.is_empty() {
                        app.repos_state.select(None);
                    } else if i >= app.config.repos.len() {
                        app.repos_state.select(Some(app.config.repos.len() - 1));
                    }
                }
            }
        }
        KeyCode::Down | KeyCode::Char('j') => {
            let n = app.config.repos.len();
            if n > 0 {
                let i = app.repos_state.selected().unwrap_or(0);
                app.repos_state.select(Some((i + 1) % n));
            }
        }
        KeyCode::Up | KeyCode::Char('k') => {
            let n = app.config.repos.len();
            if n > 0 {
                let i = app.repos_state.selected().unwrap_or(0);
                app.repos_state.select(Some((i + n - 1) % n));
            }
        }
        _ => {}
    }
    Ok(false)
}

fn handle_repo_add(app: &mut App, key: KeyEvent) -> Result<bool> {
    match key.code {
        KeyCode::Esc => {
            app.input.clear();
            app.view = View::Repos;
        }
        KeyCode::Enter => {
            let path = if !app.fuzzy_matches.is_empty() {
                app.fuzzy_matches[app.fuzzy_selected].0.clone()
            } else {
                let raw = app.input.trim();
                if raw.is_empty() {
                    app.view = View::RepoAdd {
                        error: Some("path is empty".into()),
                    };
                    return Ok(false);
                }
                expand_tilde(raw)
            };
            match validate_repo(&path) {
                Ok(()) => {
                    if !app.config.repos.iter().any(|p| p == &path) {
                        app.config.repos.push(path);
                        if let Err(e) = app.config.save() {
                            app.view = View::Error(format!("save failed: {e:#}"));
                            return Ok(false);
                        }
                    }
                    app.repos_state
                        .select(Some(app.config.repos.len().saturating_sub(1)));
                    app.input.clear();
                    app.fuzzy_matches.clear();
                    app.view = View::Repos;
                }
                Err(e) => {
                    app.view = View::RepoAdd { error: Some(e) };
                }
            }
        }
        KeyCode::Down => {
            if !app.fuzzy_matches.is_empty() {
                app.fuzzy_selected = (app.fuzzy_selected + 1) % app.fuzzy_matches.len();
            }
        }
        KeyCode::Up => {
            if !app.fuzzy_matches.is_empty() {
                let n = app.fuzzy_matches.len();
                app.fuzzy_selected = (app.fuzzy_selected + n - 1) % n;
            }
        }
        KeyCode::Backspace => {
            app.input.pop();
            app.fuzzy_selected = 0;
            app.refresh_matches();
        }
        KeyCode::Char(c) => {
            app.input.push(c);
            app.fuzzy_selected = 0;
            app.refresh_matches();
        }
        _ => {}
    }
    Ok(false)
}

fn handle_date(app: &mut App, key: KeyEvent) -> Result<bool> {
    match key.code {
        KeyCode::Esc => app.view = View::Home,
        KeyCode::Enter => {
            if app.selected_date != app.date_cursor {
                app.hours_overrides.clear();
            }
            app.selected_date = app.date_cursor;
            app.view = View::Home;
        }
        KeyCode::Left | KeyCode::Char('h') => shift_cursor(app, -1),
        KeyCode::Right | KeyCode::Char('l') => shift_cursor(app, 1),
        KeyCode::Up | KeyCode::Char('k') => shift_cursor(app, -7),
        KeyCode::Down | KeyCode::Char('j') => shift_cursor(app, 7),
        KeyCode::Char('[') => shift_month(app, -1),
        KeyCode::Char(']') => shift_month(app, 1),
        KeyCode::Char('t') => app.date_cursor = app.today,
        _ => {}
    }
    Ok(false)
}

fn shift_cursor(app: &mut App, days: i64) {
    if let Some(d) = app
        .date_cursor
        .checked_add_signed(chrono::Duration::days(days))
    {
        app.date_cursor = d;
    }
}

fn shift_month(app: &mut App, delta: i32) {
    let (y, m) = (app.date_cursor.year(), app.date_cursor.month() as i32);
    let total = y * 12 + (m - 1) + delta;
    let new_y = total.div_euclid(12);
    let new_m = (total.rem_euclid(12) + 1) as u32;
    let max_day = days_in_month(new_y, new_m);
    let day = app.date_cursor.day().min(max_day);
    if let Some(d) = NaiveDate::from_ymd_opt(new_y, new_m, day) {
        app.date_cursor = d;
    }
}

fn handle_results(app: &mut App, key: KeyEvent) -> Result<bool> {
    if let Some(buf) = &mut app.results_edit {
        match key.code {
            KeyCode::Esc => {
                app.results_edit = None;
            }
            KeyCode::Enter => {
                if let Some(v) = parse_duration(buf) {
                    let groups = report::group_with_hours(&app.scan_records, None);
                    if let Some(g) = groups.get(app.results_selected) {
                        app.hours_overrides.insert(g.ticket.clone(), v);
                    }
                    app.results_edit = None;
                }
                // Unparseable input keeps the user in edit mode so they can fix it.
            }
            KeyCode::Backspace => {
                buf.pop();
            }
            KeyCode::Char(c)
                if c.is_ascii_digit()
                    || matches!(c, '.' | 'h' | 'H' | 'm' | 'M' | ' ') =>
            {
                buf.push(c);
            }
            _ => {}
        }
        return Ok(false);
    }

    match key.code {
        KeyCode::Esc | KeyCode::Char('q') => {
            app.view = View::Home;
        }
        KeyCode::Down | KeyCode::Char('j') => {
            let n = report::group_with_hours(&app.scan_records, None).len();
            if n > 0 {
                app.results_selected = (app.results_selected + 1) % n;
            }
        }
        KeyCode::Up | KeyCode::Char('k') => {
            let n = report::group_with_hours(&app.scan_records, None).len();
            if n > 0 {
                app.results_selected = (app.results_selected + n - 1) % n;
            }
        }
        KeyCode::Char('e') => {
            app.results_edit = Some(String::new());
        }
        _ => {}
    }
    Ok(false)
}

fn run_scan(app: &mut App) {
    if app.config.repos.is_empty() {
        app.view = View::Error("No repos configured. Press 'r' to add one.".into());
        return;
    }
    let mut all = Vec::new();
    for path in &app.config.repos {
        match scanner::scan(path, Some(app.selected_date)) {
            Ok(mut records) => all.append(&mut records),
            Err(e) => {
                app.view = View::Error(format!("scan {}: {e:#}", path.display()));
                return;
            }
        }
    }
    all.sort_by_key(|r| r.author_time);
    let key = cache_key(app.selected_date, &app.config.repos);
    app.scan_cache.insert(key, all.clone());
    app.scan_records = all;
    app.results_are_cached = false;
    app.results_selected = 0;
    app.results_edit = None;
    app.view = View::Results;
}

fn validate_repo(path: &std::path::Path) -> std::result::Result<(), String> {
    if !path.exists() {
        return Err(format!("path does not exist: {}", path.display()));
    }
    if !path.join(".git").exists() && !path.join("HEAD").exists() {
        return Err(format!("not a git repo: {}", path.display()));
    }
    Ok(())
}

fn expand_tilde(raw: &str) -> PathBuf {
    if let Some(rest) = raw.strip_prefix("~/") {
        if let Some(home) = dirs_home() {
            return home.join(rest);
        }
    }
    PathBuf::from(raw)
}

fn dirs_home() -> Option<PathBuf> {
    directories::BaseDirs::new().map(|b| b.home_dir().to_path_buf())
}

// --- rendering ---------------------------------------------------------

fn render(f: &mut Frame, app: &App) {
    match &app.view {
        View::Home => render_home(f, app),
        View::Repos => render_repos(f, app),
        View::RepoAdd { error } => render_repo_add(f, app, error.as_deref()),
        View::Date => render_date(f, app),
        View::Scanning => render_scanning(f, app),
        View::Results => render_results(f, app),
        View::Error(msg) => render_error(f, msg),
    }
}

fn render_scanning(f: &mut Frame, app: &App) {
    let area = f.area();
    let block = Block::default()
        .title(" scanning ")
        .borders(Borders::ALL);
    let inner = block.inner(area);
    f.render_widget(block, area);

    let n = app.config.repos.len();
    let date = app.selected_date.format("%Y-%m-%d").to_string();
    let mut lines: Vec<Line> = vec![
        Line::from(""),
        Line::from(Span::styled(
            format!(
                "  scanning {n} repo{} for {date}...",
                if n == 1 { "" } else { "s" }
            ),
            Style::default().add_modifier(Modifier::BOLD),
        )),
        Line::from(""),
    ];
    for path in &app.config.repos {
        lines.push(Line::from(Span::styled(
            format!("    {}", path.display()),
            Style::default().fg(Color::DarkGray),
        )));
    }
    f.render_widget(Paragraph::new(Text::from(lines)), inner);
}

fn render_home(f: &mut Frame, app: &App) {
    let area = f.area();
    let block = Block::default()
        .title(" dlog ")
        .borders(Borders::ALL);
    let inner = block.inner(area);
    f.render_widget(block, area);

    let n = app.config.repos.len();
    let lines = vec![
        Line::from(""),
        Line::from(vec![
            Span::raw("  Repos: "),
            Span::styled(n.to_string(), Style::default().add_modifier(Modifier::BOLD)),
            Span::raw(if n == 1 { " configured" } else { " configured" }),
        ]),
        Line::from(vec![
            Span::raw("  Date:  "),
            Span::styled(
                app.selected_date.format("%Y-%m-%d (%a)").to_string(),
                Style::default().add_modifier(Modifier::BOLD),
            ),
        ]),
        Line::from(""),
        Line::from(""),
        Line::from("  [r] edit repos").dim(),
        Line::from("  [d] pick date").dim(),
        Line::from("  [s] scan (cached)   [S] rescan").dim(),
        Line::from("  [q] quit").dim(),
    ];
    let para = Paragraph::new(Text::from(lines));
    f.render_widget(para, inner);
}

fn render_repos(f: &mut Frame, app: &App) {
    let area = f.area();
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Min(0), Constraint::Length(1)])
        .split(area);

    let items: Vec<ListItem> = if app.config.repos.is_empty() {
        vec![ListItem::new(
            Line::from("  (no repos configured — press 'a' to add one)").dim(),
        )]
    } else {
        app.config
            .repos
            .iter()
            .map(|p| ListItem::new(Line::from(format!("  {}", p.display()))))
            .collect()
    };
    let list = List::new(items)
        .block(
            Block::default()
                .title(" repos ")
                .borders(Borders::ALL),
        )
        .highlight_style(
            Style::default()
                .bg(Color::DarkGray)
                .add_modifier(Modifier::BOLD),
        )
        .highlight_symbol("> ");
    let mut state = app.repos_state.clone();
    f.render_stateful_widget(list, chunks[0], &mut state);

    let hint = Paragraph::new(Line::from(
        "[a] add   [x] delete   [↑/↓] move   [Esc] back",
    ))
    .dim();
    f.render_widget(hint, chunks[1]);
}

fn render_repo_add(f: &mut Frame, app: &App, error: Option<&str>) {
    let area = f.area();
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(3), // input
            Constraint::Length(1), // status / error
            Constraint::Min(0),    // matches
            Constraint::Length(1), // hint
        ])
        .split(area);

    let input = Paragraph::new(format!("{}_", app.input)).block(
        Block::default()
            .title(" type to filter (fuzzy) ")
            .borders(Borders::ALL),
    );
    f.render_widget(input, chunks[0]);

    let status_line = if let Some(err) = error {
        Line::from(Span::styled(
            format!("  {err}"),
            Style::default().fg(Color::Red),
        ))
    } else if let Some(idx) = &app.fuzzy_index {
        let n = idx.len();
        let text = if idx.done() {
            format!("  {n} git repo{} indexed", if n == 1 { "" } else { "s" })
        } else {
            format!("  scanning... {n} git repo{} so far", if n == 1 { "" } else { "s" })
        };
        Line::from(Span::styled(text, Style::default().fg(Color::DarkGray)))
    } else {
        Line::from("")
    };
    f.render_widget(Paragraph::new(status_line), chunks[1]);

    let match_area = chunks[2];
    let width = match_area.width.saturating_sub(4) as usize;
    let items: Vec<ListItem> = if app.fuzzy_matches.is_empty() {
        vec![ListItem::new(
            Line::from("  (no matches)").dim(),
        )]
    } else {
        app.fuzzy_matches
            .iter()
            .map(|(_, s)| ListItem::new(Line::from(truncate_left(s, width))))
            .collect()
    };
    let list = List::new(items)
        .block(
            Block::default()
                .title(" matches ")
                .borders(Borders::ALL),
        )
        .highlight_style(
            Style::default()
                .bg(Color::Cyan)
                .fg(Color::Black)
                .add_modifier(Modifier::BOLD),
        )
        .highlight_symbol("> ");
    let mut state = ListState::default();
    if !app.fuzzy_matches.is_empty() {
        state.select(Some(app.fuzzy_selected));
    }
    f.render_stateful_widget(list, match_area, &mut state);

    let hint = Paragraph::new(Line::from(
        "[↑/↓] pick   [Enter] use highlighted   [Esc] cancel",
    ))
    .dim();
    f.render_widget(hint, chunks[3]);
}

fn truncate_left(s: &str, max: usize) -> String {
    if max == 0 {
        return String::new();
    }
    let char_count = s.chars().count();
    if char_count <= max {
        return s.to_string();
    }
    let skip = char_count - (max.saturating_sub(3));
    let tail: String = s.chars().skip(skip).collect();
    format!("...{tail}")
}

fn render_date(f: &mut Frame, app: &App) {
    let area = f.area();
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Min(0), Constraint::Length(1)])
        .split(area);

    let calendar_block = Block::default()
        .title(" pick a date ")
        .borders(Borders::ALL);
    let inner = calendar_block.inner(chunks[0]);
    f.render_widget(calendar_block, chunks[0]);

    let cursor = app.date_cursor;
    let first_of_month = NaiveDate::from_ymd_opt(cursor.year(), cursor.month(), 1).unwrap();
    let offset = first_of_month.weekday().num_days_from_monday() as i64;
    let days = days_in_month(cursor.year(), cursor.month()) as i64;

    let header = Line::from(Span::styled(
        format!("{} {}", month_name(cursor.month()), cursor.year()),
        Style::default().add_modifier(Modifier::BOLD),
    ))
    .alignment(Alignment::Center);
    let labels = Line::from(Span::styled(
        "Mo Tu We Th Fr Sa Su",
        Style::default().fg(Color::DarkGray),
    ))
    .alignment(Alignment::Center);

    let mut lines: Vec<Line> = vec![Line::from(""), header, labels, Line::from("")];

    let mut day = 1i64;
    for _week in 0..6 {
        if day > days {
            break;
        }
        let mut spans: Vec<Span> = Vec::new();
        for wd in 0..7 {
            let idx = _week * 7 + wd;
            if idx < offset || day > days {
                spans.push(Span::raw("   "));
            } else {
                let date =
                    NaiveDate::from_ymd_opt(cursor.year(), cursor.month(), day as u32).unwrap();
                let label = format!("{:>2}", day);
                let mut style = Style::default();
                if date == app.date_cursor {
                    style = style.bg(Color::Cyan).fg(Color::Black);
                } else if date == app.selected_date {
                    style = style.fg(Color::Green).add_modifier(Modifier::BOLD);
                } else if date == app.today {
                    style = style.add_modifier(Modifier::UNDERLINED);
                }
                spans.push(Span::styled(label, style));
                spans.push(Span::raw(" "));
                day += 1;
            }
        }
        lines.push(Line::from(spans).alignment(Alignment::Center));
    }

    let para = Paragraph::new(Text::from(lines));
    f.render_widget(para, inner);

    let hint = Paragraph::new(Line::from(
        "[←/→/↑/↓] move   [ [/] ] month   [t] today   [Enter] select   [Esc] cancel",
    ))
    .dim();
    f.render_widget(hint, chunks[1]);
}

fn render_results(f: &mut Frame, app: &App) {
    let area = f.area();
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Min(0), Constraint::Length(1)])
        .split(area);

    let title = if app.results_are_cached {
        format!(
            " results — {} (cached — shift+S to rescan) ",
            app.selected_date.format("%Y-%m-%d")
        )
    } else {
        format!(" results — {} ", app.selected_date.format("%Y-%m-%d"))
    };
    let block = Block::default().title(title).borders(Borders::ALL);
    let inner = block.inner(chunks[0]);
    f.render_widget(block, chunks[0]);

    let groups = report::group_with_hours(&app.scan_records, None);
    let lines = lines_for_summaries(&groups, &app.scan_records, app);

    let para = Paragraph::new(Text::from(lines)).wrap(Wrap { trim: false });
    f.render_widget(para, inner);

    let hint_text = if app.results_edit.is_some() {
        "type e.g. 30m, 2h, 2h 30m   [Enter] save   [Esc] cancel"
    } else {
        "[↑/↓] select   [e] change time   [Esc] back"
    };
    let hint = Paragraph::new(Line::from(hint_text)).dim();
    f.render_widget(hint, chunks[1]);
}

fn lines_for_summaries(
    groups: &[GroupSummary<'_>],
    all_records: &[CommitRecord],
    app: &App,
) -> Vec<Line<'static>> {
    if all_records.is_empty() {
        return vec![Line::from(Span::styled(
            "  (no matching commits)",
            Style::default().fg(Color::DarkGray),
        ))];
    }
    let repo_w = all_records.iter().map(|r| r.repo.len()).max().unwrap_or(0);
    let mut lines: Vec<Line<'static>> = Vec::new();
    let mut total: f32 = 0.0;

    for (i, g) in groups.iter().enumerate() {
        if i > 0 {
            lines.push(Line::from(""));
        }
        let selected = i == app.results_selected;
        let marker = if selected { "> " } else { "  " };
        let n = g.commits.len();

        let (header_text, value) = if selected && app.results_edit.is_some() {
            let buf = app.results_edit.as_deref().unwrap_or("");
            (
                format!("{marker}{} — edit: {buf}_", g.ticket),
                app.hours_overrides
                    .get(&g.ticket)
                    .copied()
                    .unwrap_or(g.gap.value),
            )
        } else if let Some(ov) = app.hours_overrides.get(&g.ticket) {
            (
                format!(
                    "{marker}{} — {} (manual)",
                    g.ticket,
                    format_hours(*ov)
                ),
                *ov,
            )
        } else {
            (
                format!("{marker}{} — {}", g.ticket, g.gap.display()),
                g.gap.value,
            )
        };
        total += value;

        let header_style = if selected {
            Style::default()
                .bg(Color::DarkGray)
                .fg(Color::White)
                .add_modifier(Modifier::BOLD)
        } else {
            Style::default()
                .fg(Color::Cyan)
                .add_modifier(Modifier::BOLD)
        };
        lines.push(Line::from(Span::styled(header_text, header_style)));

        lines.push(Line::from(Span::styled(
            format!("  {}", report::subtitle(n, g.span.value)),
            Style::default().fg(Color::DarkGray),
        )));

        let body_indent = " ".repeat(2 + 1 + repo_w + 1 + 2 + 5 + 2 + 7 + 2);
        for c in &g.commits {
            let hm = Local
                .timestamp_opt(c.author_time, 0)
                .single()
                .map(|dt| dt.format("%H:%M").to_string())
                .unwrap_or_else(|| "--:--".to_string());
            let short = c.oid[..7.min(c.oid.len())].to_string();
            lines.push(Line::from(format!(
                "  [{:<w$}]  {}  {}  {}",
                c.repo,
                hm,
                short,
                c.subject,
                w = repo_w
            )));
            for body_line in c.body.lines() {
                if body_line.trim().is_empty() {
                    continue;
                }
                lines.push(Line::from(Span::styled(
                    format!("{}{}", body_indent, body_line),
                    Style::default().fg(Color::DarkGray),
                )));
            }
        }
    }

    lines.push(Line::from(""));
    lines.push(Line::from(Span::styled(
        format!("Total: {}", format_hours(total)),
        Style::default()
            .fg(Color::Yellow)
            .add_modifier(Modifier::BOLD),
    )));
    lines
}

fn render_error(f: &mut Frame, msg: &str) {
    let area = f.area();
    let block = Block::default()
        .title(" error ")
        .borders(Borders::ALL)
        .border_style(Style::default().fg(Color::Red));
    let inner = block.inner(area);
    f.render_widget(block, area);
    let lines = vec![
        Line::from(""),
        Line::from(format!("  {msg}")),
        Line::from(""),
        Line::from("  press any key to return").dim(),
    ];
    f.render_widget(Paragraph::new(Text::from(lines)), inner);
}

// --- calendar helpers --------------------------------------------------

fn days_in_month(year: i32, month: u32) -> u32 {
    let next = if month == 12 {
        NaiveDate::from_ymd_opt(year + 1, 1, 1)
    } else {
        NaiveDate::from_ymd_opt(year, month + 1, 1)
    };
    next.unwrap().pred_opt().unwrap().day()
}

fn month_name(m: u32) -> &'static str {
    match m {
        1 => "January",
        2 => "February",
        3 => "March",
        4 => "April",
        5 => "May",
        6 => "June",
        7 => "July",
        8 => "August",
        9 => "September",
        10 => "October",
        11 => "November",
        12 => "December",
        _ => "?",
    }
}
