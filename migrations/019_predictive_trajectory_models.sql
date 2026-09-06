-- SPDX-License-Identifier: Apache-2.0
-- Rich predictive trajectory projections and versioned matching policy.
ALTER TABLE execution_trajectories ADD COLUMN subject_kind TEXT;
ALTER TABLE execution_trajectories ADD COLUMN subject_id TEXT;
CREATE INDEX idx_trajectory_subject ON execution_trajectories(subject_kind, subject_id, started_at);

CREATE TABLE trajectory_points (
    trajectory_id TEXT NOT NULL REFERENCES execution_trajectories(id),
    point_index INTEGER NOT NULL CHECK(point_index >= 0),
    event_id TEXT NOT NULL UNIQUE REFERENCES trajectory_events(id),
    observed_at TEXT NOT NULL,
    data TEXT NOT NULL,
    PRIMARY KEY(trajectory_id, point_index)
);
CREATE INDEX idx_trajectory_point_event ON trajectory_points(event_id);

CREATE TABLE risk_signals (
    id TEXT PRIMARY KEY CHECK(id LIKE 'risk-signal-%'),
    trajectory_id TEXT NOT NULL REFERENCES execution_trajectories(id),
    point_index INTEGER NOT NULL CHECK(point_index >= 0),
    kind TEXT NOT NULL,
    observed_at TEXT NOT NULL,
    data TEXT NOT NULL
);
CREATE INDEX idx_risk_signal_trajectory ON risk_signals(trajectory_id, point_index);
CREATE INDEX idx_risk_signal_kind ON risk_signals(kind, observed_at);

CREATE TABLE failure_trajectories (
    id TEXT PRIMARY KEY CHECK(id LIKE 'failure-trajectory-%'),
    failure_class TEXT NOT NULL,
    status TEXT NOT NULL,
    runtime_version TEXT,
    revision INTEGER NOT NULL CHECK(revision > 0),
    data TEXT NOT NULL
);
CREATE INDEX idx_failure_trajectory_lookup ON failure_trajectories(failure_class, status, runtime_version);

CREATE TABLE failure_trajectory_steps (
    failure_trajectory_id TEXT NOT NULL REFERENCES failure_trajectories(id),
    revision INTEGER NOT NULL CHECK(revision > 0),
    step_index INTEGER NOT NULL CHECK(step_index >= 0),
    data TEXT NOT NULL,
    PRIMARY KEY(failure_trajectory_id, revision, step_index)
);
CREATE INDEX idx_failure_step_lookup ON failure_trajectory_steps(failure_trajectory_id, revision, step_index);

CREATE TABLE failure_trajectory_families (
    id TEXT PRIMARY KEY CHECK(id LIKE 'failure-family-%'),
    failure_class TEXT NOT NULL,
    data TEXT NOT NULL
);
CREATE INDEX idx_failure_family_lookup ON failure_trajectory_families(failure_class);

CREATE TABLE forecast_signal_refs (
    forecast_id TEXT NOT NULL REFERENCES failure_forecasts(id),
    signal_id TEXT NOT NULL REFERENCES risk_signals(id),
    PRIMARY KEY(forecast_id, signal_id)
);

CREATE TABLE intervention_windows (
    intervention_id TEXT PRIMARY KEY REFERENCES preventive_interventions(id),
    trajectory_id TEXT NOT NULL REFERENCES execution_trajectories(id),
    opens_at INTEGER NOT NULL CHECK(opens_at >= 0),
    closes_at INTEGER,
    data TEXT NOT NULL
);

CREATE TABLE forecast_policy_versions (
    version TEXT PRIMARY KEY,
    created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
    data TEXT NOT NULL
);
