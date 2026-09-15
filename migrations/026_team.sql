-- SPDX-License-Identifier: Apache-2.0
CREATE TABLE agent_teams(id TEXT PRIMARY KEY, revision INTEGER NOT NULL, data TEXT NOT NULL CHECK(json_valid(data)));
CREATE TABLE agent_team_revisions(id TEXT NOT NULL, revision INTEGER NOT NULL, data TEXT NOT NULL CHECK(json_valid(data)), PRIMARY KEY(id,revision));
CREATE TABLE agent_delegations(id TEXT PRIMARY KEY, team TEXT NOT NULL REFERENCES agent_teams(id), data TEXT NOT NULL CHECK(json_valid(data)));
CREATE TABLE delegation_revocations(id TEXT PRIMARY KEY REFERENCES agent_delegations(id), reason TEXT NOT NULL, created_at TEXT NOT NULL);
CREATE TABLE team_events(id INTEGER PRIMARY KEY, team TEXT NOT NULL REFERENCES agent_teams(id), kind TEXT NOT NULL, data TEXT NOT NULL CHECK(json_valid(data)), created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP);
CREATE TRIGGER immutable_team_revision_update BEFORE UPDATE ON agent_team_revisions BEGIN SELECT RAISE(ABORT,'immutable team revision'); END;
CREATE TRIGGER immutable_team_revision_delete BEFORE DELETE ON agent_team_revisions BEGIN SELECT RAISE(ABORT,'immutable team revision'); END;
CREATE TRIGGER immutable_delegation_update BEFORE UPDATE ON agent_delegations BEGIN SELECT RAISE(ABORT,'immutable delegation'); END;
CREATE TRIGGER immutable_delegation_delete BEFORE DELETE ON agent_delegations BEGIN SELECT RAISE(ABORT,'immutable delegation'); END;
CREATE TRIGGER immutable_revocation_update BEFORE UPDATE ON delegation_revocations BEGIN SELECT RAISE(ABORT,'immutable revocation'); END;
CREATE TRIGGER immutable_revocation_delete BEFORE DELETE ON delegation_revocations BEGIN SELECT RAISE(ABORT,'immutable revocation'); END;
CREATE TRIGGER immutable_team_event_update BEFORE UPDATE ON team_events BEGIN SELECT RAISE(ABORT,'immutable team event'); END;
CREATE TRIGGER immutable_team_event_delete BEFORE DELETE ON team_events BEGIN SELECT RAISE(ABORT,'immutable team event'); END;
