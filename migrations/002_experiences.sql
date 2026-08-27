CREATE TABLE evaluations (
    id TEXT PRIMARY KEY NOT NULL,
    execution_id TEXT NOT NULL UNIQUE REFERENCES executions(id),
    data TEXT NOT NULL CHECK(json_valid(data))
);
CREATE TABLE experiences (
    id TEXT PRIMARY KEY NOT NULL,
    created_at TEXT NOT NULL,
    reality_id TEXT NOT NULL REFERENCES realities(id),
    execution_id TEXT NOT NULL UNIQUE REFERENCES executions(id),
    evaluation_id TEXT NOT NULL UNIQUE REFERENCES evaluations(id),
    outcome TEXT NOT NULL,
    data TEXT NOT NULL CHECK(json_valid(data))
);
CREATE INDEX experiences_outcome ON experiences(outcome, created_at, id);
CREATE TABLE experience_artifacts (
    experience_id TEXT NOT NULL REFERENCES experiences(id),
    path TEXT NOT NULL,
    blake3 TEXT NOT NULL,
    bytes INTEGER NOT NULL CHECK(bytes >= 0),
    kind TEXT NOT NULL,
    PRIMARY KEY(experience_id, path)
);
CREATE TRIGGER experiences_immutable_update BEFORE UPDATE ON experiences
BEGIN SELECT RAISE(ABORT, 'experiences are immutable'); END;
CREATE TRIGGER experiences_immutable_delete BEFORE DELETE ON experiences
BEGIN SELECT RAISE(ABORT, 'experiences are immutable'); END;
CREATE TRIGGER evaluations_immutable_update BEFORE UPDATE ON evaluations
BEGIN SELECT RAISE(ABORT, 'evaluations are immutable'); END;
CREATE TRIGGER evaluations_immutable_delete BEFORE DELETE ON evaluations
BEGIN SELECT RAISE(ABORT, 'evaluations are immutable'); END;
CREATE TRIGGER experience_artifacts_immutable_update BEFORE UPDATE ON experience_artifacts
BEGIN SELECT RAISE(ABORT, 'artifact references are immutable'); END;
CREATE TRIGGER experience_artifacts_immutable_delete BEFORE DELETE ON experience_artifacts
BEGIN SELECT RAISE(ABORT, 'artifact references are immutable'); END;
