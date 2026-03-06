# sonic-cohesion-engine (Commit 1 + 1.1 baseline)

Rust control-plane baseline for SCE:
- `sce_core` library: API, ingest, queue worker, reports, migrations
- `sce_daemon` binary: local daemon with graceful shutdown

## Run

```bash
cargo run -p sce_daemon --bin sce_daemon
```

With explicit DB URL:

```bash
cargo run -p sce_daemon --bin sce_daemon -- --db-url "sqlite://./sce.db?mode=rwc" --bind 127.0.0.1:9911
```

## API

- `GET /v1/health`
- `POST /v1/projects`
- `GET /v1/projects`
- `POST /v1/projects/:project_id/assets`

## Smoke WAV Fixture

Use a nominal-level WAV during smoke checks so governance outcomes are informative:

```bash
python3 tools/generate_smoke_wav.py /tmp/sce-smoke.wav --peak-dbfs -12
```

Alternative (equivalent style target):

```bash
python3 tools/generate_smoke_wav.py /tmp/sce-smoke.wav --rms-dbfs -18
```

Single-command smoke runner (daemon + project + WAV/MP3 register + artifact checks):

```bash
bash tools/run_smoke.sh
```

## CLL Trends CLI

Generate Commit 4 project trends and diversity alerts:

```bash
cargo run -p sce_daemon --bin sce_cli -- trends --project-root /path/to/project --last-n 25
```

Use full valid history (all valid runs):

```bash
cargo run -p sce_daemon --bin sce_cli -- trends --project-root /path/to/project --last-n 0
```

Foreground watcher (explicit interval, governance-only):

```bash
cargo run -p sce_daemon --bin sce_cli -- watch-trends --project-root /path/to/project --last-n 25 --interval-seconds 30
```

Watcher with full valid history:

```bash
cargo run -p sce_daemon --bin sce_cli -- watch-trends --project-root /path/to/project --last-n 0 --interval-seconds 30
```

Advisory constitution suggestion:

```bash
cargo run -p sce_daemon --bin sce_cli -- suggest-constitution --project-root /path/to/project --last-n 25
```

Suggestion with full valid history:

```bash
cargo run -p sce_daemon --bin sce_cli -- suggest-constitution --project-root /path/to/project --last-n 0
```

## Notes

- Migration strategy is clean-slate for this baseline: `0001_init.sql` already includes Commit 1.1 hardening fields/constraints.
- Atomic claim uses `UPDATE ... RETURNING`; startup hard-fails when SQLite `< 3.35.0`.
