CREATE TABLE IF NOT EXISTS capability_manifests (
  id TEXT PRIMARY KEY,
  profile TEXT NOT NULL,
  revision INTEGER NOT NULL CHECK(revision > 0),
  manifest_hash TEXT NOT NULL UNIQUE,
  created_at TEXT NOT NULL,
  data TEXT NOT NULL
);

CREATE TABLE IF NOT EXISTS reality_manifest_history (
  reality_id TEXT NOT NULL REFERENCES realities(id),
  manifest_id TEXT NOT NULL REFERENCES capability_manifests(id),
  revision INTEGER NOT NULL,
  effective_at TEXT NOT NULL,
  revoked_at TEXT,
  PRIMARY KEY(reality_id, revision)
);

CREATE TABLE IF NOT EXISTS reality_provider_runtime (
  reality_id TEXT PRIMARY KEY REFERENCES realities(id),
  provider TEXT NOT NULL,
  created_at TEXT NOT NULL,
  data TEXT NOT NULL
);

CREATE TABLE IF NOT EXISTS capability_events (
  id TEXT PRIMARY KEY,
  reality_id TEXT NOT NULL REFERENCES realities(id),
  manifest_id TEXT NOT NULL REFERENCES capability_manifests(id),
  kind TEXT NOT NULL,
  created_at TEXT NOT NULL,
  data TEXT NOT NULL
);
CREATE INDEX IF NOT EXISTS capability_events_reality_idx
  ON capability_events(reality_id, created_at, id);

CREATE TABLE IF NOT EXISTS issued_credentials (
  id TEXT PRIMARY KEY,
  reality_id TEXT NOT NULL REFERENCES realities(id),
  provider TEXT NOT NULL,
  issued_at TEXT NOT NULL,
  revoked_at TEXT,
  data TEXT NOT NULL
);

CREATE TABLE IF NOT EXISTS capability_token_audit (
  token_hash TEXT PRIMARY KEY,
  reality_id TEXT NOT NULL REFERENCES realities(id),
  manifest_id TEXT NOT NULL REFERENCES capability_manifests(id),
  expires_at TEXT NOT NULL,
  revoked_at TEXT
);

CREATE TABLE IF NOT EXISTS capability_benchmark_runs (
  id TEXT PRIMARY KEY,
  created_at TEXT NOT NULL,
  data TEXT NOT NULL
);

CREATE TRIGGER IF NOT EXISTS capability_manifests_immutable_update
BEFORE UPDATE ON capability_manifests BEGIN SELECT RAISE(ABORT, 'capability manifests are immutable'); END;
CREATE TRIGGER IF NOT EXISTS capability_manifests_immutable_delete
BEFORE DELETE ON capability_manifests BEGIN SELECT RAISE(ABORT, 'capability manifests are immutable'); END;
CREATE TRIGGER IF NOT EXISTS capability_events_immutable_update
BEFORE UPDATE ON capability_events BEGIN SELECT RAISE(ABORT, 'capability events are immutable'); END;
CREATE TRIGGER IF NOT EXISTS capability_events_immutable_delete
BEFORE DELETE ON capability_events BEGIN SELECT RAISE(ABORT, 'capability events are immutable'); END;
