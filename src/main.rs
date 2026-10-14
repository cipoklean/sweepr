mod docker;
mod quarantine;
mod rules;
mod scan;

use clap::{Args, Parser, Subcommand};
use rules::Bucket;
use std::io::{self, BufRead, Write};
use std::path::PathBuf;

/// Find the junk your dev work left behind, sorted into safe-to-clean and needs-you.
/// Cleaning moves items to a quarantine you can undo for 7 days.
#[derive(Parser)]
#[command(name = "sweepr", version, about)]
struct Cli {
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Args)]
struct ScanArgs {
    /// Folders to scan (default: your home folder).
    paths: Vec<PathBuf>,
    /// A project counts as idle after this many days without changes.
    #[arg(long, default_value_t = 30)]
    idle_days: u64,
    /// Flag single files at least this big, in MB.
    #[arg(long, default_value_t = 500)]
    large_mb: u64,
    /// Skip package caches, emulator images and Downloads in your home folder.
    #[arg(long)]
    no_global: bool,
    /// Skip Docker images, build cache, volumes and containers.
    #[arg(long)]
    no_docker: bool,
}

#[derive(Subcommand)]
enum Cmd {
    /// Scan folders and print what can be cleaned. Deletes nothing.
    Scan {
        #[command(flatten)]
        scan: ScanArgs,
        /// Print JSON instead of the report.
        #[arg(long)]
        json: bool,
        /// Rows to show per bucket.
        #[arg(long, default_value_t = 25)]
        top: usize,
    },
    /// Move every "Safe to clean" item into quarantine. "Needs you" items are never touched.
    Clean {
        #[command(flatten)]
        scan: ScanArgs,
        /// Show what would move, and stop.
        #[arg(long)]
        dry_run: bool,
        /// Don't ask for confirmation.
        #[arg(short, long)]
        yes: bool,
        /// Also prune Docker dangling images and build cache. Can't be undone.
        #[arg(long)]
        docker: bool,
    },
    /// Put a cleaned batch back where it came from (default: the latest).
    Undo {
        /// Batch id from `sweepr quarantine`.
        id: Option<String>,
    },
    /// List batches waiting in quarantine.
    Quarantine,
    /// Permanently delete batches older than 7 days.
    Purge {
        /// Delete every batch now, including ones still inside the undo window.
        #[arg(long)]
        all: bool,
    },
}

fn main() {
    let Cli { cmd } = Cli::parse();
    let base = quarantine::base_dir();
    match cmd {
        Cmd::Scan { scan, json, top } => {
            let started = std::time::Instant::now();
            let items = run_scan(scan);
            if json {
                println!("{}", serde_json::to_string_pretty(&items).unwrap());
                return;
            }
            print_report(&items, top);
            let safe: u64 = items.iter().filter(|i| i.bucket == Bucket::Safe).map(|i| i.bytes).sum();
            println!(
                "\nScanned in {:.1}s. {} could be freed safely. Nothing was deleted. Run `sweepr clean` to free it.",
                started.elapsed().as_secs_f64(),
                human(safe)
            );
        }
        Cmd::Clean { scan, dry_run, yes, docker } => {
            auto_purge(&base);
            let safe: Vec<_> = run_scan(scan).into_iter().filter(|i| i.bucket == Bucket::Safe).collect();
            let (dock, files): (Vec<_>, Vec<_>) = safe.into_iter().partition(|i| i.kind.is_docker());
            if files.is_empty() && (dock.is_empty() || !docker) {
                println!("Nothing safe to clean. 🎉");
                docker_hint(&dock, docker);
                return;
            }
            if !files.is_empty() {
                let total: u64 = files.iter().map(|i| i.bytes).sum();
                println!("Sweepr will move {} items ({}) to quarantine:\n", files.len(), human(total));
                for i in &files {
                    println!("{:>10}  {:<22} {}", human(i.bytes), i.kind.label(), i.path.display());
                }
            }
            if docker && !dock.is_empty() {
                let total: u64 = dock.iter().map(|i| i.bytes).sum();
                println!("\nDocker will prune {} ({}). This can't be undone; Docker rebuilds or re-pulls what it needs:\n", plural(dock.len()), human(total));
                for i in &dock {
                    println!("{:>10}  {:<22} {}", human(i.bytes), i.kind.label(), i.path.display());
                }
            }
            if dry_run {
                println!("\nDry run: nothing moved.");
                docker_hint(&dock, docker);
                return;
            }

            if !files.is_empty() {
                let total: u64 = files.iter().map(|i| i.bytes).sum();
                if yes || confirm(&format!("\nMove {} to quarantine? You can undo for {} days. [y/N] ", human(total), quarantine::KEEP_DAYS)) {
                    let list: Vec<_> = files
                        .iter()
                        .map(|i| (i.path.clone(), i.bytes, i.kind.label().to_string()))
                        .collect();
                    match quarantine::quarantine(&base, &list) {
                        Ok(out) => {
                            for (p, why) in &out.skipped {
                                eprintln!("skipped {}: {why}", p.display());
                            }
                            if out.batch.entries.is_empty() {
                                println!("Nothing was moved.");
                            } else {
                                println!(
                                    "\nMoved {} items ({}) to quarantine as batch {}.\nUndo with `sweepr undo`. They're deleted for good after {} days, or now with `sweepr purge --all`.",
                                    out.batch.entries.len(),
                                    human(out.batch.bytes()),
                                    out.batch.id,
                                    quarantine::KEEP_DAYS
                                );
                            }
                        }
                        Err(e) => fail(&format!("could not create quarantine at {}: {e}", base.display())),
                    }
                } else {
                    println!("Skipped. Nothing moved to quarantine.");
                }
            }

            if docker && !dock.is_empty() {
                let total: u64 = dock.iter().map(|i| i.bytes).sum();
                if yes || confirm(&format!("\nPrune {} of Docker data? This can't be undone. [y/N] ", human(total))) {
                    let refs: Vec<_> = dock.iter().collect();
                    for args in docker::prune_commands(&refs) {
                        match docker::run(&args) {
                            Ok(()) => println!("ran docker {}", args.join(" ")),
                            Err(e) => eprintln!("docker {} failed: {e}", args.join(" ")),
                        }
                    }
                } else {
                    println!("Skipped Docker.");
                }
            }
            docker_hint(&dock, docker);
        }
        Cmd::Undo { id } => {
            let id = match id.or_else(|| quarantine::list(&base).first().map(|b| b.id.clone())) {
                Some(id) => id,
                None => {
                    println!("Quarantine is empty, nothing to undo.");
                    return;
                }
            };
            match quarantine::restore(&base, &id) {
                Ok(out) => {
                    for p in &out.restored {
                        println!("restored {}", p.display());
                    }
                    for (p, why) in &out.failed {
                        eprintln!("kept in quarantine {}: {why}", p.display());
                    }
                    println!("Restored {} items from batch {id}.", out.restored.len());
                }
                Err(e) => fail(&format!("no batch {id}: {e}")),
            }
        }
        Cmd::Quarantine => {
            let batches = quarantine::list(&base);
            if batches.is_empty() {
                println!("Quarantine is empty.");
                return;
            }
            println!("{:<14} {:>10} {:>6}  {}", "BATCH", "SIZE", "ITEMS", "AGE");
            for b in &batches {
                let left = quarantine::KEEP_DAYS.saturating_sub(b.age_days());
                println!("{:<14} {:>10} {:>6}  {} days old, {} left to undo", b.id, human(b.bytes()), b.entries.len(), b.age_days(), left);
            }
        }
        Cmd::Purge { all } => {
            if all && !confirm("Permanently delete everything in quarantine? This can't be undone. [y/N] ") {
                println!("Cancelled.");
                return;
            }
            match quarantine::purge(&base, all) {
                Ok((n, bytes)) => println!("Deleted {n} batches for good, freeing {}.", human(bytes)),
                Err(e) => fail(&format!("purge failed: {e}")),
            }
        }
    }
}

fn run_scan(a: ScanArgs) -> Vec<scan::Item> {
    let roots = if a.paths.is_empty() {
        vec![dirs::home_dir().expect("no home folder found")]
    } else {
        a.paths
    };
    let mut items = scan::scan(&scan::Options {
        roots,
        idle_days: a.idle_days,
        large_file_mb: a.large_mb,
        include_global: !a.no_global,
    });
    if !a.no_global && !a.no_docker {
        items.extend(docker::detect());
        items.sort_by(|x, y| y.bytes.cmp(&x.bytes));
    }
    // Never offer anything already sitting in quarantine.
    let q = quarantine::base_dir();
    items.retain(|i| !i.path.starts_with(&q));
    items
}

fn docker_hint(dock: &[scan::Item], docker: bool) {
    if !docker && !dock.is_empty() {
        let total: u64 = dock.iter().map(|i| i.bytes).sum();
        println!("\n{} Docker can free ({}) left alone. Add --docker to prune them too.", plural(dock.len()), human(total));
    }
}

fn plural(n: usize) -> String {
    if n == 1 { "1 item".into() } else { format!("{n} items") }
}

fn auto_purge(base: &std::path::Path) {
    if let Ok((n, bytes)) = quarantine::purge(base, false) {
        if n > 0 {
            println!("Cleared {n} expired quarantine batches ({}).\n", human(bytes));
        }
    }
}

fn confirm(prompt: &str) -> bool {
    print!("{prompt}");
    let _ = io::stdout().flush();
    let mut line = String::new();
    io::stdin().lock().read_line(&mut line).ok();
    matches!(line.trim().to_ascii_lowercase().as_str(), "y" | "yes")
}

fn fail(msg: &str) -> ! {
    eprintln!("sweepr: {msg}");
    std::process::exit(1)
}

fn print_report(items: &[scan::Item], top: usize) {
    for (bucket, title) in [(Bucket::Safe, "SAFE TO CLEAN"), (Bucket::NeedsYou, "NEEDS YOU")] {
        let rows: Vec<_> = items.iter().filter(|i| i.bucket == bucket).collect();
        let total: u64 = rows.iter().map(|i| i.bytes).sum();
        println!("\n{title}  {}  ({} items)", human(total), rows.len());
        println!("{}", "-".repeat(72));
        for i in rows.iter().take(top) {
            println!("{:>10}  {:<22} {}", human(i.bytes), i.kind.label(), i.path.display());
            println!("{:>10}  {}", "", i.reason);
        }
        if rows.len() > top {
            println!("{:>10}  … {} more (use --top)", "", rows.len() - top);
        }
    }
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
