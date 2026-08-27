CREATE TABLE realities (
    id TEXT PRIMARY KEY NOT NULL,
    created_at TEXT NOT NULL,
    data TEXT NOT NULL CHECK (json_valid(data))
);
CREATE TABLE executions (
    id TEXT PRIMARY KEY NOT NULL,
    reality_id TEXT NOT NULL REFERENCES realities(id),
    created_at TEXT NOT NULL,
    data TEXT NOT NULL CHECK (json_valid(data))
);
CREATE INDEX executions_reality ON executions(reality_id);
CREATE TRIGGER executions_immutable_update BEFORE UPDATE ON executions
BEGIN SELECT RAISE(ABORT, 'execution records are append-only'); END;
CREATE TRIGGER executions_immutable_delete BEFORE DELETE ON executions
BEGIN SELECT RAISE(ABORT, 'execution records are append-only'); END;
