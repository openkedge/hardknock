CREATE TABLE lesson_applications (
    id TEXT PRIMARY KEY NOT NULL,
    lesson_id TEXT NOT NULL,
    lesson_version INTEGER NOT NULL,
    experience_id TEXT NOT NULL REFERENCES experiences(id),
    created_at TEXT NOT NULL,
    relevance REAL NOT NULL CHECK(relevance BETWEEN 0 AND 1),
    influence TEXT NOT NULL,
    verification TEXT NOT NULL,
    data TEXT NOT NULL CHECK(json_valid(data)),
    FOREIGN KEY(lesson_id,lesson_version) REFERENCES lesson_versions(lesson_id,version),
    UNIQUE(lesson_id,experience_id)
);
CREATE INDEX applications_experience ON lesson_applications(experience_id);
CREATE INDEX applications_lesson ON lesson_applications(lesson_id,created_at,id);
CREATE TABLE application_artifacts (
    application_id TEXT NOT NULL REFERENCES lesson_applications(id),
    experience_id TEXT NOT NULL,
    path TEXT NOT NULL,
    PRIMARY KEY(application_id,path),
    FOREIGN KEY(experience_id,path) REFERENCES experience_artifacts(experience_id,path)
);
CREATE TABLE experience_relations (
    source_experience_id TEXT NOT NULL REFERENCES experiences(id),
    target_experience_id TEXT NOT NULL REFERENCES experiences(id),
    relation_type TEXT NOT NULL CHECK(relation_type IN ('retry_of','counterfactual_of','transfer_from')),
    PRIMARY KEY(source_experience_id,target_experience_id,relation_type),
    CHECK(source_experience_id != target_experience_id)
);
CREATE TABLE repeated_mistakes (
    experience_id TEXT NOT NULL REFERENCES experiences(id),
    lesson_id TEXT NOT NULL REFERENCES lessons(id),
    data TEXT NOT NULL CHECK(json_valid(data)),
    PRIMARY KEY(experience_id,lesson_id)
);
CREATE TABLE lesson_validations (
    lesson_id TEXT NOT NULL,
    lesson_version INTEGER NOT NULL,
    policy TEXT NOT NULL,
    data TEXT NOT NULL CHECK(json_valid(data)),
    PRIMARY KEY(lesson_id,lesson_version),
    FOREIGN KEY(lesson_id,lesson_version) REFERENCES lesson_versions(lesson_id,version)
);
CREATE TRIGGER applications_immutable_update BEFORE UPDATE ON lesson_applications
BEGIN SELECT RAISE(ABORT, 'applications are immutable'); END;
CREATE TRIGGER applications_immutable_delete BEFORE DELETE ON lesson_applications
BEGIN SELECT RAISE(ABORT, 'applications are immutable'); END;
CREATE TRIGGER application_artifacts_immutable_update BEFORE UPDATE ON application_artifacts
BEGIN SELECT RAISE(ABORT, 'application artifacts are immutable'); END;
CREATE TRIGGER application_artifacts_immutable_delete BEFORE DELETE ON application_artifacts
BEGIN SELECT RAISE(ABORT, 'application artifacts are immutable'); END;
CREATE TRIGGER relations_immutable_update BEFORE UPDATE ON experience_relations
BEGIN SELECT RAISE(ABORT, 'experience relations are immutable'); END;
CREATE TRIGGER relations_immutable_delete BEFORE DELETE ON experience_relations
BEGIN SELECT RAISE(ABORT, 'experience relations are immutable'); END;
CREATE TRIGGER mistakes_immutable_update BEFORE UPDATE ON repeated_mistakes
BEGIN SELECT RAISE(ABORT, 'mistake observations are immutable'); END;
CREATE TRIGGER mistakes_immutable_delete BEFORE DELETE ON repeated_mistakes
BEGIN SELECT RAISE(ABORT, 'mistake observations are immutable'); END;
CREATE TRIGGER validations_immutable_update BEFORE UPDATE ON lesson_validations
BEGIN SELECT RAISE(ABORT, 'validation decisions are immutable'); END;
CREATE TRIGGER validations_immutable_delete BEFORE DELETE ON lesson_validations
BEGIN SELECT RAISE(ABORT, 'validation decisions are immutable'); END;
