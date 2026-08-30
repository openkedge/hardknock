-- Extend immutable Experience relations for authoritative effect evidence.
DROP TRIGGER experience_relations_immutable_update;
DROP TRIGGER experience_relations_immutable_delete;
ALTER TABLE experience_relations RENAME TO old_effect_experience_relations;
CREATE TABLE experience_relations (
    source_experience_id TEXT NOT NULL REFERENCES experiences(id),
    target_experience_id TEXT NOT NULL REFERENCES experiences(id),
    relation_type TEXT NOT NULL CHECK(relation_type IN (
        'retry_of','counterfactual_of','transfer_from','chaos_variant_of','recovery_of',
        'commit_of','compensation_of','reconciliation_of'
    )),
    PRIMARY KEY(source_experience_id,target_experience_id,relation_type),
    CHECK(source_experience_id != target_experience_id)
);
INSERT INTO experience_relations SELECT * FROM old_effect_experience_relations;
DROP TABLE old_effect_experience_relations;
CREATE TRIGGER experience_relations_immutable_update BEFORE UPDATE ON experience_relations
BEGIN SELECT RAISE(ABORT, 'experience relations are immutable'); END;
CREATE TRIGGER experience_relations_immutable_delete BEFORE DELETE ON experience_relations
BEGIN SELECT RAISE(ABORT, 'experience relations are immutable'); END;

CREATE TABLE effect_ledgers (
    id TEXT PRIMARY KEY NOT NULL,
    reality_id TEXT UNIQUE REFERENCES realities(id),
    created_at TEXT NOT NULL,
    data TEXT NOT NULL CHECK(json_valid(data))
);

CREATE TABLE effects (
    id TEXT PRIMARY KEY NOT NULL,
    ledger_id TEXT NOT NULL REFERENCES effect_ledgers(id),
    reality_id TEXT REFERENCES realities(id),
    session_id TEXT NOT NULL,
    adapter TEXT NOT NULL,
    lifecycle TEXT NOT NULL,
    scope_hash TEXT NOT NULL,
    created_at TEXT NOT NULL,
    data TEXT NOT NULL CHECK(json_valid(data))
);
CREATE INDEX effects_lifecycle ON effects(lifecycle, created_at, id);
CREATE INDEX effects_reality ON effects(reality_id, created_at, id);

CREATE TABLE effect_events (
    id TEXT PRIMARY KEY NOT NULL,
    effect_id TEXT NOT NULL REFERENCES effects(id),
    sequence INTEGER NOT NULL CHECK(sequence > 0),
    event_type TEXT NOT NULL,
    timestamp TEXT NOT NULL,
    data TEXT NOT NULL CHECK(json_valid(data)),
    UNIQUE(effect_id, sequence)
);
CREATE INDEX effect_events_effect ON effect_events(effect_id, sequence);

CREATE TABLE prepared_effects (
    id TEXT PRIMARY KEY NOT NULL,
    effect_id TEXT NOT NULL UNIQUE REFERENCES effects(id),
    expires_at TEXT,
    data TEXT NOT NULL CHECK(json_valid(data))
);

CREATE TABLE external_state_snapshots (
    id TEXT PRIMARY KEY NOT NULL,
    effect_id TEXT NOT NULL REFERENCES effects(id),
    phase TEXT NOT NULL,
    captured_at TEXT NOT NULL,
    data TEXT NOT NULL CHECK(json_valid(data))
);
CREATE INDEX external_state_snapshots_effect ON external_state_snapshots(effect_id, captured_at, id);

CREATE TABLE commit_authorizations (
    id TEXT PRIMARY KEY NOT NULL,
    scope_hash TEXT NOT NULL,
    granted_at TEXT NOT NULL,
    expires_at TEXT,
    data TEXT NOT NULL CHECK(json_valid(data))
);

CREATE TABLE commit_receipts (
    id TEXT PRIMARY KEY NOT NULL,
    effect_id TEXT NOT NULL UNIQUE REFERENCES effects(id),
    committed_at TEXT NOT NULL,
    data TEXT NOT NULL CHECK(json_valid(data))
);

CREATE TABLE compensation_receipts (
    id TEXT PRIMARY KEY NOT NULL,
    original_receipt TEXT NOT NULL REFERENCES commit_receipts(id),
    compensated_at TEXT NOT NULL,
    data TEXT NOT NULL CHECK(json_valid(data))
);

CREATE TABLE effect_plans (
    id TEXT PRIMARY KEY NOT NULL,
    created_at TEXT NOT NULL,
    data TEXT NOT NULL CHECK(json_valid(data))
);

CREATE TABLE effect_groups (
    id TEXT PRIMARY KEY NOT NULL,
    plan_id TEXT NOT NULL REFERENCES effect_plans(id),
    created_at TEXT NOT NULL,
    outcome TEXT NOT NULL,
    data TEXT NOT NULL CHECK(json_valid(data))
);

CREATE TABLE reconciliation_attempts (
    id TEXT PRIMARY KEY NOT NULL,
    effect_id TEXT NOT NULL REFERENCES effects(id),
    attempted_at TEXT NOT NULL,
    result TEXT NOT NULL,
    data TEXT NOT NULL CHECK(json_valid(data))
);

CREATE TABLE effect_experience_links (
    effect_id TEXT NOT NULL REFERENCES effects(id),
    experience_id TEXT NOT NULL REFERENCES experiences(id),
    relation TEXT NOT NULL,
    created_at TEXT NOT NULL,
    PRIMARY KEY(effect_id, experience_id, relation)
);

CREATE TABLE effect_benchmark_runs (
    id TEXT PRIMARY KEY NOT NULL,
    created_at TEXT NOT NULL,
    data TEXT NOT NULL CHECK(json_valid(data))
);

CREATE TRIGGER effect_events_immutable_update BEFORE UPDATE ON effect_events BEGIN
    SELECT RAISE(ABORT, 'effect events are immutable');
END;
CREATE TRIGGER effect_events_immutable_delete BEFORE DELETE ON effect_events BEGIN
    SELECT RAISE(ABORT, 'effect events are immutable');
END;
CREATE TRIGGER commit_receipts_immutable_update BEFORE UPDATE ON commit_receipts BEGIN
    SELECT RAISE(ABORT, 'commit receipts are immutable');
END;
CREATE TRIGGER commit_receipts_immutable_delete BEFORE DELETE ON commit_receipts BEGIN
    SELECT RAISE(ABORT, 'commit receipts are immutable');
END;
CREATE TRIGGER compensation_receipts_immutable_update BEFORE UPDATE ON compensation_receipts BEGIN
    SELECT RAISE(ABORT, 'compensation receipts are immutable');
END;
CREATE TRIGGER compensation_receipts_immutable_delete BEFORE DELETE ON compensation_receipts BEGIN
    SELECT RAISE(ABORT, 'compensation receipts are immutable');
END;
CREATE TRIGGER external_state_snapshots_immutable_update BEFORE UPDATE ON external_state_snapshots BEGIN
    SELECT RAISE(ABORT, 'external state snapshots are immutable');
END;
CREATE TRIGGER external_state_snapshots_immutable_delete BEFORE DELETE ON external_state_snapshots BEGIN
    SELECT RAISE(ABORT, 'external state snapshots are immutable');
END;
CREATE TRIGGER effect_experience_links_immutable_update BEFORE UPDATE ON effect_experience_links BEGIN
    SELECT RAISE(ABORT, 'effect experience links are immutable');
END;
CREATE TRIGGER effect_experience_links_immutable_delete BEFORE DELETE ON effect_experience_links BEGIN
    SELECT RAISE(ABORT, 'effect experience links are immutable');
END;
