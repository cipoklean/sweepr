//! Docker: images, build cache, volumes and containers, read through the `docker` CLI.
//! Skipped quietly when Docker isn't installed or the daemon isn't running.

use crate::rules::{Bucket, Kind};
use crate::scan::Item;
use serde_json::Value;
use std::path::PathBuf;
use std::process::Command;

/// Ask Docker what it holds. Returns nothing when Docker is missing or stopped.
pub fn detect() -> Vec<Item> {
    let out = Command::new("docker")
        .args(["system", "df", "-v", "--format", "{{json .}}"])
        .output();
    match out {
        Ok(o) if o.status.success() => parse(&String::from_utf8_lossy(&o.stdout)),
        _ => Vec::new(),
    }
}

/// Turn `docker system df -v --format '{{json .}}'` into items.
pub fn parse(json: &str) -> Vec<Item> {
    let Ok(v) = serde_json::from_str::<Value>(json.trim()) else { return Vec::new() };
    let mut items = Vec::new();

    for img in arr(&v, "Images") {
        let repo = s(img, "Repository");
        let tag = s(img, "Tag");
        let id = short_id(&s(img, "ID"));
        let in_use = num(img, "Containers") > 0;
        if in_use {
            continue;
        }
        // UniqueSize avoids double counting layers shared with other images.
        let bytes = if has(img, "UniqueSize") { size(img, "UniqueSize") } else { size(img, "Size") };
        if bytes == 0 {
            continue;
        }
        if repo == "<none>" && tag == "<none>" {
            items.push(item(
                format!("docker image <none> ({id})"),
                Kind::DockerDanglingImage,
                Bucket::Safe,
                bytes,
                "Untagged image no container uses, usually left behind by a rebuild.".into(),
            ));
        } else {
            items.push(item(
                format!("docker image {repo}:{tag} ({id})"),
                Kind::DockerImage,
                Bucket::NeedsYou,
                bytes,
                "No container uses this image. Remove it if you don't plan to run it again.".into(),
            ));
        }
    }

    let cache: u64 = arr(&v, "BuildCache")
        .iter()
        .filter(|c| !boolish(c, "InUse"))
        .map(|c| size(c, "Size"))
        .sum();
    if cache > 0 {
        items.push(item(
            "docker build cache".into(),
            Kind::DockerBuildCache,
            Bucket::Safe,
            cache,
            "Build cache not used by a running build. Docker rebuilds it as needed.".into(),
        ));
    }

    for vol in arr(&v, "Volumes") {
        if num(vol, "Links") > 0 {
            continue;
        }
        let bytes = size(vol, "Size");
        if bytes == 0 {
            continue;
        }
        items.push(item(
            format!("docker volume {}", s(vol, "Name")),
            Kind::DockerVolume,
            Bucket::NeedsYou,
            bytes,
            "No container is attached. Volumes often hold database data, so check before removing.".into(),
        ));
    }

    for c in arr(&v, "Containers") {
        if s(c, "State") == "running" {
            continue;
        }
        let bytes = size(c, "Size");
        if bytes == 0 {
            continue;
        }
        let name = s(c, "Names");
        items.push(item(
            format!("docker container {} ({})", name, short_id(&s(c, "ID"))),
            Kind::DockerContainer,
            Bucket::NeedsYou,
            bytes,
            format!("Stopped container ({}). Anything written inside it is lost on removal.", s(c, "Status")),
        ));
    }
    items
}

/// Commands `sweepr clean --docker` runs. Only the Safe kinds are ever pruned.
pub fn prune_commands(items: &[&Item]) -> Vec<Vec<&'static str>> {
    let mut cmds = Vec::new();
    if items.iter().any(|i| i.kind == Kind::DockerDanglingImage) {
        cmds.push(vec!["image", "prune", "-f"]);
    }
    if items.iter().any(|i| i.kind == Kind::DockerBuildCache) {
        cmds.push(vec!["builder", "prune", "-a", "-f"]);
    }
    cmds
}

pub fn run(args: &[&str]) -> Result<(), String> {
    let o = Command::new("docker").args(args).output().map_err(|e| e.to_string())?;
    if o.status.success() {
        Ok(())
    } else {
        Err(String::from_utf8_lossy(&o.stderr).trim().to_string())
    }
}

fn item(label: String, kind: Kind, bucket: Bucket, bytes: u64, reason: String) -> Item {
    Item { path: PathBuf::from(label), kind, bucket, bytes, idle_days: None, reason }
}

fn arr<'a>(v: &'a Value, key: &str) -> Vec<&'a Value> {
    v.get(key).and_then(|a| a.as_array()).map(|a| a.iter().collect()).unwrap_or_default()
}

fn has(v: &Value, key: &str) -> bool {
    v.get(key).map(|x| !x.is_null() && x.as_str() != Some("")).unwrap_or(false)
}

fn s(v: &Value, key: &str) -> String {
    match v.get(key) {
        Some(Value::String(x)) => x.clone(),
        Some(Value::Null) | None => String::new(),
        Some(x) => x.to_string(),
    }
}

fn num(v: &Value, key: &str) -> u64 {
    match v.get(key) {
        Some(Value::Number(n)) => n.as_u64().unwrap_or(0),
        Some(Value::String(x)) => x.trim().parse().unwrap_or(0),
        _ => 0,
    }
}

fn boolish(v: &Value, key: &str) -> bool {
    match v.get(key) {
        Some(Value::Bool(b)) => *b,
        Some(Value::String(x)) => x.eq_ignore_ascii_case("true"),
        _ => false,
    }
}

fn size(v: &Value, key: &str) -> u64 {
    match v.get(key) {
        Some(Value::Number(n)) => n.as_u64().unwrap_or(0),
        Some(Value::String(x)) => parse_size(x),
        _ => 0,
    }
}

fn short_id(id: &str) -> String {
    id.trim_start_matches("sha256:").chars().take(12).collect()
}

/// Docker prints sizes like "1.23GB", "512MB", "12.3kB", "0B" or "2B (virtual 77.8MB)".
pub fn parse_size(text: &str) -> u64 {
    let first = text.split_whitespace().next().unwrap_or("");
    let first = first.split('(').next().unwrap_or("");
    let split = first.find(|c: char| c.is_ascii_alphabetic()).unwrap_or(first.len());
    let (n, unit) = first.split_at(split);
    let Ok(n) = n.trim().parse::<f64>() else { return 0 };
    let mult: f64 = match unit.trim().to_ascii_lowercase().as_str() {
        "" | "b" => 1.0,
        "kb" => 1e3,
        "mb" => 1e6,
        "gb" => 1e9,
        "tb" => 1e12,
        "kib" => 1024.0,
        "mib" => 1024f64.powi(2),
        "gib" => 1024f64.powi(3),
        "tib" => 1024f64.powi(4),
        _ => return 0,
    };
    (n * mult) as u64
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = r#"{
      "Images": [
        {"Containers":"0","ID":"sha256:aaaaaaaaaaaaaaaa","Repository":"<none>","Tag":"<none>","Size":"1.2GB","UniqueSize":"1.1GB"},
        {"Containers":"0","ID":"bbbbbbbbbbbbbbbb","Repository":"postgres","Tag":"15","Size":"400MB","UniqueSize":"350MB"},
        {"Containers":"1","ID":"cccccccccccccccc","Repository":"redis","Tag":"7","Size":"100MB","UniqueSize":"100MB"}
      ],
      "Containers": [
        {"ID":"dddddddddddd","Names":"old_api","State":"exited","Status":"Exited (0) 3 weeks ago","Size":"25MB (virtual 400MB)"},
        {"ID":"eeeeeeeeeeee","Names":"cache","State":"running","Status":"Up 2 hours","Size":"1MB"}
      ],
      "Volumes": [
        {"Name":"pgdata","Links":"0","Size":"800MB"},
        {"Name":"live","Links":"1","Size":"5GB"}
      ],
      "BuildCache": [
        {"ID":"x1","InUse":false,"Size":"2.5GB"},
        {"ID":"x2","InUse":"true","Size":"1GB"}
      ]
    }"#;

    #[test]
    fn sizes() {
        assert_eq!(parse_size("1.5GB"), 1_500_000_000);
        assert_eq!(parse_size("12.3kB"), 12_300);
        assert_eq!(parse_size("0B"), 0);
        assert_eq!(parse_size("2B (virtual 77.8MB)"), 2);
        assert_eq!(parse_size("1GiB"), 1 << 30);
        assert_eq!(parse_size("junk"), 0);
    }

    #[test]
    fn buckets_docker_items() {
        let items = parse(SAMPLE);
        let find = |k: Kind| items.iter().filter(|i| i.kind == k).collect::<Vec<_>>();

        let dangling = find(Kind::DockerDanglingImage);
        assert_eq!(dangling.len(), 1);
        assert_eq!(dangling[0].bucket, Bucket::Safe);
        assert_eq!(dangling[0].bytes, 1_100_000_000);

        let cache = find(Kind::DockerBuildCache);
        assert_eq!(cache[0].bytes, 2_500_000_000, "in-use cache is left out");
        assert_eq!(cache[0].bucket, Bucket::Safe);

        assert_eq!(find(Kind::DockerImage).len(), 1, "redis is in use, postgres is not");
        assert_eq!(find(Kind::DockerVolume).len(), 1, "attached volume is left out");
        assert_eq!(find(Kind::DockerContainer).len(), 1, "running container is left out");
        assert!(find(Kind::DockerVolume)[0].bucket == Bucket::NeedsYou);
    }

    #[test]
    fn bad_json_is_ignored() {
        assert!(parse("Cannot connect to the Docker daemon").is_empty());
    }

    #[test]
    fn prune_only_safe_kinds() {
        let items = parse(SAMPLE);
        let refs: Vec<&Item> = items.iter().collect();
        let cmds = prune_commands(&refs);
        assert_eq!(cmds.len(), 2);
        assert!(cmds.iter().all(|c| c[1] == "prune"));
        assert!(!cmds.iter().any(|c| c[0] == "volume" || c[0] == "container"));
    }
}
