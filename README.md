# Sweepr

**Dev junk, explained.** Sweepr scans your laptop for the gigabytes your development work leaves behind and sorts them into two lists:

- **Safe to clean**: things that come back on their own (`node_modules`, Rust `target/`, virtualenvs, `__pycache__`, Gradle build output, `.next/`, package caches) in projects you haven't touched for a while.
- **Needs you**: things only you can judge (large files, old installers in Downloads, Android emulator images, and anything inside a repo with uncommitted or unpushed work).

> Status: early. `sweepr clean` never deletes anything outright: it moves items to a quarantine you can undo for 7 days.

## Try it

```sh
cargo run -- scan                 # scan your home folder
cargo run -- scan ~/code          # scan one folder
cargo run -- scan --idle-days 60  # call a project idle after 60 days
cargo run -- scan --json          # machine-readable output

cargo run -- clean --dry-run      # see what clean would move
cargo run -- clean                # move every Safe item to quarantine (asks first)
cargo run -- quarantine           # list cleaned batches and days left to undo
cargo run -- undo                 # put the latest batch back
cargo run -- purge                # delete batches older than 7 days for good
cargo run -- clean --docker       # also prune Docker dangling images and build cache
```

### Notes, with or without AI

```sh
cargo run -- scan --notes                # a plain-words note per item, written locally (offline)
cargo run -- scan --ai                   # notes written by an AI model
cargo run -- scan --show-ai-payload      # print exactly what --ai would send, send nothing
```

`--ai` talks to any OpenAI-compatible chat API. By default it uses [Ollama](https://ollama.com) on your own machine (`http://localhost:11434/v1`, model `llama3.2`), so nothing leaves your laptop. Point it elsewhere with:

```sh
export SWEEPR_AI_URL=https://api.openai.com/v1   # or Groq, OpenRouter, LM Studio, ...
export SWEEPR_AI_MODEL=gpt-4o-mini
export SWEEPR_AI_KEY=...
```

**Privacy:** only metadata is sent: the item's kind, size, age, bucket, the project folder's name, and a file's extension. Never file contents, full paths or file names. If the model can't be reached, Sweepr falls back to the built-in notes.

### Docker

When Docker is installed and running, `scan` reads it with `docker system df -v` (use `--no-docker` to skip):

| Item | Bucket | Why |
|---|---|---|
| Dangling images (`<none>:<none>`, no container) | Safe to clean | Leftovers from rebuilds |
| Build cache not in use | Safe to clean | Docker rebuilds it on the next build |
| Tagged images no container uses | Needs you | You may still run them |
| Volumes with no container attached | Needs you | Often hold database data |
| Stopped containers | Needs you | Changes inside them are lost on removal |

Docker data can't be quarantined, so `clean` only prunes it when you pass `--docker`, asks separately, and only runs `docker image prune -f` and `docker builder prune -a -f`. Volumes, containers and tagged images are never removed by Sweepr.

`clean` only ever touches the **Safe to clean** list. **Needs you** items are never moved. Undo refuses to overwrite anything that has since appeared at the original path, and items on another drive are skipped rather than copied. Quarantine lives in your OS data folder (set `SWEEPR_HOME` to move it); expired batches are cleared the next time you run `clean`.

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
2. ~~Clean command with a 7-day quarantine and undo~~
3. ~~Docker images, volumes and build cache~~
4. ~~AI notes that explain every item in plain words (metadata only, never file contents)~~
5. Terminal UI, then a desktop app
6. One-line installers on GitHub Releases

## License

MIT
