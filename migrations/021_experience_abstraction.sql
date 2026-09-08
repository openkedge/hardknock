-- SPDX-License-Identifier: Apache-2.0
-- V0.17 evidence-backed abstraction, held-out transfer, and reversible distillation.
CREATE TABLE experience_patterns (
    id TEXT PRIMARY KEY CHECK(id LIKE 'pattern-%'),
    kind TEXT NOT NULL,
    status TEXT NOT NULL,
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL,
    data TEXT NOT NULL CHECK(json_valid(data))
);
CREATE INDEX idx_experience_patterns_status ON experience_patterns(status, kind, updated_at);

CREATE TABLE experience_pattern_members (
    pattern_id TEXT NOT NULL REFERENCES experience_patterns(id),
    position INTEGER NOT NULL CHECK(position >= 0),
    artifact_kind TEXT NOT NULL,
    artifact_id TEXT NOT NULL,
    artifact_revision INTEGER NOT NULL CHECK(artifact_revision > 0),
    PRIMARY KEY(pattern_id, position),
    UNIQUE(pattern_id, artifact_kind, artifact_id, artifact_revision)
);

CREATE TABLE abstract_knowledge (
    id TEXT PRIMARY KEY CHECK(id LIKE 'abstract-%'),
    revision INTEGER NOT NULL CHECK(revision > 0),
    kind TEXT NOT NULL,
    maturity TEXT NOT NULL,
    origin TEXT NOT NULL,
    updated_at TEXT NOT NULL,
    data TEXT NOT NULL CHECK(json_valid(data))
);

CREATE TABLE abstract_knowledge_revisions (
    abstract_id TEXT NOT NULL REFERENCES abstract_knowledge(id),
    revision INTEGER NOT NULL CHECK(revision > 0),
    reason TEXT NOT NULL,
    created_at TEXT NOT NULL,
    data TEXT NOT NULL CHECK(json_valid(data)),
    PRIMARY KEY(abstract_id, revision)
);

CREATE TABLE transfer_hypotheses (
    id TEXT PRIMARY KEY CHECK(id LIKE 'transfer-hypothesis-%'),
    abstract_id TEXT NOT NULL REFERENCES abstract_knowledge(id),
    status TEXT NOT NULL,
    created_at TEXT NOT NULL,
    data TEXT NOT NULL CHECK(json_valid(data))
);
CREATE INDEX idx_transfer_hypotheses_abstract ON transfer_hypotheses(abstract_id, status);

CREATE TABLE transfer_evaluation_sets (
    hypothesis_id TEXT PRIMARY KEY REFERENCES transfer_hypotheses(id),
    data TEXT NOT NULL CHECK(json_valid(data))
);

CREATE TABLE transfer_evidence (
    id TEXT PRIMARY KEY CHECK(id LIKE 'transfer-evidence-%'),
    hypothesis_id TEXT NOT NULL REFERENCES transfer_hypotheses(id),
    abstract_id TEXT NOT NULL REFERENCES abstract_knowledge(id),
    context_role TEXT NOT NULL,
    outcome TEXT NOT NULL,
    local INTEGER NOT NULL CHECK(local IN (0,1)),
    created_at TEXT NOT NULL,
    data TEXT NOT NULL CHECK(json_valid(data))
);
CREATE INDEX idx_transfer_evidence_abstract ON transfer_evidence(abstract_id, context_role, outcome);

CREATE TABLE generalization_boundaries (
    abstract_id TEXT NOT NULL REFERENCES abstract_knowledge(id),
    revision INTEGER NOT NULL CHECK(revision > 0),
    data TEXT NOT NULL CHECK(json_valid(data)),
    PRIMARY KEY(abstract_id, revision)
);

CREATE TABLE knowledge_specializations (
    parent_id TEXT NOT NULL REFERENCES abstract_knowledge(id),
    parent_revision INTEGER NOT NULL CHECK(parent_revision > 0),
    child_id TEXT NOT NULL REFERENCES abstract_knowledge(id),
    child_revision INTEGER NOT NULL CHECK(child_revision > 0),
    created_at TEXT NOT NULL,
    data TEXT NOT NULL CHECK(json_valid(data)),
    PRIMARY KEY(parent_id, parent_revision, child_id, child_revision)
);

CREATE TABLE knowledge_exceptions (
    id TEXT PRIMARY KEY CHECK(id LIKE 'knowledge-exception-%'),
    parent_id TEXT NOT NULL REFERENCES abstract_knowledge(id),
    parent_revision INTEGER NOT NULL CHECK(parent_revision > 0),
    created_at TEXT NOT NULL,
    data TEXT NOT NULL CHECK(json_valid(data))
);

CREATE TABLE knowledge_distillations (
    id TEXT PRIMARY KEY CHECK(id LIKE 'distillation-%'),
    created_at TEXT NOT NULL,
    data TEXT NOT NULL CHECK(json_valid(data))
);

CREATE TABLE knowledge_representation_state (
    artifact_kind TEXT NOT NULL,
    artifact_id TEXT NOT NULL,
    artifact_revision INTEGER NOT NULL CHECK(artifact_revision > 0),
    changed_at TEXT NOT NULL,
    data TEXT NOT NULL CHECK(json_valid(data)),
    PRIMARY KEY(artifact_kind, artifact_id, artifact_revision)
);

CREATE TABLE negative_transfer_events (
    sequence INTEGER PRIMARY KEY AUTOINCREMENT,
    abstract_id TEXT NOT NULL REFERENCES abstract_knowledge(id),
    outcome TEXT NOT NULL,
    observed_at TEXT NOT NULL,
    data TEXT NOT NULL CHECK(json_valid(data))
);

CREATE TABLE analogy_mappings (
    id TEXT PRIMARY KEY CHECK(id LIKE 'analogy-%'),
    created_at TEXT NOT NULL,
    data TEXT NOT NULL CHECK(json_valid(data))
);

CREATE TABLE abstraction_events (
    sequence INTEGER PRIMARY KEY AUTOINCREMENT,
    subject TEXT NOT NULL,
    kind TEXT NOT NULL,
    created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
    data TEXT NOT NULL CHECK(json_valid(data))
);
CREATE INDEX idx_abstraction_events_subject ON abstraction_events(subject, sequence);

CREATE TRIGGER abstract_knowledge_revisions_no_update
BEFORE UPDATE ON abstract_knowledge_revisions BEGIN SELECT RAISE(ABORT, 'abstract knowledge revisions are append-only'); END;
CREATE TRIGGER abstract_knowledge_revisions_no_delete
BEFORE DELETE ON abstract_knowledge_revisions BEGIN SELECT RAISE(ABORT, 'abstract knowledge revisions are append-only'); END;
CREATE TRIGGER transfer_evidence_no_update
BEFORE UPDATE ON transfer_evidence BEGIN SELECT RAISE(ABORT, 'transfer evidence is append-only'); END;
CREATE TRIGGER transfer_evidence_no_delete
BEFORE DELETE ON transfer_evidence BEGIN SELECT RAISE(ABORT, 'transfer evidence is append-only'); END;
CREATE TRIGGER negative_transfer_events_no_update
BEFORE UPDATE ON negative_transfer_events BEGIN SELECT RAISE(ABORT, 'negative transfer events are append-only'); END;
CREATE TRIGGER negative_transfer_events_no_delete
BEFORE DELETE ON negative_transfer_events BEGIN SELECT RAISE(ABORT, 'negative transfer events are append-only'); END;
