mod rules;
mod scan;

use clap::{Parser, Subcommand};
use rules::Bucket;
use std::path::PathBuf;

/// Find the junk your dev work left behind, sorted into safe-to-clean and needs-you.
/// Sweepr only reports for now: it never deletes anything.
#[derive(Parser)]
#[command(name = "sweepr", version, about)]
struct Cli {
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// Scan folders and print what can be cleaned.
    Scan {
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
        /// Print JSON instead of the report.
        #[arg(long)]
        json: bool,
        /// Rows to show per bucket.
        #[arg(long, default_value_t = 25)]
        top: usize,
    },
}

fn main() {
    let Cli { cmd } = Cli::parse();
    match cmd {
        Cmd::Scan { paths, idle_days, large_mb, no_global, json, top } => {
            let roots = if paths.is_empty() {
                vec![dirs::home_dir().expect("no home folder found")]
            } else {
                paths
            };
            let started = std::time::Instant::now();
            let items = scan::scan(&scan::Options {
                roots,
                idle_days,
                large_file_mb: large_mb,
                include_global: !no_global,
            });
            if json {
                println!("{}", serde_json::to_string_pretty(&items).unwrap());
                return;
            }
            print_report(&items, top, started.elapsed());
        }
    }
}

fn print_report(items: &[scan::Item], top: usize, took: std::time::Duration) {
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
    let safe: u64 = items.iter().filter(|i| i.bucket == Bucket::Safe).map(|i| i.bytes).sum();
    println!("\nScanned in {:.1}s. {} could be freed safely. Nothing was deleted.", took.as_secs_f64(), human(safe));
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
