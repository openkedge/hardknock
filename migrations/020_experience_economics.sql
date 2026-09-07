-- SPDX-License-Identifier: Apache-2.0
-- V0.16 deterministic experience opportunities, bounded portfolios, and actual yield.
CREATE TABLE experience_opportunities (
    id TEXT PRIMARY KEY CHECK(id LIKE 'opportunity-%'),
    kind TEXT NOT NULL,
    target_kind TEXT NOT NULL,
    target_id TEXT NOT NULL,
    status TEXT NOT NULL,
    created_at TEXT NOT NULL,
    data TEXT NOT NULL CHECK(json_valid(data))
);
CREATE INDEX idx_experience_opportunity_status ON experience_opportunities(status, kind, created_at);
CREATE INDEX idx_experience_opportunity_target ON experience_opportunities(target_kind, target_id);

CREATE TABLE experience_opportunity_reasons (
    opportunity_id TEXT NOT NULL REFERENCES experience_opportunities(id),
    position INTEGER NOT NULL CHECK(position >= 0),
    reason TEXT NOT NULL,
    PRIMARY KEY(opportunity_id, position)
);

CREATE TABLE experience_portfolios (
    id TEXT PRIMARY KEY CHECK(id LIKE 'portfolio-%'),
    revision INTEGER NOT NULL CHECK(revision > 0),
    objective TEXT NOT NULL,
    created_at TEXT NOT NULL,
    data TEXT NOT NULL CHECK(json_valid(data))
);

CREATE TABLE experience_portfolio_revisions (
    portfolio_id TEXT NOT NULL REFERENCES experience_portfolios(id),
    revision INTEGER NOT NULL CHECK(revision > 0),
    reason TEXT,
    created_at TEXT NOT NULL,
    data TEXT NOT NULL CHECK(json_valid(data)),
    PRIMARY KEY(portfolio_id, revision)
);

CREATE TABLE portfolio_selections (
    portfolio_id TEXT NOT NULL REFERENCES experience_portfolios(id),
    revision INTEGER NOT NULL,
    opportunity_id TEXT NOT NULL REFERENCES experience_opportunities(id),
    priority INTEGER NOT NULL CHECK(priority > 0),
    data TEXT NOT NULL CHECK(json_valid(data)),
    PRIMARY KEY(portfolio_id, revision, opportunity_id)
);

CREATE TABLE portfolio_deferrals (
    portfolio_id TEXT NOT NULL REFERENCES experience_portfolios(id),
    revision INTEGER NOT NULL,
    opportunity_id TEXT NOT NULL REFERENCES experience_opportunities(id),
    data TEXT NOT NULL CHECK(json_valid(data)),
    PRIMARY KEY(portfolio_id, revision, opportunity_id)
);

CREATE TABLE budget_ledgers (
    id TEXT PRIMARY KEY CHECK(id LIKE 'budget-ledger-%'),
    portfolio_id TEXT NOT NULL REFERENCES experience_portfolios(id),
    revision INTEGER NOT NULL,
    data TEXT NOT NULL CHECK(json_valid(data)),
    UNIQUE(portfolio_id, revision)
);

CREATE TABLE opportunity_results (
    opportunity_id TEXT PRIMARY KEY REFERENCES experience_opportunities(id),
    outcome TEXT NOT NULL,
    completed_at TEXT NOT NULL,
    data TEXT NOT NULL CHECK(json_valid(data))
);

CREATE TABLE experiment_cost_estimates (
    opportunity_id TEXT PRIMARY KEY REFERENCES experience_opportunities(id),
    data TEXT NOT NULL CHECK(json_valid(data))
);

CREATE TABLE experiment_actual_costs (
    opportunity_id TEXT PRIMARY KEY REFERENCES opportunity_results(opportunity_id),
    data TEXT NOT NULL CHECK(json_valid(data))
);

CREATE TABLE evidence_saturation_records (
    opportunity_id TEXT NOT NULL REFERENCES experience_opportunities(id),
    observed_at TEXT NOT NULL,
    saturation TEXT NOT NULL,
    data TEXT NOT NULL CHECK(json_valid(data)),
    PRIMARY KEY(opportunity_id, observed_at)
);

CREATE TABLE experience_debt (
    target_key TEXT PRIMARY KEY,
    severity TEXT NOT NULL,
    data TEXT NOT NULL CHECK(json_valid(data))
);

CREATE TABLE experience_economics_events (
    sequence INTEGER PRIMARY KEY AUTOINCREMENT,
    subject TEXT NOT NULL,
    kind TEXT NOT NULL,
    created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
    data TEXT NOT NULL CHECK(json_valid(data))
);
CREATE INDEX idx_economics_events_subject ON experience_economics_events(subject, sequence);

CREATE TRIGGER experience_portfolio_revisions_no_update
BEFORE UPDATE ON experience_portfolio_revisions BEGIN SELECT RAISE(ABORT, 'portfolio revisions are append-only'); END;
CREATE TRIGGER experience_portfolio_revisions_no_delete
BEFORE DELETE ON experience_portfolio_revisions BEGIN SELECT RAISE(ABORT, 'portfolio revisions are append-only'); END;
CREATE TRIGGER opportunity_results_no_update
BEFORE UPDATE ON opportunity_results BEGIN SELECT RAISE(ABORT, 'opportunity results are append-only'); END;
CREATE TRIGGER opportunity_results_no_delete
BEFORE DELETE ON opportunity_results BEGIN SELECT RAISE(ABORT, 'opportunity results are append-only'); END;
