//! Quarantine: cleaned items are moved here, not deleted, and can be restored for 7 days.

use serde::{Deserialize, Serialize};
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

pub const KEEP_DAYS: u64 = 7;

#[derive(Debug, Serialize, Deserialize)]
pub struct Entry {
    pub original: PathBuf,
    pub stored: PathBuf,
    pub bytes: u64,
    pub kind: String,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct Batch {
    pub id: String,
    pub created_unix: u64,
    pub entries: Vec<Entry>,
}

impl Batch {
    pub fn bytes(&self) -> u64 {
        self.entries.iter().map(|e| e.bytes).sum()
    }
    pub fn age_days(&self) -> u64 {
        now().saturating_sub(self.created_unix) / 86_400
    }
    pub fn expired(&self) -> bool {
        self.age_days() >= KEEP_DAYS
    }
}

/// Where batches live. `SWEEPR_HOME` overrides it (handy for tests).
pub fn base_dir() -> PathBuf {
    if let Some(p) = std::env::var_os("SWEEPR_HOME") {
        return PathBuf::from(p).join("quarantine");
    }
    dirs::data_local_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join("sweepr")
        .join("quarantine")
}

fn now() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0)
}

pub struct MoveOutcome {
    pub batch: Batch,
    pub skipped: Vec<(PathBuf, String)>,
}

/// Move each path into a new batch. A path on another drive (rename fails) is skipped, never copied.
pub fn quarantine(base: &Path, items: &[(PathBuf, u64, String)]) -> io::Result<MoveOutcome> {
    let created = now();
    let mut id = created.to_string();
    let mut n = 1;
    while base.join(&id).exists() {
        id = format!("{created}-{n}");
        n += 1;
    }
    let dir = base.join(&id);
    fs::create_dir_all(&dir)?;

    let mut batch = Batch { id, created_unix: created, entries: Vec::new() };
    let mut skipped = Vec::new();
    for (i, (path, bytes, kind)) in items.iter().enumerate() {
        if let Err(why) = guard(path) {
            skipped.push((path.clone(), why));
            continue;
        }
        let name = path.file_name().map(|n| n.to_os_string()).unwrap_or_else(|| "item".into());
        let slot = dir.join(i.to_string());
        fs::create_dir_all(&slot)?;
        let stored = slot.join(name);
        match fs::rename(path, &stored) {
            Ok(()) => batch.entries.push(Entry {
                original: path.clone(),
                stored,
                bytes: *bytes,
                kind: kind.clone(),
            }),
            Err(e) => {
                let _ = fs::remove_dir(&slot);
                skipped.push((path.clone(), format!("could not move ({e}); left in place")));
            }
        }
    }
    write_manifest(&dir, &batch)?;
    if batch.entries.is_empty() {
        let _ = fs::remove_dir_all(&dir);
    }
    Ok(MoveOutcome { batch, skipped })
}

/// Refuse anything that looks like a top-level or home folder, however it got here.
fn guard(path: &Path) -> Result<(), String> {
    if !path.exists() {
        return Err("no longer exists".into());
    }
    if path.components().count() <= 2 {
        return Err("too close to the root, refusing".into());
    }
    if let Some(home) = dirs::home_dir() {
        if path == home {
            return Err("is your home folder, refusing".into());
        }
    }
    Ok(())
}

fn write_manifest(dir: &Path, batch: &Batch) -> io::Result<()> {
    let json = serde_json::to_string_pretty(batch).map_err(io::Error::other)?;
    fs::write(dir.join("manifest.json"), json)
}

pub fn list(base: &Path) -> Vec<Batch> {
    let mut out: Vec<Batch> = fs::read_dir(base)
        .into_iter()
        .flatten()
        .flatten()
        .filter_map(|e| fs::read_to_string(e.path().join("manifest.json")).ok())
        .filter_map(|s| serde_json::from_str(&s).ok())
        .collect();
    out.sort_by(|a, b| b.created_unix.cmp(&a.created_unix).then(b.id.cmp(&a.id)));
    out
}

pub struct RestoreOutcome {
    pub restored: Vec<PathBuf>,
    pub failed: Vec<(PathBuf, String)>,
}

/// Put a batch back where it came from. Never overwrites something that now sits at the original path.
pub fn restore(base: &Path, id: &str) -> io::Result<RestoreOutcome> {
    let dir = base.join(id);
    let text = fs::read_to_string(dir.join("manifest.json"))?;
    let mut batch: Batch = serde_json::from_str(&text).map_err(io::Error::other)?;
    let mut out = RestoreOutcome { restored: Vec::new(), failed: Vec::new() };
    let mut remaining = Vec::new();
    for e in batch.entries.drain(..) {
        if e.original.exists() {
            out.failed.push((e.original.clone(), "something new is already there".into()));
            remaining.push(e);
            continue;
        }
        if let Some(parent) = e.original.parent() {
            let _ = fs::create_dir_all(parent);
        }
        match fs::rename(&e.stored, &e.original) {
            Ok(()) => out.restored.push(e.original),
            Err(err) => {
                out.failed.push((e.original.clone(), err.to_string()));
                remaining.push(e);
            }
        }
    }
    if remaining.is_empty() {
        fs::remove_dir_all(&dir)?;
    } else {
        batch.entries = remaining;
        write_manifest(&dir, &batch)?;
    }
    Ok(out)
}

/// Permanently delete batches past the undo window (or all of them). Returns bytes freed.
pub fn purge(base: &Path, all: bool) -> io::Result<(usize, u64)> {
    let mut count = 0;
    let mut bytes = 0;
    for b in list(base) {
        if all || b.expired() {
            bytes += b.bytes();
            fs::remove_dir_all(base.join(&b.id))?;
            count += 1;
        }
    }
    Ok((count, bytes))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn setup() -> (tempfile::TempDir, PathBuf, PathBuf) {
        let tmp = tempfile::tempdir().unwrap();
        let base = tmp.path().join("q");
        let nm = tmp.path().join("proj").join("node_modules");
        fs::create_dir_all(nm.join("pkg")).unwrap();
        fs::write(nm.join("pkg/index.js"), "x").unwrap();
        (tmp, base, nm)
    }

    #[test]
    fn quarantine_then_undo_round_trips() {
        let (_tmp, base, nm) = setup();
        let out = quarantine(&base, &[(nm.clone(), 1, "node_modules".into())]).unwrap();
        assert!(out.skipped.is_empty());
        assert!(!nm.exists());
        assert_eq!(list(&base).len(), 1);

        let r = restore(&base, &out.batch.id).unwrap();
        assert_eq!(r.restored, vec![nm.clone()]);
        assert!(nm.join("pkg/index.js").exists());
        assert!(list(&base).is_empty());
    }

    #[test]
    fn undo_never_overwrites() {
        let (_tmp, base, nm) = setup();
        let out = quarantine(&base, &[(nm.clone(), 1, "node_modules".into())]).unwrap();
        fs::create_dir_all(&nm).unwrap();
        let r = restore(&base, &out.batch.id).unwrap();
        assert!(r.restored.is_empty());
        assert_eq!(r.failed.len(), 1);
        assert_eq!(list(&base).len(), 1, "batch kept so nothing is lost");
    }

    #[test]
    fn purge_keeps_fresh_batches() {
        let (_tmp, base, nm) = setup();
        quarantine(&base, &[(nm, 1, "node_modules".into())]).unwrap();
        assert_eq!(purge(&base, false).unwrap().0, 0);
        assert_eq!(purge(&base, true).unwrap().0, 1);
        assert!(list(&base).is_empty());
    }
}
