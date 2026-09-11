-- SPDX-License-Identifier: Apache-2.0
CREATE TABLE compositions(id TEXT PRIMARY KEY, revision INTEGER NOT NULL, name TEXT NOT NULL UNIQUE, data TEXT NOT NULL CHECK(json_valid(data)));
CREATE TABLE composition_revisions(id TEXT NOT NULL, revision INTEGER NOT NULL, hash TEXT NOT NULL, data TEXT NOT NULL CHECK(json_valid(data)), PRIMARY KEY(id,revision));
CREATE TABLE composition_components(key TEXT PRIMARY KEY, data TEXT NOT NULL CHECK(json_valid(data)));
CREATE TABLE composition_evidence(id TEXT PRIMARY KEY, composition TEXT NOT NULL REFERENCES compositions(id), revision INTEGER NOT NULL, data TEXT NOT NULL CHECK(json_valid(data)));
CREATE TABLE composition_handoffs(id TEXT PRIMARY KEY, composition TEXT NOT NULL REFERENCES compositions(id), data TEXT NOT NULL CHECK(json_valid(data)));
CREATE TABLE composition_interaction_failures(id TEXT PRIMARY KEY, composition TEXT NOT NULL REFERENCES compositions(id), data TEXT NOT NULL CHECK(json_valid(data)));
CREATE TABLE composite_skills(id TEXT PRIMARY KEY, composition TEXT NOT NULL REFERENCES compositions(id), revision INTEGER NOT NULL, data TEXT NOT NULL CHECK(json_valid(data)));
CREATE TABLE composition_events(id INTEGER PRIMARY KEY, composition TEXT NOT NULL, kind TEXT NOT NULL, data TEXT NOT NULL CHECK(json_valid(data)), created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP);
CREATE TRIGGER immutable_composition_revision_update BEFORE UPDATE ON composition_revisions BEGIN SELECT RAISE(ABORT,'immutable composition revision'); END;
CREATE TRIGGER immutable_composition_revision_delete BEFORE DELETE ON composition_revisions BEGIN SELECT RAISE(ABORT,'immutable composition revision'); END;
CREATE TRIGGER immutable_composition_evidence_update BEFORE UPDATE ON composition_evidence BEGIN SELECT RAISE(ABORT,'immutable composition evidence'); END;
CREATE TRIGGER immutable_composition_evidence_delete BEFORE DELETE ON composition_evidence BEGIN SELECT RAISE(ABORT,'immutable composition evidence'); END;
CREATE TRIGGER immutable_composition_component_update BEFORE UPDATE ON composition_components BEGIN SELECT RAISE(ABORT,'immutable composition component'); END;
CREATE TRIGGER immutable_composition_component_delete BEFORE DELETE ON composition_components BEGIN SELECT RAISE(ABORT,'immutable composition component'); END;
