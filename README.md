# Sweepr

**Dev junk, explained.** Sweepr scans your laptop for the gigabytes your development work leaves behind and sorts them into two lists:

- **Safe to clean**: things that come back on their own (`node_modules`, Rust `target/`, virtualenvs, `__pycache__`, Gradle build output, `.next/`, package caches) in projects you haven't touched for a while.
- **Needs you**: things only you can judge (large files, old installers in Downloads, Android emulator images, and anything inside a repo with uncommitted or unpushed work).

> Status: early. Sweepr only **reports** for now. It never deletes anything.

## Try it

```sh
cargo run -- scan                 # scan your home folder
cargo run -- scan ~/code          # scan one folder
cargo run -- scan --idle-days 60  # call a project idle after 60 days
cargo run -- scan --json          # machine-readable output
```

Example:

```
SAFE TO CLEAN  14.2 GB  (46 items)
------------------------------------------------------------------------
    2.1 GB  node_modules           ~/code/remit-app/node_modules
            project idle 240 days · restore with npm / pnpm / yarn install
...
NEEDS YOU  52.4 GB  (12 items)
------------------------------------------------------------------------
   18.7 GB  Android emulator image ~/.android/avd/Pixel_7_API_34.avd
            last used 6 days ago, holds emulator data
```

## How it decides

| Found | Bucket | Why |
|---|---|---|
| Build folders in a project idle ≥ 30 days | Safe | Rebuilt by `npm install`, `cargo build`, etc. |
| Build folders in a recently active project | Needs you | You may still be using them |
| Anything in a repo with uncommitted or unpushed work | Needs you | Never auto-cleaned |
| Package caches (npm, pip, yarn, pnpm, cargo, Gradle, Maven) | Safe | Re-downloaded on demand |
| Files ≥ 500 MB, old installers, emulator images | Needs you | Not restorable, or hold your data |

## Roadmap

1. ~~Scanner and size report~~
2. Clean command with a 7-day quarantine and undo
3. Docker images, volumes and build cache
4. AI notes that explain every item in plain words (metadata only, never file contents)
5. Terminal UI, then a desktop app
6. One-line installers on GitHub Releases

## License

MIT
