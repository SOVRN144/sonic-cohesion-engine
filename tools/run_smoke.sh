#!/usr/bin/env bash
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
PORT=9926
WAV_PEAK_DBFS="-12"
WAV_RMS_DBFS=""
KEEP_TEMP=0

usage() {
  cat <<'EOF'
Single-command local smoke runner for SCE Commit 3.

Usage:
  bash tools/run_smoke.sh [--port <port>] [--wav-peak-dbfs <db>] [--wav-rms-dbfs <db>] [--keep-temp]

Notes:
  - WAV fixture is generated via tools/generate_smoke_wav.py (never full-scale by default).
  - If both --wav-peak-dbfs and --wav-rms-dbfs are provided, the last one wins.
EOF
}

while (($# > 0)); do
  case "$1" in
    --port)
      PORT="${2:?missing port}"
      shift 2
      ;;
    --wav-peak-dbfs)
      WAV_PEAK_DBFS="${2:?missing dBFS value}"
      WAV_RMS_DBFS=""
      shift 2
      ;;
    --wav-rms-dbfs)
      WAV_RMS_DBFS="${2:?missing dBFS value}"
      shift 2
      ;;
    --keep-temp)
      KEEP_TEMP=1
      shift
      ;;
    -h|--help)
      usage
      exit 0
      ;;
    *)
      echo "Unknown argument: $1" >&2
      usage >&2
      exit 2
      ;;
  esac
done

TMPDIR="$(mktemp -d)"
DB_PATH="$TMPDIR/smoke.db"
PROJECT_ROOT="$TMPDIR/project"
AUDIO_DIR="$PROJECT_ROOT/audio"
WAV_PATH="$AUDIO_DIR/smoke.wav"
MP3_PATH="$AUDIO_DIR/tiny.mp3"
FIXTURE_MP3="$ROOT_DIR/core/sce_core/tests/fixtures/tiny.mp3"

mkdir -p "$AUDIO_DIR"

if [[ -n "$WAV_RMS_DBFS" ]]; then
  python3 "$ROOT_DIR/tools/generate_smoke_wav.py" "$WAV_PATH" --rms-dbfs "$WAV_RMS_DBFS"
else
  python3 "$ROOT_DIR/tools/generate_smoke_wav.py" "$WAV_PATH" --peak-dbfs "$WAV_PEAK_DBFS"
fi

cp "$FIXTURE_MP3" "$MP3_PATH"

cleanup() {
  if [[ -n "${DAEMON_PID:-}" ]]; then
    kill "$DAEMON_PID" >/dev/null 2>&1 || true
    wait "$DAEMON_PID" 2>/dev/null || true
  fi
  if [[ "$KEEP_TEMP" -eq 0 ]]; then
    rm -rf "$TMPDIR"
  fi
}
trap cleanup EXIT

pushd "$ROOT_DIR" >/dev/null
cargo run -p sce_daemon --bin sce_daemon -- --db-url "sqlite://$DB_PATH?mode=rwc" --bind "127.0.0.1:$PORT" >"$TMPDIR/daemon.log" 2>&1 &
DAEMON_PID=$!
popd >/dev/null

for _ in $(seq 1 120); do
  if curl -sf "http://127.0.0.1:$PORT/v1/health" >/dev/null; then
    break
  fi
  sleep 0.25
done
curl -sf "http://127.0.0.1:$PORT/v1/health" >/dev/null

PROJECT_ID=$(
  curl -sS -X POST "http://127.0.0.1:$PORT/v1/projects" \
    -H 'content-type: application/json' \
    -d "{\"name\":\"smoke\",\"root_path\":\"$PROJECT_ROOT\"}" \
  | python3 -c 'import json,sys; print(json.load(sys.stdin)["project_id"])'
)

WAV_RUN_ID=$(
  curl -sS -X POST "http://127.0.0.1:$PORT/v1/projects/$PROJECT_ID/assets" \
    -H 'content-type: application/json' \
    -d "{\"file_path\":\"$WAV_PATH\",\"kind\":\"mix\"}" \
  | python3 -c 'import json,sys; print(json.load(sys.stdin)["queued_run_id"])'
)

MP3_RUN_ID=$(
  curl -sS -X POST "http://127.0.0.1:$PORT/v1/projects/$PROJECT_ID/assets" \
    -H 'content-type: application/json' \
    -d "{\"file_path\":\"$MP3_PATH\",\"kind\":\"mix\"}" \
  | python3 -c 'import json,sys; print(json.load(sys.stdin)["queued_run_id"])'
)

python3 - <<'PY' "$DB_PATH" "$PROJECT_ROOT" "$WAV_RUN_ID" "$MP3_RUN_ID" "$TMPDIR"
import json
import os
import sqlite3
import sys
import time

db_path, project_root, wav_run_id, mp3_run_id, temp_root = sys.argv[1:6]
conn = sqlite3.connect(db_path)
conn.row_factory = sqlite3.Row

def wait_done(run_id: str, timeout_sec: float = 45.0):
    deadline = time.time() + timeout_sec
    while time.time() < deadline:
        row = conn.execute(
            "SELECT status, error_message, asset_id FROM analysis_runs WHERE id=?",
            (run_id,),
        ).fetchone()
        if row and row["status"] in ("done", "failed"):
            return row
        time.sleep(0.25)
    raise RuntimeError(f"timeout waiting for run {run_id}")

def snapshot_for(run_id: str):
    row = wait_done(run_id)
    if row["status"] != "done":
        raise RuntimeError(f"run {run_id} failed: {row['error_message']}")

    run_dir = os.path.join(
        project_root, "SCE", "assets", row["asset_id"], "analysis", run_id
    )
    metrics_path = os.path.join(run_dir, "metrics.json")
    gates_path = os.path.join(run_dir, "gates.json")
    drift_path = os.path.join(run_dir, "drift.json")
    report_path = os.path.join(run_dir, "report.json")

    for path in (metrics_path, gates_path, drift_path, report_path):
        if not os.path.exists(path):
            raise RuntimeError(f"missing artifact: {path}")

    with open(metrics_path, "r", encoding="utf-8") as f:
        metrics = json.load(f)["metrics"]
    with open(report_path, "r", encoding="utf-8") as f:
        summary = json.load(f)["summary"]

    return {
        "run_id": run_id,
        "sample_rate_hz": metrics["sample_rate_hz"],
        "channels": metrics["channels"],
        "sample_peak_dbfs": metrics["sample_peak_dbfs"],
        "true_peak_dbtp": metrics["true_peak_dbtp"],
        "integrated_lufs": metrics["integrated_lufs"],
        "lossy_source": metrics["lossy_source"],
        "gate_status": summary["gate_status"],
        "drift_score": summary["drift_score"],
        "run_dir": run_dir,
    }

out = {
    "temp_root": temp_root,
    "project_root": project_root,
    "runs": {
        "wav": snapshot_for(wav_run_id),
        "mp3": snapshot_for(mp3_run_id),
    },
}
print(json.dumps(out, indent=2))
PY
