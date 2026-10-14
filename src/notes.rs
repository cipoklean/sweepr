//! Notes: one plain-words line per item explaining what it is and whether to clean it.
//!
//! Built-in notes are written locally from the rules. AI notes come from any
//! OpenAI-compatible chat API (local Ollama by default). Only metadata is ever sent:
//! the item's kind, size, age, bucket, the project folder's name, and a file's
//! extension. Never file contents, never full paths.

use crate::rules::{Bucket, Kind};
use crate::scan::Item;
use serde_json::{json, Value};
use std::time::Duration;

pub struct AiConfig {
    pub url: String,
    pub model: String,
    pub key: Option<String>,
}

impl AiConfig {
    /// `SWEEPR_AI_URL` (default local Ollama), `SWEEPR_AI_MODEL`, `SWEEPR_AI_KEY`.
    pub fn from_env() -> Self {
        let get = |k: &str| std::env::var(k).ok().filter(|v| !v.trim().is_empty());
        AiConfig {
            url: get("SWEEPR_AI_URL").unwrap_or_else(|| "http://localhost:11434/v1".into()),
            model: get("SWEEPR_AI_MODEL").unwrap_or_else(|| "llama3.2".into()),
            key: get("SWEEPR_AI_KEY"),
        }
    }
}

/// A short, local explanation built from the rules. Always available, works offline.
pub fn builtin(item: &Item) -> String {
    let size = human(item.bytes);
    let name = folder_name(item);
    let idle = item
        .idle_days
        .map(|d| format!(", untouched for {d} days"))
        .unwrap_or_default();
    let back = item.kind.restore_hint();
    match (item.kind, item.bucket) {
        (Kind::LargeFile, _) => format!("A single {size} file{idle}. Sweepr can't tell if you still need it, so check before deleting."),
        (Kind::OldInstaller, _) => format!("An installer or archive in Downloads ({size}){idle}. If the app is installed, you likely don't need it."),
        (Kind::AndroidAvd, _) => format!("An Android emulator image ({size}). Removing it loses the emulator's apps and settings."),
        (Kind::PackageCache, _) => format!("A package manager's download cache ({size}). Clearing it is safe; packages re-download on the next install."),
        (k, _) if k.is_docker() => format!("{} ({size}). {}.", k.label(), capitalise(back)),
        (_, Bucket::Safe) => format!("{} for {name} ({size}){idle}. Safe to clean; get it back with {back}.", item.kind.label()),
        (_, Bucket::NeedsYou) => format!("{} for {name} ({size}). Held back: {}.", item.kind.label(), item.reason),
    }
}

/// The only data that leaves the machine for one item.
pub fn redact(id: usize, item: &Item) -> Value {
    let ext = if matches!(item.kind, Kind::LargeFile | Kind::OldInstaller) {
        item.path.extension().map(|e| e.to_string_lossy().to_lowercase())
    } else {
        None
    };
    let project = if item.kind.is_docker() || matches!(item.kind, Kind::LargeFile | Kind::OldInstaller | Kind::PackageCache | Kind::AndroidAvd) {
        None
    } else {
        Some(folder_name(item))
    };
    json!({
        "id": id,
        "kind": item.kind.label(),
        "bucket": match item.bucket { Bucket::Safe => "safe to clean", Bucket::NeedsYou => "needs owner review" },
        "size": human(item.bytes),
        "idle_days": item.idle_days,
        "project": project,
        "extension": ext,
        "restore": item.kind.restore_hint(),
        "rule": item.reason,
    })
}

pub fn payload(items: &[&Item], model: &str) -> Value {
    let meta: Vec<Value> = items.iter().enumerate().map(|(i, it)| redact(i, it)).collect();
    json!({
        "model": model,
        "temperature": 0.2,
        "messages": [
            {"role": "system", "content": SYSTEM},
            {"role": "user", "content": serde_json::to_string(&meta).unwrap()}
        ]
    })
}

const SYSTEM: &str = "You explain disk cleanup findings to a software developer. \
For each item write one plain sentence under 30 words: what it is, whether it is safe to remove, \
and how it comes back. Use only the metadata given; never invent file names or facts. \
Respect the bucket: never call a 'needs owner review' item safe. \
Reply with JSON only: {\"notes\":[{\"id\":0,\"note\":\"...\"}]}";

/// Ask the model for notes. Items it skips fall back to the built-in note.
pub fn ai(items: &[&Item], cfg: &AiConfig) -> Result<Vec<String>, String> {
    let url = format!("{}/chat/completions", cfg.url.trim_end_matches('/'));
    let agent = ureq::AgentBuilder::new().timeout(Duration::from_secs(120)).build();
    let mut req = agent.post(&url).set("Content-Type", "application/json");
    if let Some(k) = &cfg.key {
        req = req.set("Authorization", &format!("Bearer {k}"));
    }
    let body = payload(items, &cfg.model).to_string();
    let resp = req.send_string(&body).map_err(|e| match e {
        ureq::Error::Status(code, r) => format!("{url} answered {code}: {}", r.into_string().unwrap_or_default().chars().take(200).collect::<String>()),
        ureq::Error::Transport(t) => format!("could not reach {url}: {t}"),
    })?;
    let v: Value = serde_json::from_str(&resp.into_string().map_err(|e| e.to_string())?).map_err(|e| e.to_string())?;
    let content = v["choices"][0]["message"]["content"].as_str().unwrap_or("");
    Ok(merge(items, content))
}

/// Read the model's reply and fill gaps with built-in notes.
pub fn merge(items: &[&Item], content: &str) -> Vec<String> {
    let mut out: Vec<String> = items.iter().map(|i| builtin(i)).collect();
    let text = content.trim().trim_start_matches("```json").trim_start_matches("```").trim_end_matches("```");
    let start = text.find('{').unwrap_or(0);
    let end = text.rfind('}').map(|e| e + 1).unwrap_or(text.len());
    if let Ok(v) = serde_json::from_str::<Value>(&text[start..end.max(start)]) {
        for n in v["notes"].as_array().into_iter().flatten() {
            if let (Some(id), Some(note)) = (n["id"].as_u64(), n["note"].as_str()) {
                if let Some(slot) = out.get_mut(id as usize) {
                    let note = note.trim();
                    if !note.is_empty() {
                        *slot = note.to_string();
                    }
                }
            }
        }
    }
    out
}

fn folder_name(item: &Item) -> String {
    item.path
        .parent()
        .and_then(|p| p.file_name())
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| "this project".into())
}

fn capitalise(s: &str) -> String {
    let mut c = s.chars();
    c.next().map(|f| f.to_uppercase().collect::<String>() + c.as_str()).unwrap_or_default()
}

fn human(b: u64) -> String {
    const U: [&str; 5] = ["B", "KB", "MB", "GB", "TB"];
    let mut v = b as f64;
    let mut i = 0;
    while v >= 1024.0 && i < U.len() - 1 {
        v /= 1024.0;
        i += 1;
    }
    if i == 0 { format!("{b} B") } else { format!("{v:.1} {}", U[i]) }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn nm() -> Item {
        Item {
            path: PathBuf::from("/home/dev/code/secret-client/node_modules"),
            kind: Kind::NodeModules,
            bucket: Bucket::Safe,
            bytes: 1_500_000_000,
            idle_days: Some(94),
            reason: "project idle 94 days".into(),
        }
    }

    #[test]
    fn builtin_note_reads_well() {
        let n = builtin(&nm());
        assert!(n.contains("secret-client") && n.contains("94 days") && n.contains("npm"));
    }

    #[test]
    fn redaction_never_sends_paths() {
        let big = Item {
            path: PathBuf::from("/home/dev/Videos/tax-return-2025.mp4"),
            kind: Kind::LargeFile,
            bucket: Bucket::NeedsYou,
            bytes: 900_000_000,
            idle_days: Some(10),
            reason: "large file".into(),
        };
        let a = nm();
        let sent = payload(&[&a, &big], "m").to_string();
        assert!(!sent.contains("/home/dev"), "no full paths");
        assert!(!sent.contains("tax-return"), "no file names");
        assert_eq!(redact(1, &big)["extension"], "mp4", "extension only");
    }

    #[test]
    fn merge_uses_model_and_falls_back() {
        let a = nm();
        let b = nm();
        let reply = "```json\n{\"notes\":[{\"id\":1,\"note\":\"Old deps, safe to drop.\"}]}\n```";
        let notes = merge(&[&a, &b], reply);
        assert_eq!(notes[1], "Old deps, safe to drop.");
        assert_eq!(notes[0], builtin(&a), "missing id falls back to built-in");
        assert_eq!(merge(&[&a], "not json")[0], builtin(&a));
    }
}
