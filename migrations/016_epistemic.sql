CREATE TABLE claims (
  id TEXT PRIMARY KEY,
  kind TEXT NOT NULL,
  canonical_hash TEXT NOT NULL,
  created_at TEXT NOT NULL,
  data TEXT NOT NULL CHECK(json_valid(data)),
  UNIQUE(canonical_hash)
);

CREATE TABLE evidence_paths (
  id TEXT PRIMARY KEY,
  claim_id TEXT NOT NULL REFERENCES claims(id),
  source_kind TEXT NOT NULL,
  outcome TEXT NOT NULL CHECK(outcome IN ('supports','contradicts','inconclusive')),
  context_fingerprint TEXT NOT NULL,
  created_at TEXT NOT NULL,
  data TEXT NOT NULL CHECK(json_valid(data))
);
CREATE INDEX evidence_paths_claim ON evidence_paths(claim_id, created_at, id);
CREATE INDEX evidence_paths_context ON evidence_paths(context_fingerprint, claim_id);

CREATE TABLE epistemic_dependencies (
  evidence_path_id TEXT NOT NULL REFERENCES evidence_paths(id),
  kind TEXT NOT NULL,
  value TEXT NOT NULL,
  PRIMARY KEY(evidence_path_id, kind, value)
);
CREATE INDEX epistemic_dependency_lookup ON epistemic_dependencies(kind, value, evidence_path_id);

-- Derived graph edges are materialized only as an inspection cache. Raw paths
-- and their dependency rows remain canonical and the graph can be rebuilt.
CREATE TABLE epistemic_dependency_edges (
  claim_id TEXT NOT NULL REFERENCES claims(id),
  from_node TEXT NOT NULL,
  to_node TEXT NOT NULL,
  kind TEXT NOT NULL,
  PRIMARY KEY(claim_id, from_node, to_node, kind)
);

CREATE TABLE evidence_sessions (
  id TEXT PRIMARY KEY,
  claim_id TEXT NOT NULL REFERENCES claims(id),
  created_at TEXT NOT NULL,
  data TEXT NOT NULL CHECK(json_valid(data))
);
CREATE INDEX evidence_session_claim ON evidence_sessions(claim_id, created_at, id);

CREATE TABLE fused_evidence_assessments (
  sequence INTEGER PRIMARY KEY AUTOINCREMENT,
  claim_id TEXT NOT NULL REFERENCES claims(id),
  created_at TEXT NOT NULL,
  data TEXT NOT NULL CHECK(json_valid(data))
);
CREATE INDEX fused_assessment_claim ON fused_evidence_assessments(claim_id, sequence);

CREATE TABLE experience_influence (
  sequence INTEGER PRIMARY KEY AUTOINCREMENT,
  lesson_id TEXT NOT NULL REFERENCES lessons(id),
  session_id TEXT NOT NULL,
  agent_key TEXT NOT NULL,
  repository TEXT NOT NULL,
  decision_id TEXT,
  outcome TEXT NOT NULL CHECK(outcome IN ('successful','failed','inconclusive')),
  observed_at TEXT NOT NULL,
  data TEXT NOT NULL CHECK(json_valid(data))
);
CREATE INDEX experience_influence_lesson ON experience_influence(lesson_id, observed_at, sequence);

CREATE TABLE experience_quarantines (
  sequence INTEGER PRIMARY KEY AUTOINCREMENT,
  lesson_id TEXT NOT NULL REFERENCES lessons(id),
  state TEXT NOT NULL CHECK(state IN ('active','advisory','quarantined','disabled')),
  reason TEXT NOT NULL,
  created_at TEXT NOT NULL,
  data TEXT NOT NULL CHECK(json_valid(data))
);
CREATE INDEX experience_quarantine_lesson ON experience_quarantines(lesson_id, sequence);

CREATE TRIGGER claims_immutable_update BEFORE UPDATE ON claims
BEGIN SELECT RAISE(ABORT, 'Claims are immutable'); END;
CREATE TRIGGER claims_immutable_delete BEFORE DELETE ON claims
BEGIN SELECT RAISE(ABORT, 'Claims are retained'); END;
CREATE TRIGGER evidence_paths_immutable_update BEFORE UPDATE ON evidence_paths
BEGIN SELECT RAISE(ABORT, 'Evidence paths are immutable'); END;
CREATE TRIGGER evidence_paths_immutable_delete BEFORE DELETE ON evidence_paths
BEGIN SELECT RAISE(ABORT, 'Evidence paths are retained'); END;
CREATE TRIGGER epistemic_dependencies_immutable_update BEFORE UPDATE ON epistemic_dependencies
BEGIN SELECT RAISE(ABORT, 'Epistemic dependencies are immutable'); END;
CREATE TRIGGER epistemic_dependencies_immutable_delete BEFORE DELETE ON epistemic_dependencies
BEGIN SELECT RAISE(ABORT, 'Epistemic dependencies are retained'); END;
CREATE TRIGGER evidence_sessions_immutable_update BEFORE UPDATE ON evidence_sessions
BEGIN SELECT RAISE(ABORT, 'Evidence sessions are immutable'); END;
CREATE TRIGGER evidence_sessions_immutable_delete BEFORE DELETE ON evidence_sessions
BEGIN SELECT RAISE(ABORT, 'Evidence sessions are retained'); END;
CREATE TRIGGER fused_assessments_immutable_update BEFORE UPDATE ON fused_evidence_assessments
BEGIN SELECT RAISE(ABORT, 'Fused assessments are immutable'); END;
CREATE TRIGGER fused_assessments_immutable_delete BEFORE DELETE ON fused_evidence_assessments
BEGIN SELECT RAISE(ABORT, 'Fused assessments are retained'); END;
CREATE TRIGGER experience_influence_immutable_update BEFORE UPDATE ON experience_influence
BEGIN SELECT RAISE(ABORT, 'Experience influence is immutable'); END;
CREATE TRIGGER experience_influence_immutable_delete BEFORE DELETE ON experience_influence
BEGIN SELECT RAISE(ABORT, 'Experience influence is retained'); END;
CREATE TRIGGER experience_quarantines_immutable_update BEFORE UPDATE ON experience_quarantines
BEGIN SELECT RAISE(ABORT, 'Experience quarantine history is immutable'); END;
CREATE TRIGGER experience_quarantines_immutable_delete BEFORE DELETE ON experience_quarantines
BEGIN SELECT RAISE(ABORT, 'Experience quarantine history is retained'); END;

