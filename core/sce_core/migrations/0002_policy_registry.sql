ALTER TABLE analysis_runs
ADD COLUMN constitution_version TEXT NOT NULL DEFAULT 'active';

CREATE TABLE IF NOT EXISTS policy_canary_state (
  project_id TEXT PRIMARY KEY,
  candidate_version TEXT NOT NULL,
  remaining_budget INTEGER NOT NULL CHECK (remaining_budget >= 0),
  started_at TEXT NOT NULL,
  updated_at TEXT NOT NULL,
  FOREIGN KEY(project_id) REFERENCES projects(id)
);
