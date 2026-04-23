use std::collections::{HashMap, HashSet};
use std::io;
use std::path::PathBuf;
use std::time::Duration;

use anyhow::Result;
use chrono::{Datelike, DateTime, FixedOffset, Local, NaiveDate, TimeZone};
use crossterm::event::{self, Event, KeyCode, KeyEvent, KeyEventKind};
use crossterm::execute;
use crossterm::terminal::{
    disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen,
};
use ratatui::backend::CrosstermBackend;
use ratatui::layout::{Alignment, Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style, Stylize};
use ratatui::text::{Line, Span, Text};
use ratatui::widgets::{Block, Borders, Clear, List, ListItem, ListState, Paragraph, Wrap};
use ratatui::{Frame, Terminal};

use crate::config::{Config, JiraConfig};
use crate::fuzzy::{self, FuzzyIndex};
use crate::github::{self, PrInfo};
use crate::hours::{format_hours, parse_duration};
use crate::jira::{self, JiraClient, WorklogOutcome};
use crate::report::{self, GroupSummary};
use crate::scanner::{self, CommitRecord};
use crate::standup::{self, StandupReport};
use crate::store::Store;

const FUZZY_LIMIT: usize = 8;
const LEFT_PANE_WIDTH: u16 = 28;
const LEFT_PANE_WIDTH_ADDING: u16 = 60;
const MIDDLE_PANE_WIDTH: u16 = 30;

#[derive(Clone, Copy, PartialEq, Eq)]
enum Focus {
    Left,
    Middle,
    Right,
}

enum LeftMode {
    Browse,
    AddRepo { error: Option<String> },
}

#[derive(Clone)]
struct PushItem {
    ticket: String,
    seconds: u64,
    started: DateTime<FixedOffset>,
}

enum SetupStep {
    Url,
    Email,
    Token,
}

struct SetupState {
    step: SetupStep,
    url: String,
    email: String,
    token: String,
    error: Option<String>,
    /// True when invoked from `P` (edit existing settings) vs `p` first-run.
    editing: bool,
}

enum PushStage {
    Setup(SetupState),
    Preview {
        items: Vec<PushItem>,
        checks: Vec<bool>,
        cursor: usize,
    },
    Sending {
        queue: Vec<PushItem>,
        done: Vec<String>,
        failed: Vec<(String, String)>,
    },
    Result {
        done: Vec<String>,
        failed: Vec<(String, String)>,
    },
}

struct App {
    config: Config,
    selected_date: NaiveDate,
    range_anchor: Option<NaiveDate>,
    today: NaiveDate,
    cal_month: NaiveDate, // first-of-month currently displayed
    focus: Focus,
    left_mode: LeftMode,
    scanning: bool,
    error: Option<String>,

    // Left pane — repos
    repos_state: ListState,

    // Add-repo state
    input: String,
    fuzzy_index: Option<FuzzyIndex>,
    fuzzy_matches: Vec<(PathBuf, String)>,
    fuzzy_selected: usize,

    // Right pane — results
    scan_records: Vec<CommitRecord>,
    scan_cache: HashMap<((NaiveDate, NaiveDate), Vec<PathBuf>), Vec<CommitRecord>>,
    results_are_cached: bool,
    results_selected: usize,
    results_edit: Option<String>,
    results_scroll: u16,
    hours_overrides: HashMap<String, f32>,

    // Push-to-Jira state
    push: Option<PushStage>,
    jira_client: Option<JiraClient>,
    pushed_this_session: HashSet<String>,

    // Persistent store — None if the DB couldn't be opened (fall back to
    // in-memory only).
    store: Option<Store>,

    // Morning standup overlay — when Some, right pane renders the report.
    standup: Option<StandupReport>,
    standup_scroll: u16,

    // GitHub PR enrichment: OID → PRs that contain it. Populated on scan
    // and (manually) on `G` refresh. Empty when no GitHub repos are
    // configured or `gh` is unavailable.
    pr_index: HashMap<String, Vec<PrInfo>>,
}

impl App {
    fn new(config: Config) -> Self {
        let today = Local::now().date_naive();
        let default_selection = today.pred_opt().unwrap_or(today);
        let mut repos_state = ListState::default();
        if !config.repos.is_empty() {
            repos_state.select(Some(0));
        }
        let focus = if config.repos.is_empty() {
            Focus::Left
        } else {
            Focus::Right
        };
        let cal_month = NaiveDate::from_ymd_opt(
            default_selection.year(),
            default_selection.month(),
            1,
        )
        .unwrap();
        Self {
            config,
            selected_date: default_selection,
            range_anchor: None,
            today,
            cal_month,
            focus,
            left_mode: LeftMode::Browse,
            scanning: false,
            error: None,
            repos_state,
            input: String::new(),
            fuzzy_index: None,
            fuzzy_matches: Vec::new(),
            fuzzy_selected: 0,
            scan_records: Vec::new(),
            scan_cache: HashMap::new(),
            results_are_cached: false,
            results_selected: 0,
            results_edit: None,
            results_scroll: 0,
            hours_overrides: HashMap::new(),
            push: None,
            jira_client: None,
            pushed_this_session: HashSet::new(),
            store: Store::open().ok(),
            standup: None,
            standup_scroll: 0,
            pr_index: HashMap::new(),
        }
    }

    /// Replace `pushed_this_session` with the set of tickets that already
    /// have a worklog recorded in the on-disk store for the current range.
    fn sync_pushed_from_store(&mut self) {
        if let Some(store) = &self.store {
            let (start, end) = self.current_range();
            if let Ok(set) = store.worklogs_for_range(start, end) {
                self.pushed_this_session = set;
            }
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

    fn is_adding(&self) -> bool {
        matches!(self.left_mode, LeftMode::AddRepo { .. })
    }

    fn current_range(&self) -> (NaiveDate, NaiveDate) {
        match self.range_anchor {
            Some(a) if a <= self.selected_date => (a, self.selected_date),
            Some(a) => (self.selected_date, a),
            None => (self.selected_date, self.selected_date),
        }
    }
}

fn cache_key(
    start: NaiveDate,
    end: NaiveDate,
    repos: &[PathBuf],
) -> ((NaiveDate, NaiveDate), Vec<PathBuf>) {
    let mut r = repos.to_vec();
    r.sort();
    ((start, end), r)
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
            if idx.len() > before && app.is_adding() {
                app.refresh_matches();
            }
        }

        terminal.draw(|f| render(f, app))?;

        // Skeleton was just drawn; now actually scan.
        if app.scanning {
            run_scan(app);
            continue;
        }

        if matches!(app.push, Some(PushStage::Sending { .. })) {
            step_push(app);
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

// ---------- key handling ---------------------------------------------------

fn handle_key(app: &mut App, key: KeyEvent) -> Result<bool> {
    if app.error.is_some() {
        app.error = None;
        return Ok(false);
    }
    if app.scanning {
        return Ok(false);
    }
    if app.push.is_some() {
        handle_push(app, key);
        return Ok(false);
    }
    if app.standup.is_some() {
        handle_standup(app, key);
        return Ok(false);
    }
    if app.is_adding() {
        handle_add_repo(app, key);
        return Ok(false);
    }
    if app.focus == Focus::Right && app.results_edit.is_some() {
        handle_edit_hours(app, key);
        return Ok(false);
    }

    match key.code {
        KeyCode::Char('q') | KeyCode::Esc => return Ok(true),
        KeyCode::Tab => {
            app.focus = match app.focus {
                Focus::Left => Focus::Middle,
                Focus::Middle => Focus::Right,
                Focus::Right => Focus::Left,
            };
            return Ok(false);
        }
        KeyCode::Char('s') => {
            do_scan(app, false);
            return Ok(false);
        }
        KeyCode::Char('S') => {
            do_scan(app, true);
            return Ok(false);
        }
        KeyCode::Char('p') => {
            start_push(app);
            return Ok(false);
        }
        KeyCode::Char('e') => {
            if !app.scan_records.is_empty() {
                app.focus = Focus::Right;
                app.results_edit = Some(String::new());
            }
            return Ok(false);
        }
        KeyCode::Char('J') => {
            open_jira_settings(app);
            return Ok(false);
        }
        KeyCode::Char('m') => {
            open_standup(app);
            return Ok(false);
        }
        KeyCode::Char('G') => {
            refresh_github_prs(app);
            return Ok(false);
        }
        _ => {}
    }

    match app.focus {
        Focus::Left => handle_left(app, key),
        Focus::Middle => handle_middle(app, key),
        Focus::Right => handle_right(app, key),
    }
    Ok(false)
}

fn handle_left(app: &mut App, key: KeyEvent) {
    match key.code {
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
        KeyCode::Char('a') => {
            app.input.clear();
            if app.fuzzy_index.is_none() {
                app.fuzzy_index = Some(FuzzyIndex::new(fuzzy::default_roots()));
            }
            app.fuzzy_selected = 0;
            app.refresh_matches();
            app.left_mode = LeftMode::AddRepo { error: None };
            app.focus = Focus::Left;
        }
        KeyCode::Char('x') => {
            if let Some(i) = app.repos_state.selected() {
                if i < app.config.repos.len() {
                    app.config.repos.remove(i);
                    if let Err(e) = app.config.save() {
                        app.error = Some(format!("save failed: {e:#}"));
                        return;
                    }
                    if app.config.repos.is_empty() {
                        app.repos_state.select(None);
                    } else if i >= app.config.repos.len() {
                        app.repos_state.select(Some(app.config.repos.len() - 1));
                    }
                }
            }
        }
        _ => {}
    }
}

fn handle_middle(app: &mut App, key: KeyEvent) {
    match key.code {
        KeyCode::Left | KeyCode::Char('h') => move_cursor(app, -1),
        KeyCode::Right | KeyCode::Char('l') => move_cursor(app, 1),
        KeyCode::Up | KeyCode::Char('k') => move_cursor(app, -7),
        KeyCode::Down | KeyCode::Char('j') => move_cursor(app, 7),
        KeyCode::Char('[') => shift_month(app, -1),
        KeyCode::Char(']') => shift_month(app, 1),
        KeyCode::Char('t') => {
            app.selected_date = app.today;
            app.cal_month = first_of_month(app.today);
        }
        KeyCode::Char('y') => {
            if let Some(yesterday) = app.today.pred_opt() {
                app.selected_date = yesterday;
                app.cal_month = first_of_month(yesterday);
            }
        }
        KeyCode::Char(' ') | KeyCode::Char('r') => {
            if app.range_anchor == Some(app.selected_date) {
                // Toggle off if anchor is on the same day as cursor.
                app.range_anchor = None;
            } else if app.range_anchor.is_some() {
                // Second press anywhere else: clear so the user can start again.
                app.range_anchor = None;
            } else {
                app.range_anchor = Some(app.selected_date);
            }
            app.hours_overrides.clear();
        }
        KeyCode::Enter => {
            do_scan(app, false);
        }
        _ => {}
    }
}

fn handle_right(app: &mut App, key: KeyEvent) {
    match key.code {
        KeyCode::Down | KeyCode::Char('j') => {
            let n = report::group_with_hours(&app.scan_records).len();
            if n > 0 {
                app.results_selected = (app.results_selected + 1) % n;
            }
        }
        KeyCode::Up | KeyCode::Char('k') => {
            let n = report::group_with_hours(&app.scan_records).len();
            if n > 0 {
                app.results_selected = (app.results_selected + n - 1) % n;
            }
        }
        KeyCode::PageDown => {
            app.results_scroll = app.results_scroll.saturating_add(10);
        }
        KeyCode::PageUp => {
            app.results_scroll = app.results_scroll.saturating_sub(10);
        }
        _ => {}
    }
}

fn handle_add_repo(app: &mut App, key: KeyEvent) {
    match key.code {
        KeyCode::Esc => {
            app.input.clear();
            app.fuzzy_matches.clear();
            app.left_mode = LeftMode::Browse;
        }
        KeyCode::Enter => {
            let path = if !app.fuzzy_matches.is_empty() {
                app.fuzzy_matches[app.fuzzy_selected].0.clone()
            } else {
                let raw = app.input.trim();
                if raw.is_empty() {
                    app.left_mode = LeftMode::AddRepo {
                        error: Some("path is empty".into()),
                    };
                    return;
                }
                expand_tilde(raw)
            };
            match validate_repo(&path) {
                Ok(()) => {
                    if !app.config.repos.iter().any(|p| p == &path) {
                        app.config.repos.push(path);
                        if let Err(e) = app.config.save() {
                            app.error = Some(format!("save failed: {e:#}"));
                            return;
                        }
                    }
                    app.repos_state
                        .select(Some(app.config.repos.len().saturating_sub(1)));
                    // Stay in add-mode so the user can queue more repos.
                    // Clear the query, refresh matches (the just-added repo
                    // falls out of the list via the already-configured filter).
                    app.input.clear();
                    app.fuzzy_selected = 0;
                    app.refresh_matches();
                    app.left_mode = LeftMode::AddRepo { error: None };
                }
                Err(e) => {
                    app.left_mode = LeftMode::AddRepo { error: Some(e) };
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
}

fn handle_edit_hours(app: &mut App, key: KeyEvent) {
    let Some(buf) = &mut app.results_edit else {
        return;
    };
    match key.code {
        KeyCode::Esc => {
            app.results_edit = None;
        }
        KeyCode::Enter => {
            if let Some(v) = parse_duration(buf) {
                let groups = report::group_with_hours(&app.scan_records);
                if let Some(g) = groups.get(app.results_selected) {
                    app.hours_overrides.insert(g.ticket.clone(), v);
                }
                app.results_edit = None;
            }
        }
        KeyCode::Backspace => {
            buf.pop();
        }
        KeyCode::Char(c)
            if c.is_ascii_digit() || matches!(c, '.' | 'h' | 'H' | 'm' | 'M' | ' ') =>
        {
            buf.push(c);
        }
        _ => {}
    }
}

// ---------- actions -------------------------------------------------------

fn move_cursor(app: &mut App, days: i64) {
    if let Some(d) = app
        .selected_date
        .checked_add_signed(chrono::Duration::days(days))
    {
        app.selected_date = d;
        // Keep the calendar viewing the cursor's month.
        app.cal_month = first_of_month(d);
    }
}

fn shift_month(app: &mut App, delta: i32) {
    let (y, m) = (app.cal_month.year(), app.cal_month.month() as i32);
    let total = y * 12 + (m - 1) + delta;
    let new_y = total.div_euclid(12);
    let new_m = (total.rem_euclid(12) + 1) as u32;
    if let Some(d) = NaiveDate::from_ymd_opt(new_y, new_m, 1) {
        app.cal_month = d;
    }
}

fn do_scan(app: &mut App, force: bool) {
    if app.config.repos.is_empty() {
        app.error = Some("No repos configured. Focus the left pane and press 'a'.".into());
        return;
    }
    let (start, end) = app.current_range();
    let key = cache_key(start, end, &app.config.repos);

    if force {
        app.scan_cache.remove(&key);
        if let Some(store) = &app.store {
            let _ = store.clear_scan(start, end, &app.config.repos);
        }
        app.hours_overrides.clear();
    } else {
        // 1. In-memory cache hit.
        if let Some(cached) = app.scan_cache.get(&key) {
            app.scan_records = cached.clone();
            app.results_are_cached = true;
            app.results_selected = 0;
            app.results_edit = None;
            app.results_scroll = 0;
            app.focus = Focus::Right;
            app.sync_pushed_from_store();
            return;
        }
        // 2. On-disk cache hit — warm the in-memory cache so subsequent
        //    scans don't re-hit SQLite.
        if let Some(store) = &app.store {
            if let Ok(Some(records)) = store.load_scan(start, end, &app.config.repos) {
                app.scan_cache.insert(key, records.clone());
                app.scan_records = records;
                app.results_are_cached = true;
                app.results_selected = 0;
                app.results_edit = None;
                app.results_scroll = 0;
                app.focus = Focus::Right;
                app.sync_pushed_from_store();
                return;
            }
        }
    }
    // 3. No cache — trigger a fresh scan.
    app.scanning = true;
    app.focus = Focus::Right;
}

// ---------- push flow -----------------------------------------------------

fn build_pr_index(repos: &[PathBuf]) -> HashMap<String, Vec<PrInfo>> {
    let mut idx: HashMap<String, Vec<PrInfo>> = HashMap::new();
    for path in repos {
        if github::is_github_repo(path) {
            for pr in github::fetch_prs(path) {
                for oid in &pr.commit_oids {
                    idx.entry(oid.clone()).or_default().push(pr.clone());
                }
            }
        }
    }
    idx
}

fn enrich_records_with_prs(
    records: &mut Vec<CommitRecord>,
    pr_index: &HashMap<String, Vec<PrInfo>>,
) {
    for r in records.iter_mut() {
        let Some(prs) = pr_index.get(&r.oid) else {
            continue;
        };
        let extra: String = prs
            .iter()
            .flat_map(|pr| [pr.title.as_str(), pr.body.as_str(), pr.head_branch.as_str()])
            .collect::<Vec<_>>()
            .join(" ");
        if extra.is_empty() {
            continue;
        }
        if r.branches.is_empty() {
            r.branches = extra;
        } else {
            r.branches.push(' ');
            r.branches.push_str(&extra);
        }
    }
}

fn refresh_github_prs(app: &mut App) {
    let idx = build_pr_index(&app.config.repos);
    enrich_records_with_prs(&mut app.scan_records, &idx);
    app.pr_index = idx;
}

fn open_standup(app: &mut App) {
    match standup::build(&app.config, app.today) {
        Ok(report) => {
            app.standup = Some(report);
            app.standup_scroll = 0;
            app.focus = Focus::Right;
        }
        Err(e) => {
            app.error = Some(format!("standup: {e:#}"));
        }
    }
}

fn handle_standup(app: &mut App, key: KeyEvent) {
    match key.code {
        KeyCode::Esc | KeyCode::Char('q') => {
            app.standup = None;
            app.standup_scroll = 0;
        }
        KeyCode::PageDown => {
            app.standup_scroll = app.standup_scroll.saturating_add(10);
        }
        KeyCode::PageUp => {
            app.standup_scroll = app.standup_scroll.saturating_sub(10);
        }
        _ => {}
    }
}

fn open_jira_settings(app: &mut App) {
    let (url, email) = match &app.config.jira {
        Some(c) => (c.base_url.clone(), c.email.clone()),
        None => (String::new(), String::new()),
    };
    app.push = Some(PushStage::Setup(SetupState {
        step: SetupStep::Url,
        url,
        email,
        token: String::new(),
        error: None,
        editing: true,
    }));
}

fn start_push(app: &mut App) {
    if app.scan_records.is_empty() {
        app.error = Some("Nothing to push — run a scan first.".into());
        return;
    }
    match &app.config.jira {
        None => {
            app.push = Some(PushStage::Setup(SetupState {
                step: SetupStep::Url,
                url: String::new(),
                email: String::new(),
                token: String::new(),
                error: None,
                editing: false,
            }));
        }
        Some(cfg) => {
            if app.jira_client.is_none() {
                match JiraClient::from_config(cfg) {
                    Ok(client) => app.jira_client = Some(client),
                    Err(e) => {
                        app.error = Some(format!("Jira: {e:#}"));
                        return;
                    }
                }
            }
            open_preview(app);
        }
    }
}

fn open_preview(app: &mut App) {
    let items = build_push_items(app);
    if items.is_empty() {
        app.error = Some(
            "Nothing to push — all tickets round to 0 minutes or are untagged.".into(),
        );
        return;
    }
    let checks = items
        .iter()
        .map(|it| !app.pushed_this_session.contains(&it.ticket))
        .collect();
    app.push = Some(PushStage::Preview {
        items,
        checks,
        cursor: 0,
    });
}

fn build_push_items(app: &App) -> Vec<PushItem> {
    let groups = report::group_with_hours(&app.scan_records);
    groups
        .into_iter()
        .filter(|g| g.ticket != report::UNTAGGED)
        .filter_map(|g| {
            let hours = app
                .hours_overrides
                .get(&g.ticket)
                .copied()
                .unwrap_or(g.gap.value);
            let seconds = jira::hours_to_jira_seconds(hours);
            if seconds == 0 {
                return None;
            }
            let first_commit = g.commits.iter().min_by_key(|c| c.author_time)?;
            let local = Local.timestamp_opt(first_commit.author_time, 0).single()?;
            Some(PushItem {
                ticket: g.ticket,
                seconds,
                started: local.fixed_offset(),
            })
        })
        .collect()
}

fn handle_push(app: &mut App, key: KeyEvent) {
    match &mut app.push {
        Some(PushStage::Setup(_)) => handle_push_setup(app, key),
        Some(PushStage::Preview { .. }) => handle_push_preview(app, key),
        Some(PushStage::Sending { .. }) => {
            // Sending ignores input; progress is driven by event_loop.
        }
        Some(PushStage::Result { .. }) => {
            if matches!(key.code, KeyCode::Esc | KeyCode::Enter | KeyCode::Char('q')) {
                app.push = None;
            }
        }
        None => {}
    }
}

fn handle_push_setup(app: &mut App, key: KeyEvent) {
    let Some(PushStage::Setup(state)) = &mut app.push else {
        return;
    };
    match key.code {
        KeyCode::Esc => {
            app.push = None;
        }
        KeyCode::Enter => {
            match state.step {
                SetupStep::Url => {
                    let v = state.url.trim();
                    if !(v.starts_with("http://") || v.starts_with("https://")) {
                        state.error = Some("URL must start with http:// or https://".into());
                        return;
                    }
                    state.url = v.to_string();
                    state.step = SetupStep::Email;
                    state.error = None;
                }
                SetupStep::Email => {
                    let v = state.email.trim();
                    if !v.contains('@') {
                        state.error = Some("not an email address".into());
                        return;
                    }
                    state.email = v.to_string();
                    state.step = SetupStep::Token;
                    state.error = None;
                }
                SetupStep::Token => {
                    if state.token.is_empty() {
                        if !state.editing {
                            state.error = Some("token is empty".into());
                            return;
                        }
                        // Editing + empty token = keep existing token in Keychain.
                    } else if let Err(e) = jira::save_token(&state.email, &state.token) {
                        state.error = Some(format!("keyring: {e:#}"));
                        return;
                    }
                    let cfg = JiraConfig {
                        base_url: state.url.clone(),
                        email: state.email.clone(),
                    };
                    let editing = state.editing;
                    app.config.jira = Some(cfg.clone());
                    if let Err(e) = app.config.save() {
                        app.error = Some(format!("save config: {e:#}"));
                        app.push = None;
                        return;
                    }
                    match JiraClient::from_config(&cfg) {
                        Ok(client) => {
                            app.jira_client = Some(client);
                            if editing {
                                app.push = None;
                            } else {
                                open_preview(app);
                            }
                        }
                        Err(e) => {
                            app.error = Some(format!("Jira: {e:#}"));
                            app.push = None;
                        }
                    }
                }
            }
        }
        KeyCode::Backspace => match state.step {
            SetupStep::Url => {
                state.url.pop();
            }
            SetupStep::Email => {
                state.email.pop();
            }
            SetupStep::Token => {
                state.token.pop();
            }
        },
        KeyCode::Char(c) => match state.step {
            SetupStep::Url => state.url.push(c),
            SetupStep::Email => state.email.push(c),
            SetupStep::Token => state.token.push(c),
        },
        _ => {}
    }
}

fn handle_push_preview(app: &mut App, key: KeyEvent) {
    let Some(PushStage::Preview {
        items,
        checks,
        cursor,
    }) = &mut app.push
    else {
        return;
    };
    match key.code {
        KeyCode::Esc => {
            app.push = None;
        }
        KeyCode::Up | KeyCode::Char('k') => {
            if !items.is_empty() {
                *cursor = (*cursor + items.len() - 1) % items.len();
            }
        }
        KeyCode::Down | KeyCode::Char('j') => {
            if !items.is_empty() {
                *cursor = (*cursor + 1) % items.len();
            }
        }
        KeyCode::Char(' ') => {
            if let Some(c) = checks.get_mut(*cursor) {
                *c = !*c;
            }
        }
        KeyCode::Enter => {
            let queue: Vec<PushItem> = items
                .iter()
                .zip(checks.iter())
                .filter(|(_, c)| **c)
                .map(|(it, _)| it.clone())
                .collect();
            if queue.is_empty() {
                // No-op; stay in preview.
                return;
            }
            app.push = Some(PushStage::Sending {
                queue,
                done: Vec::new(),
                failed: Vec::new(),
            });
        }
        _ => {}
    }
}

fn step_push(app: &mut App) {
    // Scope the initial mutable borrow so we can touch other app fields below.
    let item = {
        let Some(PushStage::Sending {
            queue,
            done,
            failed,
        }) = &mut app.push
        else {
            return;
        };
        if queue.is_empty() {
            let finished_done = std::mem::take(done);
            let finished_failed = std::mem::take(failed);
            app.push = Some(PushStage::Result {
                done: finished_done,
                failed: finished_failed,
            });
            return;
        }
        queue.remove(0)
    };

    let outcome = {
        let Some(client) = &app.jira_client else {
            app.error = Some("Jira client missing.".into());
            app.push = None;
            return;
        };
        client.post_worklog(&item.ticket, item.started, item.seconds)
    };

    match outcome {
        Ok(WorklogOutcome::Ok { worklog_id }) => {
            app.pushed_this_session.insert(item.ticket.clone());
            let (start, end) = app.current_range();
            if let Some(store) = &app.store {
                let _ = store.record_worklog(&item.ticket, start, end, &worklog_id);
            }
            if let Some(PushStage::Sending { done, .. }) = &mut app.push {
                done.push(item.ticket);
            }
        }
        Ok(WorklogOutcome::Err { status, message }) => {
            if let Some(PushStage::Sending { failed, .. }) = &mut app.push {
                failed.push((item.ticket, format!("HTTP {status}: {message}")));
            }
        }
        Err(e) => {
            if let Some(PushStage::Sending { failed, .. }) = &mut app.push {
                failed.push((item.ticket, format!("{e:#}")));
            }
        }
    }
}

fn run_scan(app: &mut App) {
    let (start, end) = app.current_range();
    let mut all = Vec::new();
    for path in &app.config.repos {
        match scanner::scan(path, start, end) {
            Ok(mut records) => all.append(&mut records),
            Err(e) => {
                app.error = Some(format!("scan {}: {e:#}", path.display()));
                app.scanning = false;
                return;
            }
        }
    }
    all.sort_by_key(|r| r.author_time);

    // Pull GitHub PR metadata for all configured github repos, then enrich
    // each commit's `branches` string with PR title/body/head-branch text so
    // ticket extraction catches keys that only appear in the PR.
    let pr_index = build_pr_index(&app.config.repos);
    enrich_records_with_prs(&mut all, &pr_index);

    let key = cache_key(start, end, &app.config.repos);
    app.scan_cache.insert(key, all.clone());
    if let Some(store) = &app.store {
        let _ = store.save_scan(start, end, &app.config.repos, &all);
    }
    app.scan_records = all;
    app.pr_index = pr_index;
    app.results_are_cached = false;
    app.results_selected = 0;
    app.results_edit = None;
    app.results_scroll = 0;
    app.scanning = false;
    app.sync_pushed_from_store();
}

// ---------- helpers -------------------------------------------------------

fn first_of_month(d: NaiveDate) -> NaiveDate {
    NaiveDate::from_ymd_opt(d.year(), d.month(), 1).unwrap()
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

fn center_rect(area: Rect, w: u16, h: u16) -> Rect {
    let w = w.min(area.width);
    let h = h.min(area.height);
    Rect {
        x: area.x + (area.width - w) / 2,
        y: area.y + (area.height - h) / 2,
        width: w,
        height: h,
    }
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

fn focused_border(focus: Focus, target: Focus) -> Style {
    if focus == target {
        Style::default().fg(Color::Yellow)
    } else {
        Style::default().fg(Color::DarkGray)
    }
}

// ---------- rendering -----------------------------------------------------

fn render(f: &mut Frame, app: &mut App) {
    render_layout(f, f.area(), app);
    if let Some(msg) = &app.error {
        render_error_popup(f, msg);
    }
}

fn render_layout(f: &mut Frame, area: Rect, app: &mut App) {
    let outer = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Min(0), Constraint::Length(1)])
        .split(area);

    if app.is_adding() {
        // Hide the calendar column while adding so the left pane can grow
        // to show full paths, and the right pane keeps results in view.
        let panes = Layout::default()
            .direction(Direction::Horizontal)
            .constraints([
                Constraint::Length(LEFT_PANE_WIDTH_ADDING),
                Constraint::Min(0),
            ])
            .split(outer[0]);
        render_left_pane(f, panes[0], app);
        render_right_pane(f, panes[1], app);
    } else {
        let panes = Layout::default()
            .direction(Direction::Horizontal)
            .constraints([
                Constraint::Length(LEFT_PANE_WIDTH),
                Constraint::Length(MIDDLE_PANE_WIDTH),
                Constraint::Min(0),
            ])
            .split(outer[0]);
        render_left_pane(f, panes[0], app);
        render_middle_pane(f, panes[1], app);
        render_right_pane(f, panes[2], app);
    }
    render_hint_bar(f, outer[1], app);
}

// ---------- left pane (repos) --------------------------------------------

fn render_left_pane(f: &mut Frame, area: Rect, app: &App) {
    match &app.left_mode {
        LeftMode::Browse => render_left_browse(f, area, app),
        LeftMode::AddRepo { error } => render_left_add(f, area, app, error.as_deref()),
    }
}

fn render_left_browse(f: &mut Frame, area: Rect, app: &App) {
    let items: Vec<ListItem> = if app.config.repos.is_empty() {
        vec![ListItem::new(Line::from("  (press `a` to add)").dim())]
    } else {
        let width = area.width.saturating_sub(4) as usize;
        app.config
            .repos
            .iter()
            .map(|p| {
                let name = p
                    .file_name()
                    .map(|n| n.to_string_lossy().into_owned())
                    .unwrap_or_else(|| p.display().to_string());
                ListItem::new(Line::from(truncate_left(&name, width)))
            })
            .collect()
    };
    let list = List::new(items)
        .block(
            Block::default()
                .title(format!(" repos ({}) ", app.config.repos.len()))
                .borders(Borders::ALL)
                .border_style(focused_border(app.focus, Focus::Left)),
        )
        .highlight_style(
            Style::default()
                .bg(Color::DarkGray)
                .add_modifier(Modifier::BOLD),
        )
        .highlight_symbol("> ");
    let mut state = app.repos_state.clone();
    f.render_stateful_widget(list, area, &mut state);
}

fn render_left_add(f: &mut Frame, area: Rect, app: &App, error: Option<&str>) {
    let block = Block::default()
        .title(" add repo ")
        .borders(Borders::ALL)
        .border_style(focused_border(app.focus, Focus::Left));
    let inner = block.inner(area);
    f.render_widget(block, area);

    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(3),
            Constraint::Length(1),
            Constraint::Min(0),
        ])
        .split(inner);

    let input_width = chunks[0].width.saturating_sub(4) as usize;
    let displayed = truncate_left(&format!("{}_", app.input), input_width);
    let input = Paragraph::new(displayed).block(
        Block::default()
            .title(" search ")
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
            format!("  {n} git repo{}", if n == 1 { "" } else { "s" })
        } else {
            format!("  scanning... {n} so far")
        };
        Line::from(Span::styled(text, Style::default().fg(Color::DarkGray)))
    } else {
        Line::from("")
    };
    f.render_widget(Paragraph::new(status_line), chunks[1]);

    let matches_width = chunks[2].width.saturating_sub(4) as usize;
    let items: Vec<ListItem> = if app.fuzzy_matches.is_empty() {
        vec![ListItem::new(Line::from("  (no matches)").dim())]
    } else {
        app.fuzzy_matches
            .iter()
            .map(|(_, s)| ListItem::new(Line::from(truncate_left(s, matches_width))))
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
    f.render_stateful_widget(list, chunks[2], &mut state);
}

// ---------- middle pane (calendar) ---------------------------------------

fn render_middle_pane(f: &mut Frame, area: Rect, app: &App) {
    let (range_start, range_end) = app.current_range();
    let ranging = app.range_anchor.is_some();

    let title = if ranging {
        let days = (range_end - range_start).num_days() + 1;
        format!(" date — range: {} day{} ", days, if days == 1 { "" } else { "s" })
    } else {
        " date ".to_string()
    };

    let block = Block::default()
        .title(title)
        .borders(Borders::ALL)
        .border_style(focused_border(app.focus, Focus::Middle));
    let inner = block.inner(area);
    f.render_widget(block, area);

    let month = app.cal_month;
    let first_weekday_offset = month.weekday().num_days_from_monday() as i64;
    let n_days = days_in_month(month.year(), month.month()) as i64;

    let mut lines: Vec<Line> = Vec::new();
    // Header: "April 2026"
    lines.push(Line::from(""));
    lines.push(
        Line::from(Span::styled(
            format!("{} {}", month_name(month.month()), month.year()),
            Style::default().add_modifier(Modifier::BOLD),
        ))
        .alignment(Alignment::Center),
    );
    lines.push(
        Line::from(Span::styled(
            "Mo Tu We Th Fr Sa Su",
            Style::default().fg(Color::DarkGray),
        ))
        .alignment(Alignment::Center),
    );

    // Day grid.
    let mut day = 1i64;
    for _week in 0..6 {
        if day > n_days {
            break;
        }
        let mut spans: Vec<Span> = Vec::new();
        for wd in 0..7 {
            let idx = _week * 7 + wd;
            if idx < first_weekday_offset || day > n_days {
                spans.push(Span::raw("   "));
            } else {
                let date =
                    NaiveDate::from_ymd_opt(month.year(), month.month(), day as u32).unwrap();
                let is_cursor = date == app.selected_date;
                let is_anchor = Some(date) == app.range_anchor && !is_cursor;
                let in_range = ranging && date >= range_start && date <= range_end;
                let is_today = date == app.today;

                let mut style = Style::default();
                if is_cursor {
                    style = style
                        .bg(Color::Cyan)
                        .fg(Color::Black)
                        .add_modifier(Modifier::BOLD);
                } else if is_anchor {
                    style = style
                        .bg(Color::Green)
                        .fg(Color::Black)
                        .add_modifier(Modifier::BOLD);
                } else if in_range {
                    style = style.bg(Color::DarkGray).fg(Color::White);
                } else if is_today {
                    style = style.add_modifier(Modifier::UNDERLINED);
                }
                spans.push(Span::styled(format!("{:>2}", day), style));
                spans.push(Span::raw(" "));
                day += 1;
            }
        }
        lines.push(Line::from(spans).alignment(Alignment::Center));
    }

    // Footer: selection summary.
    lines.push(Line::from(""));
    let selection = if ranging {
        format!(
            "  {} → {}",
            range_start.format("%Y-%m-%d"),
            range_end.format("%Y-%m-%d")
        )
    } else {
        format!("  {}", app.selected_date.format("%Y-%m-%d (%a)"))
    };
    lines.push(Line::from(Span::styled(
        selection,
        Style::default()
            .fg(if ranging { Color::Green } else { Color::Cyan })
            .add_modifier(Modifier::BOLD),
    )));
    if app.results_are_cached {
        lines.push(Line::from(Span::styled(
            "  cached — press S to rescan",
            Style::default().fg(Color::DarkGray),
        )));
    }

    f.render_widget(Paragraph::new(Text::from(lines)), inner);
}

// ---------- right pane (results) -----------------------------------------

fn render_right_pane(f: &mut Frame, area: Rect, app: &mut App) {
    let title = if app.standup.is_some() {
        " standup "
    } else {
        match &app.push {
            Some(PushStage::Setup(s)) if s.editing => " Jira settings ",
            Some(PushStage::Setup(_)) => " push to Jira — setup ",
            Some(PushStage::Preview { .. }) => " push to Jira — preview ",
            Some(PushStage::Sending { .. }) => " push to Jira — sending ",
            Some(PushStage::Result { .. }) => " push to Jira — done ",
            None => " results ",
        }
    };
    let border_color = if app.standup.is_some() {
        Color::Blue
    } else if app.push.is_some() {
        Color::Magenta
    } else if app.focus == Focus::Right {
        Color::Yellow
    } else {
        Color::DarkGray
    };
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(border_color))
        .title(title);
    let inner = block.inner(area);
    f.render_widget(block, area);

    if let Some(stage) = &app.push {
        render_push(f, inner, stage);
        return;
    }

    if let Some(report) = &app.standup {
        render_standup(f, inner, report, app.standup_scroll);
        return;
    }

    if app.scanning {
        render_scanning_skeleton(f, inner, app);
        return;
    }
    if app.config.repos.is_empty() {
        let msg = Paragraph::new(Text::from(vec![
            Line::from(""),
            Line::from("  Add at least one repo on the left, then press `s`.").dim(),
        ]));
        f.render_widget(msg, inner);
        return;
    }
    if app.scan_records.is_empty() && !app.results_are_cached {
        let n = app.config.repos.len();
        let (s, e) = app.current_range();
        let range_desc = if s == e {
            s.format("%Y-%m-%d").to_string()
        } else {
            format!("{} → {}", s.format("%Y-%m-%d"), e.format("%Y-%m-%d"))
        };
        let msg = Paragraph::new(Text::from(vec![
            Line::from(""),
            Line::from(Span::styled(
                format!(
                    "  Press `s` to scan {} repo{} for {}.",
                    n,
                    if n == 1 { "" } else { "s" },
                    range_desc
                ),
                Style::default().fg(Color::DarkGray),
            )),
        ]));
        f.render_widget(msg, inner);
        return;
    }

    let groups = report::group_with_hours(&app.scan_records);
    let (lines, group_line_offsets) = lines_for_summaries(&groups, &app.scan_records, app);

    // Auto-scroll: if the selected group's header is outside the visible
    // area, snap it to the top so its commit rows are visible below. If it's
    // already in view, leave the scroll alone (don't jar on minor nav).
    let visible = inner.height;
    if let Some(&sel_line) = group_line_offsets.get(app.results_selected) {
        let top = app.results_scroll as usize;
        let sel = sel_line as usize;
        let in_view = sel >= top && sel < top + visible as usize;
        if !in_view {
            app.results_scroll = sel_line;
        }
    }
    // Clamp so we don't scroll past the bottom of the content.
    let max_scroll = (lines.len() as u16).saturating_sub(visible.max(1));
    if app.results_scroll > max_scroll {
        app.results_scroll = max_scroll;
    }

    let para = Paragraph::new(Text::from(lines))
        .wrap(Wrap { trim: false })
        .scroll((app.results_scroll, 0));
    f.render_widget(para, inner);
}

fn render_push(f: &mut Frame, area: Rect, stage: &PushStage) {
    match stage {
        PushStage::Setup(state) => render_push_setup(f, area, state),
        PushStage::Preview {
            items,
            checks,
            cursor,
        } => render_push_preview(f, area, items, checks, *cursor),
        PushStage::Sending { queue, done, failed } => {
            render_push_sending(f, area, queue, done, failed)
        }
        PushStage::Result { done, failed } => render_push_result(f, area, done, failed),
    }
}

fn render_push_setup(f: &mut Frame, area: Rect, state: &SetupState) {
    let (step_label, current) = match state.step {
        SetupStep::Url => ("step 1/3: base URL", &state.url),
        SetupStep::Email => ("step 2/3: email", &state.email),
        SetupStep::Token => ("step 3/3: API token", &state.token),
    };
    let display: String = match state.step {
        SetupStep::Token => "*".repeat(current.chars().count()),
        _ => current.clone(),
    };
    let title_prefix = if state.editing {
        "editing"
    } else {
        "setup"
    };
    let mut lines: Vec<Line> = vec![
        Line::from(""),
        Line::from(Span::styled(
            format!("  {title_prefix} — {step_label}"),
            Style::default().add_modifier(Modifier::BOLD),
        )),
        Line::from(""),
        Line::from(format!("  > {display}_")),
        Line::from(""),
    ];
    if let Some(err) = &state.error {
        lines.push(Line::from(Span::styled(
            format!("  {err}"),
            Style::default().fg(Color::Red),
        )));
        lines.push(Line::from(""));
    }
    let hint = match state.step {
        SetupStep::Url => "  e.g. https://mycompany.atlassian.net",
        SetupStep::Email => "  e.g. you@example.com",
        SetupStep::Token => {
            if state.editing {
                "  create at id.atlassian.com → Security → API tokens.  empty = keep current."
            } else {
                "  create at id.atlassian.com → Security → API tokens.  saved to Keychain."
            }
        }
    };
    lines.push(Line::from(Span::styled(
        hint,
        Style::default().fg(Color::DarkGray),
    )));
    if !state.url.is_empty() {
        lines.push(Line::from(""));
        lines.push(Line::from(Span::styled(
            format!("  url:   {}", state.url),
            Style::default().fg(Color::DarkGray),
        )));
    }
    if !state.email.is_empty() && !matches!(state.step, SetupStep::Email) {
        lines.push(Line::from(Span::styled(
            format!("  email: {}", state.email),
            Style::default().fg(Color::DarkGray),
        )));
    }
    f.render_widget(Paragraph::new(Text::from(lines)).wrap(Wrap { trim: false }), area);
}

fn render_push_preview(
    f: &mut Frame,
    area: Rect,
    items: &[PushItem],
    checks: &[bool],
    cursor: usize,
) {
    let mut lines: Vec<Line> = vec![Line::from("")];
    let mut selected_total: u64 = 0;
    let mut selected_count = 0usize;

    for (i, (item, &checked)) in items.iter().zip(checks.iter()).enumerate() {
        let marker = if i == cursor { "> " } else { "  " };
        let box_mark = if checked { "[x]" } else { "[ ]" };
        let hours = item.seconds as f32 / 3600.0;
        let started = item.started.format("%Y-%m-%d %H:%M").to_string();
        let text = format!(
            "{marker}{box_mark}  {}  —  {}   (start {})",
            item.ticket,
            format_hours(hours),
            started
        );
        let style = if i == cursor {
            Style::default()
                .bg(Color::DarkGray)
                .fg(Color::White)
                .add_modifier(Modifier::BOLD)
        } else if !checked {
            Style::default().fg(Color::DarkGray)
        } else {
            Style::default()
        };
        lines.push(Line::from(Span::styled(text, style)));
        if checked {
            selected_total += item.seconds;
            selected_count += 1;
        }
    }

    lines.push(Line::from(""));
    let total_h = selected_total as f32 / 3600.0;
    lines.push(Line::from(Span::styled(
        format!(
            "  {} of {} ticket{} — total {}",
            selected_count,
            items.len(),
            if items.len() == 1 { "" } else { "s" },
            format_hours(total_h)
        ),
        Style::default()
            .fg(Color::Yellow)
            .add_modifier(Modifier::BOLD),
    )));
    f.render_widget(Paragraph::new(Text::from(lines)).wrap(Wrap { trim: false }), area);
}

fn render_push_sending(
    f: &mut Frame,
    area: Rect,
    queue: &[PushItem],
    done: &[String],
    failed: &[(String, String)],
) {
    let completed = done.len() + failed.len();
    let total = completed + queue.len();
    let mut lines: Vec<Line> = vec![
        Line::from(""),
        Line::from(Span::styled(
            format!("  posting worklogs: {}/{}", completed, total),
            Style::default()
                .fg(Color::Yellow)
                .add_modifier(Modifier::BOLD),
        )),
        Line::from(""),
    ];
    for t in done {
        lines.push(Line::from(Span::styled(
            format!("  ✓ {t}"),
            Style::default().fg(Color::Green),
        )));
    }
    for (t, e) in failed {
        lines.push(Line::from(Span::styled(
            format!("  ✗ {t}  ({e})"),
            Style::default().fg(Color::Red),
        )));
    }
    if let Some(next) = queue.first() {
        lines.push(Line::from(Span::styled(
            format!("  … {}", next.ticket),
            Style::default().fg(Color::DarkGray),
        )));
    }
    f.render_widget(Paragraph::new(Text::from(lines)).wrap(Wrap { trim: false }), area);
}

fn render_push_result(
    f: &mut Frame,
    area: Rect,
    done: &[String],
    failed: &[(String, String)],
) {
    let mut lines: Vec<Line> = vec![
        Line::from(""),
        Line::from(Span::styled(
            format!(
                "  done: {} succeeded, {} failed",
                done.len(),
                failed.len()
            ),
            Style::default().add_modifier(Modifier::BOLD),
        )),
        Line::from(""),
    ];
    for t in done {
        lines.push(Line::from(Span::styled(
            format!("  ✓ {t}"),
            Style::default().fg(Color::Green),
        )));
    }
    if !failed.is_empty() {
        lines.push(Line::from(""));
        for (t, e) in failed {
            lines.push(Line::from(Span::styled(
                format!("  ✗ {t}"),
                Style::default().fg(Color::Red),
            )));
            lines.push(Line::from(Span::styled(
                format!("      {e}"),
                Style::default().fg(Color::DarkGray),
            )));
        }
    }
    lines.push(Line::from(""));
    lines.push(Line::from(Span::styled(
        "  press Enter or Esc to return",
        Style::default().fg(Color::DarkGray),
    )));
    f.render_widget(Paragraph::new(Text::from(lines)).wrap(Wrap { trim: false }), area);
}

fn render_standup(f: &mut Frame, area: Rect, report: &StandupReport, scroll: u16) {
    let mut lines: Vec<Line<'static>> = Vec::new();
    lines.push(Line::from(""));

    // --- Yesterday ---
    let yesterday_total: f32 = report::group_with_hours(&report.yesterday_records)
        .iter()
        .map(|g| g.gap.value)
        .sum();
    let header = if report.yesterday_records.is_empty() {
        format!(
            "  Yesterday ({}) — nothing committed",
            report.yesterday.format("%a %b %-d")
        )
    } else {
        format!(
            "  Yesterday ({}) — {} total",
            report.yesterday.format("%a %b %-d"),
            format_hours(yesterday_total)
        )
    };
    lines.push(Line::from(Span::styled(
        header,
        Style::default()
            .fg(Color::Yellow)
            .add_modifier(Modifier::BOLD),
    )));
    lines.push(Line::from(""));

    if !report.yesterday_records.is_empty() {
        let groups = report::group_with_hours(&report.yesterday_records);
        for g in &groups {
            append_ticket_bullet(&mut lines, g, /*unpushed*/ false);
        }
    }

    lines.push(Line::from(""));

    // --- Today in-flight ---
    lines.push(Line::from(Span::styled(
        if report.in_flight_records.is_empty() {
            "  Today — nothing in flight, all pushed".to_string()
        } else {
            "  Today — in-flight (unpushed)".to_string()
        },
        Style::default()
            .fg(Color::Yellow)
            .add_modifier(Modifier::BOLD),
    )));
    lines.push(Line::from(""));

    if !report.in_flight_records.is_empty() {
        let groups = report::group_with_hours(&report.in_flight_records);
        for g in &groups {
            append_ticket_bullet(&mut lines, g, /*unpushed*/ true);
        }
    }

    let para = Paragraph::new(Text::from(lines))
        .wrap(Wrap { trim: false })
        .scroll((scroll, 0));
    f.render_widget(para, area);
}

/// Render a ticket group as a single natural-language bullet line:
///   • TP-1042 (1h 35m): scaffold login form, password field, fix flaky test
/// For in-flight tickets, time is replaced with commit count (those haven't
/// been pushed, so hours-logged-to-Jira isn't yet meaningful).
fn append_ticket_bullet(lines: &mut Vec<Line<'static>>, g: &GroupSummary<'_>, unpushed: bool) {
    let summary = summarise_commits(&g.commits, &g.ticket);
    let head = if unpushed {
        let n = g.commits.len();
        format!("  • {} ({} unpushed): {}", g.ticket, n, summary)
    } else {
        format!(
            "  • {} ({}): {}",
            g.ticket,
            g.gap.display(),
            summary
        )
    };
    lines.push(Line::from(head));
}

/// Concatenate commit subjects into a comma-separated sentence, stripping the
/// obvious "TP-1042: " prefix when it matches the ticket we're already under.
/// Caps at 3 subjects to keep the bullet line short; appends "… (+N more)".
fn summarise_commits(commits: &[&CommitRecord], ticket: &str) -> String {
    const MAX: usize = 3;
    let prefix = format!("{ticket}:");
    let parts: Vec<String> = commits
        .iter()
        .take(MAX)
        .map(|c| {
            c.subject
                .trim_start_matches(prefix.as_str())
                .trim()
                .to_string()
        })
        .filter(|s| !s.is_empty())
        .collect();
    let extra = commits.len().saturating_sub(parts.len());
    if parts.is_empty() {
        return format!("{} commit{}", commits.len(), if commits.len() == 1 { "" } else { "s" });
    }
    if extra > 0 {
        format!("{}… (+{} more)", parts.join(", "), extra)
    } else {
        parts.join(", ")
    }
}

fn render_scanning_skeleton(f: &mut Frame, area: Rect, app: &App) {
    let n = app.config.repos.len();
    let (s, e) = app.current_range();
    let range_desc = if s == e {
        s.format("%Y-%m-%d").to_string()
    } else {
        format!("{} → {}", s.format("%Y-%m-%d"), e.format("%Y-%m-%d"))
    };
    let mut lines: Vec<Line> = vec![
        Line::from(""),
        Line::from(Span::styled(
            format!(
                "  scanning {n} repo{} for {range_desc}...",
                if n == 1 { "" } else { "s" }
            ),
            Style::default()
                .fg(Color::Yellow)
                .add_modifier(Modifier::BOLD),
        )),
        Line::from(""),
    ];
    for path in &app.config.repos {
        lines.push(Line::from(Span::styled(
            format!("    {}", path.display()),
            Style::default().fg(Color::DarkGray),
        )));
    }
    f.render_widget(Paragraph::new(Text::from(lines)), area);
}

fn render_hint_bar(f: &mut Frame, area: Rect, app: &App) {
    let text: &str = if app.error.is_some() {
        "press any key to dismiss"
    } else if app.scanning {
        "scanning..."
    } else if app.standup.is_some() {
        "[PgUp/PgDn] scroll   [Esc] close"
    } else if let Some(stage) = &app.push {
        match stage {
            PushStage::Setup(_) => "type to enter   [Enter] next   [Esc] cancel",
            PushStage::Preview { .. } => {
                "[↑↓] select   [space] toggle   [Enter] push   [Esc] cancel"
            }
            PushStage::Sending { .. } => "posting...",
            PushStage::Result { .. } => "press [Enter] or [Esc] to return",
        }
    } else if app.is_adding() {
        "type to filter   [↑/↓] pick   [Enter] add (keeps adding)   [Esc] done"
    } else if app.focus == Focus::Right && app.results_edit.is_some() {
        "type e.g. 30m, 2h, 2h 30m   [Enter] save   [Esc] cancel"
    } else {
        match app.focus {
            Focus::Left => {
                "[Tab] switch   [a] add   [x] remove   [s] scan   [m] standup   [p] push   [q] quit"
            }
            Focus::Middle => {
                "[Tab] switch   [←→↑↓] move   [[/]] month   [t] today   [y] yesterday   [space] range   [m] standup   [s] scan"
            }
            Focus::Right => {
                "[Tab] switch   [↑↓] group   [e] time   [m] standup   [s] scan   [S] rescan   [p] push   [G] refresh PRs   [J] Jira"
            }
        }
    };
    let hint = Paragraph::new(Line::from(text)).dim();
    f.render_widget(hint, area);
}

// ---------- results line builder -----------------------------------------

fn lines_for_summaries(
    groups: &[GroupSummary<'_>],
    all_records: &[CommitRecord],
    app: &App,
) -> (Vec<Line<'static>>, Vec<u16>) {
    if all_records.is_empty() {
        return (
            vec![Line::from(Span::styled(
                "  (no matching commits)",
                Style::default().fg(Color::DarkGray),
            ))],
            Vec::new(),
        );
    }
    let repo_w = all_records.iter().map(|r| r.repo.len()).max().unwrap_or(0);
    let mut lines: Vec<Line<'static>> = Vec::new();
    let mut offsets: Vec<u16> = Vec::with_capacity(groups.len());
    let mut total: f32 = 0.0;

    for (i, g) in groups.iter().enumerate() {
        if i > 0 {
            lines.push(Line::from(""));
        }
        offsets.push(lines.len() as u16);
        let selected = i == app.results_selected && app.focus == Focus::Right;
        let marker = if selected { "> " } else { "  " };
        let n = g.commits.len();

        let pushed_badge = if app.pushed_this_session.contains(&g.ticket) {
            " ✓"
        } else {
            ""
        };
        let (header_text, value) = if selected && app.results_edit.is_some() {
            let buf = app.results_edit.as_deref().unwrap_or("");
            (
                format!("{marker}{}{pushed_badge} — edit: {buf}_", g.ticket),
                app.hours_overrides
                    .get(&g.ticket)
                    .copied()
                    .unwrap_or(g.gap.value),
            )
        } else if let Some(ov) = app.hours_overrides.get(&g.ticket) {
            (
                format!(
                    "{marker}{}{pushed_badge} — {} (manual)",
                    g.ticket,
                    format_hours(*ov)
                ),
                *ov,
            )
        } else {
            (
                format!("{marker}{}{pushed_badge} — {}", g.ticket, g.gap.display()),
                g.gap.value,
            )
        };
        total += value;

        let editing = selected && app.results_edit.is_some();
        let header_style = if editing {
            Style::default()
                .bg(Color::Yellow)
                .fg(Color::Black)
                .add_modifier(Modifier::BOLD)
        } else if selected {
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
            let pr_suffix = match app.pr_index.get(&c.oid) {
                Some(prs) if !prs.is_empty() => {
                    let nums: Vec<String> =
                        prs.iter().map(|p| format!("#{}", p.number)).collect();
                    format!("   [PR {}]", nums.join(", "))
                }
                _ => String::new(),
            };
            lines.push(Line::from(format!(
                "  [{:<w$}]  {}  {}  {}{}",
                c.repo,
                hm,
                short,
                c.subject,
                pr_suffix,
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
    (lines, offsets)
}

// ---------- error popup --------------------------------------------------

fn render_error_popup(f: &mut Frame, msg: &str) {
    let width = (msg.len() as u16 + 6).min(f.area().width).max(30);
    let area = center_rect(f.area(), width, 6);
    f.render_widget(Clear, area);
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
        Line::from("  press any key to dismiss").dim(),
    ];
    f.render_widget(Paragraph::new(Text::from(lines)), inner);
}
