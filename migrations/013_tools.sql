CREATE TABLE IF NOT EXISTS tool_definitions (
  id TEXT PRIMARY KEY,
  name TEXT NOT NULL,
  version TEXT NOT NULL,
  manifest_hash TEXT NOT NULL UNIQUE,
  artifact_hash TEXT,
  trust TEXT NOT NULL,
  disabled INTEGER NOT NULL DEFAULT 0,
  registered_at TEXT NOT NULL,
  data TEXT NOT NULL
);
CREATE UNIQUE INDEX IF NOT EXISTS tool_definitions_name_version_idx
  ON tool_definitions(name, version);

CREATE TABLE IF NOT EXISTS micro_sandboxes (
  id TEXT PRIMARY KEY,
  reality_id TEXT NOT NULL REFERENCES realities(id),
  tool_id TEXT NOT NULL REFERENCES tool_definitions(id),
  created_at TEXT NOT NULL,
  destroyed_at TEXT,
  data TEXT NOT NULL
);
CREATE INDEX IF NOT EXISTS micro_sandboxes_reality_idx
  ON micro_sandboxes(reality_id, created_at, id);

CREATE TABLE IF NOT EXISTS execution_attestations (
  id TEXT PRIMARY KEY,
  reality_id TEXT NOT NULL REFERENCES realities(id),
  sandbox_id TEXT NOT NULL REFERENCES micro_sandboxes(id),
  tool_id TEXT NOT NULL REFERENCES tool_definitions(id),
  attestation_hash TEXT NOT NULL UNIQUE,
  created_at TEXT NOT NULL,
  data TEXT NOT NULL
);
CREATE INDEX IF NOT EXISTS execution_attestations_tool_idx
  ON execution_attestations(tool_id, created_at, id);

CREATE TABLE IF NOT EXISTS tool_lifecycle_events (
  id TEXT PRIMARY KEY,
  tool_id TEXT,
  sandbox_id TEXT,
  event TEXT NOT NULL,
  created_at TEXT NOT NULL,
  data TEXT NOT NULL
);

CREATE TRIGGER IF NOT EXISTS tool_definitions_immutable_update
BEFORE UPDATE OF manifest_hash, artifact_hash, registered_at, data ON tool_definitions
BEGIN SELECT RAISE(ABORT, 'tool definition provenance is immutable'); END;
CREATE TRIGGER IF NOT EXISTS execution_attestations_immutable_update
BEFORE UPDATE ON execution_attestations BEGIN SELECT RAISE(ABORT, 'execution attestations are immutable'); END;
CREATE TRIGGER IF NOT EXISTS execution_attestations_immutable_delete
BEFORE DELETE ON execution_attestations BEGIN SELECT RAISE(ABORT, 'execution attestations are immutable'); END;
