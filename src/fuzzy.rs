use std::path::PathBuf;
use std::sync::mpsc::{self, Receiver, TryRecvError};
use std::thread;

use nucleo_matcher::pattern::{CaseMatching, Normalization, Pattern};
use nucleo_matcher::{Config, Matcher, Utf32Str};
use walkdir::WalkDir;

const MAX_DEPTH: usize = 5;
const SKIP_DIRS: &[&str] = &[
    // VCS internals
    ".git", ".svn", ".hg",
    // package/build dirs
    "node_modules", "bower_components", "target", "dist", "build", "out",
    // language-specific noise
    ".venv", "venv", "__pycache__", ".mypy_cache", ".pytest_cache",
    ".next", ".nuxt", ".cache", ".gradle",
    // editor metadata
    ".idea", ".vscode",
    // user-level tool caches
    ".cargo", ".rustup", ".npm", ".nvm", ".pyenv", ".rbenv", ".gem",
    // macOS noise
    "Library", "Applications", ".Trash",
];

pub struct FuzzyIndex {
    rx: Option<Receiver<PathBuf>>,
    paths: Vec<PathBuf>,
    display: Vec<String>,
    matcher: Matcher,
}

impl FuzzyIndex {
    pub fn new(roots: Vec<PathBuf>) -> Self {
        let (tx, rx) = mpsc::channel();
        thread::spawn(move || {
            for root in roots {
                let mut it = WalkDir::new(&root)
                    .max_depth(MAX_DEPTH)
                    .follow_links(false)
                    .into_iter()
                    .filter_entry(|e| {
                        if e.depth() == 0 {
                            return true;
                        }
                        if !e.file_type().is_dir() {
                            return false;
                        }
                        let name = e.file_name().to_string_lossy();
                        if SKIP_DIRS.contains(&name.as_ref()) {
                            return false;
                        }
                        if name.starts_with('.') {
                            return false;
                        }
                        true
                    });
                loop {
                    let entry = match it.next() {
                        Some(Ok(e)) => e,
                        Some(Err(_)) => continue,
                        None => break,
                    };
                    if entry.depth() == 0 || !entry.file_type().is_dir() {
                        continue;
                    }
                    // `.git` can be a directory (normal repo) or a file (worktree
                    // / submodule gitlink) — `.exists()` covers both.
                    if entry.path().join(".git").exists() {
                        if tx.send(entry.path().to_path_buf()).is_err() {
                            return;
                        }
                        it.skip_current_dir();
                    }
                }
            }
        });
        let cfg = Config::DEFAULT.match_paths();
        Self {
            rx: Some(rx),
            paths: Vec::new(),
            display: Vec::new(),
            matcher: Matcher::new(cfg),
        }
    }

    pub fn drain(&mut self) {
        if let Some(rx) = &self.rx {
            loop {
                match rx.try_recv() {
                    Ok(p) => {
                        self.display.push(p.display().to_string());
                        self.paths.push(p);
                    }
                    Err(TryRecvError::Empty) => break,
                    Err(TryRecvError::Disconnected) => {
                        self.rx = None;
                        break;
                    }
                }
            }
        }
    }

    pub fn done(&self) -> bool {
        self.rx.is_none()
    }

    pub fn len(&self) -> usize {
        self.paths.len()
    }

    pub fn search(&mut self, query: &str, limit: usize) -> Vec<(PathBuf, String)> {
        if self.paths.is_empty() {
            return Vec::new();
        }
        if query.is_empty() {
            return self
                .paths
                .iter()
                .zip(self.display.iter())
                .take(limit)
                .map(|(p, s)| (p.clone(), s.clone()))
                .collect();
        }
        let pattern = Pattern::parse(query, CaseMatching::Smart, Normalization::Smart);
        let mut scored: Vec<(u32, usize)> = Vec::with_capacity(64);
        let mut buf: Vec<char> = Vec::new();
        for (idx, s) in self.display.iter().enumerate() {
            let utf = Utf32Str::new(s, &mut buf);
            if let Some(score) = pattern.score(utf, &mut self.matcher) {
                scored.push((score, idx));
            }
        }
        scored.sort_unstable_by(|a, b| b.0.cmp(&a.0));
        scored.truncate(limit);
        scored
            .into_iter()
            .map(|(_, idx)| (self.paths[idx].clone(), self.display[idx].clone()))
            .collect()
    }
}

pub fn default_roots() -> Vec<PathBuf> {
    let home = match directories::BaseDirs::new() {
        Some(b) => b.home_dir().to_path_buf(),
        None => return vec![],
    };
    let candidates = ["dev", "work", "projects", "code", "src", "Documents"];
    let found: Vec<PathBuf> = candidates
        .iter()
        .map(|c| home.join(c))
        .filter(|p| p.exists() && p.is_dir())
        .collect();
    if found.is_empty() {
        vec![home]
    } else {
        found
    }
}
