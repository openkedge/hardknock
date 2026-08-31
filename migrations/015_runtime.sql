CREATE TABLE runtime_policy_versions (
  version TEXT PRIMARY KEY,
  created_at TEXT NOT NULL,
  data TEXT NOT NULL CHECK(json_valid(data))
);

CREATE TABLE runtime_decisions (
  id TEXT PRIMARY KEY,
  session_id TEXT NOT NULL,
  context_hash TEXT NOT NULL,
  decision_kind TEXT NOT NULL,
  knowledge_state TEXT NOT NULL,
  policy_version TEXT NOT NULL REFERENCES runtime_policy_versions(version),
  created_at TEXT NOT NULL,
  data TEXT NOT NULL CHECK(json_valid(data))
);
CREATE INDEX runtime_decision_session
  ON runtime_decisions(session_id, created_at, id);
CREATE INDEX runtime_decision_context
  ON runtime_decisions(context_hash, created_at, id);
CREATE INDEX runtime_decision_kind
  ON runtime_decisions(decision_kind, created_at, id);

CREATE TABLE runtime_decision_reasons (
  decision_id TEXT NOT NULL REFERENCES runtime_decisions(id),
  position INTEGER NOT NULL CHECK(position >= 0),
  reason_kind TEXT NOT NULL,
  data TEXT NOT NULL CHECK(json_valid(data)),
  PRIMARY KEY(decision_id, position)
);

CREATE TABLE runtime_decision_evidence (
  decision_id TEXT NOT NULL REFERENCES runtime_decisions(id),
  position INTEGER NOT NULL CHECK(position >= 0),
  evidence_kind TEXT NOT NULL,
  evidence_id TEXT,
  data TEXT NOT NULL CHECK(json_valid(data)),
  PRIMARY KEY(decision_id, position)
);

CREATE TABLE runtime_decision_feedback (
  decision_id TEXT NOT NULL REFERENCES runtime_decisions(id),
  observed_at TEXT NOT NULL,
  outcome TEXT NOT NULL,
  data TEXT NOT NULL CHECK(json_valid(data)),
  PRIMARY KEY(decision_id, observed_at)
);
CREATE INDEX runtime_feedback_outcome
  ON runtime_decision_feedback(outcome, observed_at, decision_id);

CREATE TABLE runtime_abstentions (
  decision_id TEXT PRIMARY KEY REFERENCES runtime_decisions(id),
  reason TEXT NOT NULL,
  data TEXT NOT NULL CHECK(json_valid(data))
);

CREATE TABLE runtime_control_events (
  sequence INTEGER PRIMARY KEY AUTOINCREMENT,
  decision_id TEXT REFERENCES runtime_decisions(id),
  session_id TEXT NOT NULL,
  kind TEXT NOT NULL,
  created_at TEXT NOT NULL,
  data TEXT NOT NULL CHECK(json_valid(data))
);
CREATE INDEX runtime_event_session
  ON runtime_control_events(session_id, sequence);

CREATE TRIGGER runtime_policy_version_no_update BEFORE UPDATE ON runtime_policy_versions
BEGIN SELECT RAISE(ABORT, 'Runtime policy versions are immutable'); END;
CREATE TRIGGER runtime_policy_version_no_delete BEFORE DELETE ON runtime_policy_versions
BEGIN SELECT RAISE(ABORT, 'Runtime policy versions are retained'); END;
CREATE TRIGGER runtime_decision_no_update BEFORE UPDATE ON runtime_decisions
BEGIN SELECT RAISE(ABORT, 'Runtime decisions are immutable'); END;
CREATE TRIGGER runtime_decision_no_delete BEFORE DELETE ON runtime_decisions
BEGIN SELECT RAISE(ABORT, 'Runtime decision history is retained'); END;
CREATE TRIGGER runtime_reason_no_update BEFORE UPDATE ON runtime_decision_reasons
BEGIN SELECT RAISE(ABORT, 'Runtime decision reasons are immutable'); END;
CREATE TRIGGER runtime_reason_no_delete BEFORE DELETE ON runtime_decision_reasons
BEGIN SELECT RAISE(ABORT, 'Runtime decision reasons are retained'); END;
CREATE TRIGGER runtime_evidence_no_update BEFORE UPDATE ON runtime_decision_evidence
BEGIN SELECT RAISE(ABORT, 'Runtime decision evidence is immutable'); END;
CREATE TRIGGER runtime_evidence_no_delete BEFORE DELETE ON runtime_decision_evidence
BEGIN SELECT RAISE(ABORT, 'Runtime decision evidence is retained'); END;
CREATE TRIGGER runtime_feedback_no_update BEFORE UPDATE ON runtime_decision_feedback
BEGIN SELECT RAISE(ABORT, 'Runtime feedback is immutable'); END;
CREATE TRIGGER runtime_feedback_no_delete BEFORE DELETE ON runtime_decision_feedback
BEGIN SELECT RAISE(ABORT, 'Runtime feedback history is retained'); END;
CREATE TRIGGER runtime_abstention_no_update BEFORE UPDATE ON runtime_abstentions
BEGIN SELECT RAISE(ABORT, 'Runtime abstentions are immutable'); END;
CREATE TRIGGER runtime_abstention_no_delete BEFORE DELETE ON runtime_abstentions
BEGIN SELECT RAISE(ABORT, 'Runtime abstentions are retained'); END;
CREATE TRIGGER runtime_event_no_update BEFORE UPDATE ON runtime_control_events
BEGIN SELECT RAISE(ABORT, 'Runtime control events are immutable'); END;
CREATE TRIGGER runtime_event_no_delete BEFORE DELETE ON runtime_control_events
BEGIN SELECT RAISE(ABORT, 'Runtime control events are retained'); END;
