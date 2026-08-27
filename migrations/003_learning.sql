CREATE TABLE hypotheses (
    id TEXT PRIMARY KEY NOT NULL,
    source_experience TEXT NOT NULL REFERENCES experiences(id),
    data TEXT NOT NULL CHECK(json_valid(data))
);
CREATE TABLE lessons (
    id TEXT PRIMARY KEY NOT NULL,
    source_experience TEXT NOT NULL REFERENCES experiences(id),
    hypothesis_id TEXT NOT NULL UNIQUE REFERENCES hypotheses(id),
    version INTEGER NOT NULL CHECK(version >= 1),
    created_at TEXT NOT NULL,
    data TEXT NOT NULL CHECK(json_valid(data))
);
CREATE TABLE lesson_versions (
    lesson_id TEXT NOT NULL REFERENCES lessons(id),
    version INTEGER NOT NULL,
    data TEXT NOT NULL CHECK(json_valid(data)),
    PRIMARY KEY(lesson_id, version)
);
CREATE TABLE experiments (
    id TEXT PRIMARY KEY NOT NULL,
    source_experience TEXT NOT NULL REFERENCES experiences(id),
    lesson_id TEXT NOT NULL REFERENCES lessons(id),
    hypothesis_id TEXT NOT NULL REFERENCES hypotheses(id),
    created_at TEXT NOT NULL,
    status TEXT NOT NULL CHECK(status IN ('running','completed','interrupted','failed')),
    data TEXT NOT NULL CHECK(json_valid(data))
);
CREATE INDEX experiments_lesson ON experiments(lesson_id,created_at,id);
CREATE TABLE trials (
    id TEXT PRIMARY KEY NOT NULL,
    experiment_id TEXT NOT NULL REFERENCES experiments(id),
    position INTEGER NOT NULL CHECK(position IN (0,1)),
    experience_id TEXT NOT NULL UNIQUE REFERENCES experiences(id),
    reality_id TEXT NOT NULL REFERENCES realities(id),
    execution_id TEXT NOT NULL REFERENCES executions(id),
    evaluation_id TEXT NOT NULL REFERENCES evaluations(id),
    data TEXT NOT NULL CHECK(json_valid(data)),
    UNIQUE(experiment_id,position),
    UNIQUE(experiment_id,id)
);
CREATE TABLE trial_artifacts (
    trial_id TEXT NOT NULL REFERENCES trials(id),
    experience_id TEXT NOT NULL,
    path TEXT NOT NULL,
    PRIMARY KEY(trial_id,path),
    FOREIGN KEY(experience_id,path) REFERENCES experience_artifacts(experience_id,path)
);
CREATE TABLE lesson_evidence (
    lesson_id TEXT NOT NULL REFERENCES lessons(id),
    evidence_key TEXT NOT NULL,
    experience_id TEXT REFERENCES experiences(id),
    experiment_id TEXT,
    trial_id TEXT,
    relationship TEXT NOT NULL CHECK(relationship IN ('origin','supports','contradicts','inconclusive')),
    PRIMARY KEY(lesson_id,evidence_key),
    FOREIGN KEY(experiment_id,trial_id) REFERENCES trials(experiment_id,id),
    CHECK((experience_id IS NOT NULL AND experiment_id IS NULL AND trial_id IS NULL) OR
          (experience_id IS NULL AND experiment_id IS NOT NULL AND trial_id IS NOT NULL))
);
CREATE TRIGGER hypotheses_immutable_update BEFORE UPDATE ON hypotheses
BEGIN SELECT RAISE(ABORT, 'hypotheses are immutable'); END;
CREATE TRIGGER hypotheses_immutable_delete BEFORE DELETE ON hypotheses
BEGIN SELECT RAISE(ABORT, 'hypotheses are immutable'); END;
CREATE TRIGGER lesson_versions_immutable_update BEFORE UPDATE ON lesson_versions
BEGIN SELECT RAISE(ABORT, 'lesson revisions are immutable'); END;
CREATE TRIGGER lesson_versions_immutable_delete BEFORE DELETE ON lesson_versions
BEGIN SELECT RAISE(ABORT, 'lesson revisions are immutable'); END;
CREATE TRIGGER trials_immutable_update BEFORE UPDATE ON trials
BEGIN SELECT RAISE(ABORT, 'trials are immutable'); END;
CREATE TRIGGER trials_immutable_delete BEFORE DELETE ON trials
BEGIN SELECT RAISE(ABORT, 'trials are immutable'); END;
CREATE TRIGGER trial_artifacts_immutable_update BEFORE UPDATE ON trial_artifacts
BEGIN SELECT RAISE(ABORT, 'trial artifacts are immutable'); END;
CREATE TRIGGER trial_artifacts_immutable_delete BEFORE DELETE ON trial_artifacts
BEGIN SELECT RAISE(ABORT, 'trial artifacts are immutable'); END;
CREATE TRIGGER lesson_evidence_immutable_update BEFORE UPDATE ON lesson_evidence
BEGIN SELECT RAISE(ABORT, 'lesson evidence is immutable'); END;
CREATE TRIGGER lesson_evidence_immutable_delete BEFORE DELETE ON lesson_evidence
BEGIN SELECT RAISE(ABORT, 'lesson evidence is immutable'); END;
CREATE TRIGGER experiments_terminal BEFORE UPDATE ON experiments WHEN OLD.status != 'running'
BEGIN SELECT RAISE(ABORT, 'completed experiments are immutable'); END;
CREATE TRIGGER experiments_immutable_delete BEFORE DELETE ON experiments
BEGIN SELECT RAISE(ABORT, 'experiments cannot be deleted'); END;
