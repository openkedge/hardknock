-- SPDX-License-Identifier: Apache-2.0
CREATE TABLE agent_handoffs(id TEXT PRIMARY KEY, team TEXT NOT NULL REFERENCES agent_teams(id), review TEXT NOT NULL REFERENCES team_reviews(id), data TEXT NOT NULL CHECK(json_valid(data)));
CREATE INDEX agent_handoffs_team ON agent_handoffs(team);
CREATE TRIGGER immutable_agent_handoff_update BEFORE UPDATE ON agent_handoffs BEGIN SELECT RAISE(ABORT,'immutable agent handoff'); END;
CREATE TRIGGER immutable_agent_handoff_delete BEFORE DELETE ON agent_handoffs BEGIN SELECT RAISE(ABORT,'immutable agent handoff'); END;
