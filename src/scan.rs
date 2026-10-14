//! Walks the disk and turns what it finds into report items. Read-only: nothing is deleted.

use crate::rules::{self, Bucket, Kind};
use serde::Serialize;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, SystemTime};
use walkdir::WalkDir;

#[derive(Debug, Clone, Serialize)]
pub struct Item {
    pub path: PathBuf,
    pub kind: Kind,
    pub bucket: Bucket,
    pub bytes: u64,
    /// Days since the owning project (or the file) was last touched.
    pub idle_days: Option<u64>,
    /// Why it landed in its bucket, in plain words.
    pub reason: String,
}

pub struct Options {
    pub roots: Vec<PathBuf>,
    pub idle_days: u64,
    pub large_file_mb: u64,
    pub include_global: bool,
}

pub fn scan(opts: &Options) -> Vec<Item> {
    let mut items = Vec::new();
    for root in &opts.roots {
        scan_projects(root, opts, &mut items);
    }
    if opts.include_global {
        scan_global(opts, &mut items);
    }
    items.sort_by(|a, b| b.bytes.cmp(&a.bytes));
    items
}

fn scan_projects(root: &Path, opts: &Options, items: &mut Vec<Item>) {
    let large = opts.large_file_mb * 1024 * 1024;
    let mut it = WalkDir::new(root).follow_links(false).into_iter();
    while let Some(entry) = it.next() {
        let Ok(entry) = entry else { continue };
        let path = entry.path();
        let name = entry.file_name().to_string_lossy();

        if entry.file_type().is_dir() {
            if entry.depth() > 0 && rules::skip_walk(&name) {
                it.skip_current_dir();
                continue;
            }
            if let Some(kind) = rules::classify_project_dir(path) {
                it.skip_current_dir();
                items.push(project_item(path, kind, opts.idle_days));
            }
        } else if entry.file_type().is_file() {
            if let Ok(meta) = entry.metadata() {
                if meta.len() >= large {
                    items.push(Item {
                        path: path.to_path_buf(),
                        kind: Kind::LargeFile,
                        bucket: Bucket::NeedsYou,
                        bytes: meta.len(),
                        idle_days: meta.modified().ok().map(days_since),
                        reason: format!("over {} MB", opts.large_file_mb),
                    });
                }
            }
        }
    }
}

fn project_item(dir: &Path, kind: Kind, idle_limit: u64) -> Item {
    let project = dir.parent().unwrap_or(dir);
    let bytes = dir_size(dir);
    let idle = project_idle_days(project, dir);
    let git = git_state(project);

    let (bucket, reason) = match (&git, idle) {
        (GitState::Dirty, _) => (Bucket::NeedsYou, "project has uncommitted or unpushed work".to_string()),
        (_, Some(d)) if kind == Kind::PyCache => (Bucket::Safe, format!("regenerated automatically · project idle {d} days")),
        (_, Some(d)) if d >= idle_limit => (Bucket::Safe, format!("project idle {d} days · restore with {}", kind.restore_hint())),
        (_, Some(d)) => (Bucket::NeedsYou, format!("project active {d} days ago, you may still need it")),
        (_, None) => (Bucket::NeedsYou, "could not read project dates".to_string()),
    };

    Item { path: dir.to_path_buf(), kind, bucket, bytes, idle_days: idle, reason }
}

/// Well-known package caches in the home folder. All re-download on demand.
fn scan_global(opts: &Options, items: &mut Vec<Item>) {
    let Some(home) = dirs::home_dir() else { return };
    let caches = [
        ".npm/_cacache",
        ".cache/pip",
        ".cache/yarn",
        ".cache/pnpm",
        ".local/share/pnpm/store",
        ".cargo/registry/cache",
        ".gradle/caches",
        ".m2/repository",
        "Library/Caches/pip",
        "Library/Caches/Yarn",
        "AppData/Local/npm-cache",
        "AppData/Local/pip/cache",
    ];
    for rel in caches {
        let p = home.join(rel);
        if p.is_dir() {
            items.push(Item {
                bytes: dir_size(&p),
                idle_days: None,
                reason: format!("{}", Kind::PackageCache.restore_hint()),
                path: p,
                kind: Kind::PackageCache,
                bucket: Bucket::Safe,
            });
        }
    }

    // Android emulator images: big, but they hold app data, so the owner decides.
    let avd = home.join(".android/avd");
    if let Ok(rd) = std::fs::read_dir(&avd) {
        for e in rd.flatten() {
            let p = e.path();
            if p.is_dir() && p.extension().map_or(false, |x| x == "avd") {
                let idle = newest_mtime(&p).map(days_since);
                items.push(Item {
                    bytes: dir_size(&p),
                    reason: match idle {
                        Some(d) => format!("last used {d} days ago, holds emulator data"),
                        None => "holds emulator data".into(),
                    },
                    idle_days: idle,
                    path: p,
                    kind: Kind::AndroidAvd,
                    bucket: Bucket::NeedsYou,
                });
            }
        }
    }

    // Installers and archives forgotten in Downloads.
    if let Ok(rd) = std::fs::read_dir(home.join("Downloads")) {
        for e in rd.flatten() {
            let p = e.path();
            let Ok(meta) = e.metadata() else { continue };
            if !meta.is_file() || !rules::is_installer(&p) {
                continue;
            }
            let idle = meta.modified().ok().map(days_since);
            if idle.map_or(false, |d| d >= opts.idle_days) {
                items.push(Item {
                    bytes: meta.len(),
                    reason: format!("downloaded {} days ago", idle.unwrap_or(0)),
                    idle_days: idle,
                    path: p,
                    kind: Kind::OldInstaller,
                    bucket: Bucket::NeedsYou,
                });
            }
        }
    }
}

pub fn dir_size(dir: &Path) -> u64 {
    WalkDir::new(dir)
        .follow_links(false)
        .into_iter()
        .filter_map(Result::ok)
        .filter(|e| e.file_type().is_file())
        .filter_map(|e| e.metadata().ok())
        .map(|m| m.len())
        .sum()
}

/// Newest modification time among the project's own entries, ignoring the artifact folder.
fn project_idle_days(project: &Path, artifact: &Path) -> Option<u64> {
    let mut newest: Option<SystemTime> = None;
    for e in WalkDir::new(project).max_depth(2).into_iter().filter_entry(|e| {
        let n = e.file_name().to_string_lossy();
        e.path() != artifact && !matches!(n.as_ref(), "node_modules" | "target" | ".git" | "__pycache__" | ".venv" | "venv" | "build" | ".next")
    }) {
        let Ok(e) = e else { continue };
        if e.depth() == 0 {
            continue;
        }
        if let Some(t) = e.metadata().ok().and_then(|m| m.modified().ok()) {
            newest = Some(newest.map_or(t, |n| n.max(t)));
        }
    }
    newest.map(days_since)
}

fn newest_mtime(dir: &Path) -> Option<SystemTime> {
    WalkDir::new(dir)
        .into_iter()
        .filter_map(Result::ok)
        .filter_map(|e| e.metadata().ok()?.modified().ok())
        .max()
}

fn days_since(t: SystemTime) -> u64 {
    SystemTime::now().duration_since(t).unwrap_or(Duration::ZERO).as_secs() / 86_400
}

#[derive(Debug, PartialEq)]
enum GitState {
    NotARepo,
    Clean,
    Dirty,
}

/// A repo with uncommitted changes or commits not on its upstream is never auto-cleaned.
fn git_state(project: &Path) -> GitState {
    if !project.join(".git").exists() {
        return GitState::NotARepo;
    }
    let run = |args: &[&str]| {
        Command::new("git").arg("-C").arg(project).args(args).output().ok()
    };
    match run(&["status", "--porcelain"]) {
        Some(o) if o.status.success() && !o.stdout.is_empty() => return GitState::Dirty,
        Some(o) if o.status.success() => {}
        _ => return GitState::Dirty, // can't tell: be careful
    }
    if let Some(o) = run(&["rev-list", "--count", "@{u}..HEAD"]) {
        if o.status.success() && String::from_utf8_lossy(&o.stdout).trim() != "0" {
            return GitState::Dirty;
        }
    }
    GitState::Clean
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    #[test]
    fn finds_node_modules_and_sizes_it() {
        let tmp = tempfile::tempdir().unwrap();
        let proj = tmp.path().join("app");
        fs::create_dir_all(proj.join("node_modules/left-pad")).unwrap();
        fs::write(proj.join("package.json"), "{}").unwrap();
        fs::write(proj.join("node_modules/left-pad/index.js"), vec![b'x'; 2048]).unwrap();

        let items = scan(&Options {
            roots: vec![tmp.path().to_path_buf()],
            idle_days: 30,
            large_file_mb: 500,
            include_global: false,
        });
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].kind, Kind::NodeModules);
        assert_eq!(items[0].bytes, 2048);
        // Just touched, so it is not safe yet.
        assert_eq!(items[0].bucket, Bucket::NeedsYou);
    }

    #[test]
    fn idle_limit_zero_makes_it_safe() {
        let tmp = tempfile::tempdir().unwrap();
        let proj = tmp.path().join("crate");
        fs::create_dir_all(proj.join("target/debug")).unwrap();
        fs::write(proj.join("Cargo.toml"), "").unwrap();
        let items = scan(&Options {
            roots: vec![tmp.path().to_path_buf()],
            idle_days: 0,
            large_file_mb: 500,
            include_global: false,
        });
        assert_eq!(items[0].bucket, Bucket::Safe);
    }
}
