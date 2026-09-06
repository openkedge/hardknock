-- SPDX-License-Identifier: Apache-2.0
-- Predictive Experience: normalized trajectories, earned warnings, and prevention feedback.
CREATE TABLE execution_trajectories (
    id TEXT PRIMARY KEY CHECK(id LIKE 'trajectory-%'),
    session_id TEXT NOT NULL CHECK(session_id LIKE 'session-%'),
    task_family_id TEXT,
    outcome_kind TEXT,
    started_at TEXT NOT NULL,
    ended_at TEXT,
    fingerprint TEXT,
    data TEXT NOT NULL
);
CREATE INDEX idx_trajectory_family ON execution_trajectories(task_family_id, started_at);
CREATE INDEX idx_trajectory_outcome ON execution_trajectories(outcome_kind, ended_at);
CREATE INDEX idx_trajectory_fingerprint ON execution_trajectories(fingerprint);

CREATE TABLE trajectory_events (
    id TEXT PRIMARY KEY CHECK(id LIKE 'trajectory-event-%'),
    trajectory_id TEXT NOT NULL REFERENCES execution_trajectories(id),
    sequence INTEGER NOT NULL CHECK(sequence >= 0),
    event_kind TEXT NOT NULL,
    created_at TEXT NOT NULL,
    data TEXT NOT NULL,
    UNIQUE(trajectory_id, sequence)
);
CREATE INDEX idx_trajectory_event_lookup ON trajectory_events(trajectory_id, sequence);

CREATE TABLE risk_indicators (
    id TEXT PRIMARY KEY CHECK(id LIKE 'risk-indicator-%'),
    status TEXT NOT NULL,
    failure_class TEXT NOT NULL,
    data TEXT NOT NULL
);
CREATE INDEX idx_risk_indicator_failure ON risk_indicators(failure_class, status);

CREATE TABLE early_warning_signatures (
    id TEXT PRIMARY KEY CHECK(id LIKE 'early-warning-%'),
    status TEXT NOT NULL,
    failure_class TEXT NOT NULL,
    revision INTEGER NOT NULL CHECK(revision > 0),
    runtime_version TEXT,
    data TEXT NOT NULL
);
CREATE INDEX idx_warning_failure ON early_warning_signatures(failure_class, status);
CREATE INDEX idx_warning_runtime ON early_warning_signatures(runtime_version, status);

CREATE TABLE early_warning_revisions (
    id TEXT PRIMARY KEY CHECK(id LIKE 'forecast-revision-%'),
    signature_id TEXT NOT NULL REFERENCES early_warning_signatures(id),
    revision INTEGER NOT NULL CHECK(revision > 0),
    created_at TEXT NOT NULL,
    data TEXT NOT NULL,
    UNIQUE(signature_id, revision)
);
CREATE INDEX idx_warning_revision ON early_warning_revisions(signature_id, revision);

CREATE TABLE failure_forecasts (
    id TEXT PRIMARY KEY CHECK(id LIKE 'forecast-%'),
    trajectory_id TEXT NOT NULL REFERENCES execution_trajectories(id),
    signature_id TEXT NOT NULL REFERENCES early_warning_signatures(id),
    failure_class TEXT NOT NULL,
    status TEXT NOT NULL,
    created_at TEXT NOT NULL,
    data TEXT NOT NULL,
    UNIQUE(trajectory_id, signature_id, status)
);
CREATE INDEX idx_forecast_status ON failure_forecasts(status, created_at);
CREATE INDEX idx_forecast_trajectory ON failure_forecasts(trajectory_id, created_at);

CREATE TABLE preventive_interventions (
    id TEXT PRIMARY KEY CHECK(id LIKE 'preventive-%'),
    signature_id TEXT NOT NULL REFERENCES early_warning_signatures(id),
    status TEXT NOT NULL,
    failure_class TEXT NOT NULL,
    data TEXT NOT NULL
);
CREATE INDEX idx_preventive_lookup ON preventive_interventions(signature_id, status);

CREATE TABLE preventive_counterfactuals (
    id TEXT PRIMARY KEY CHECK(id LIKE 'preventive-pair-%'),
    forecast_id TEXT NOT NULL REFERENCES failure_forecasts(id),
    intervention_id TEXT NOT NULL REFERENCES preventive_interventions(id),
    experiment_id TEXT NOT NULL REFERENCES experiment_requests(id),
    created_at TEXT NOT NULL,
    data TEXT NOT NULL
);
CREATE INDEX idx_preventive_evidence ON preventive_counterfactuals(intervention_id, created_at);

CREATE TABLE forecast_feedback (
    id TEXT PRIMARY KEY CHECK(id LIKE 'forecast-feedback-%'),
    forecast_id TEXT NOT NULL UNIQUE REFERENCES failure_forecasts(id),
    status TEXT NOT NULL,
    created_at TEXT NOT NULL,
    data TEXT NOT NULL
);
CREATE INDEX idx_forecast_feedback_status ON forecast_feedback(status, created_at);

CREATE TABLE forecast_misses (
    trajectory_id TEXT PRIMARY KEY REFERENCES execution_trajectories(id),
    failure_class TEXT NOT NULL,
    forecastability TEXT NOT NULL,
    created_at TEXT NOT NULL,
    data TEXT NOT NULL
);
CREATE INDEX idx_forecast_miss_failure ON forecast_misses(failure_class, forecastability);

CREATE TABLE forecast_quality_snapshots (
    id TEXT PRIMARY KEY CHECK(id LIKE 'forecast-quality-%'),
    created_at TEXT NOT NULL,
    data TEXT NOT NULL
);

CREATE TABLE predictive_events (
    sequence INTEGER PRIMARY KEY AUTOINCREMENT,
    subject TEXT NOT NULL,
    kind TEXT NOT NULL,
    created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
    data TEXT NOT NULL
);
CREATE INDEX idx_predictive_events_subject ON predictive_events(subject, sequence);

CREATE TRIGGER trajectory_event_identity_immutable
BEFORE UPDATE ON trajectory_events BEGIN SELECT RAISE(ABORT, 'trajectory events are append-only'); END;
CREATE TRIGGER trajectory_event_no_delete
BEFORE DELETE ON trajectory_events BEGIN SELECT RAISE(ABORT, 'trajectory events are append-only'); END;
CREATE TRIGGER warning_revision_identity_immutable
BEFORE UPDATE ON early_warning_revisions BEGIN SELECT RAISE(ABORT, 'warning revisions are append-only'); END;
CREATE TRIGGER warning_revision_no_delete
BEFORE DELETE ON early_warning_revisions BEGIN SELECT RAISE(ABORT, 'warning revisions are append-only'); END;
CREATE TRIGGER preventive_counterfactual_immutable
BEFORE UPDATE ON preventive_counterfactuals BEGIN SELECT RAISE(ABORT, 'preventive evidence is append-only'); END;
CREATE TRIGGER preventive_counterfactual_no_delete
BEFORE DELETE ON preventive_counterfactuals BEGIN SELECT RAISE(ABORT, 'preventive evidence is append-only'); END;
CREATE TRIGGER forecast_feedback_immutable
BEFORE UPDATE ON forecast_feedback BEGIN SELECT RAISE(ABORT, 'forecast feedback is append-only'); END;
CREATE TRIGGER forecast_feedback_no_delete
BEFORE DELETE ON forecast_feedback BEGIN SELECT RAISE(ABORT, 'forecast feedback is append-only'); END;
