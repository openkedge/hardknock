-- SPDX-License-Identifier: Apache-2.0
-- Typed documents share an immutable journal; current applicability is rederived.
CREATE TABLE team_governance(team TEXT NOT NULL REFERENCES agent_teams(id), revision INTEGER NOT NULL, data TEXT NOT NULL CHECK(json_valid(data)), PRIMARY KEY(team,revision));
CREATE TABLE team_records(kind TEXT NOT NULL, id TEXT NOT NULL, team TEXT NOT NULL REFERENCES agent_teams(id), data TEXT NOT NULL CHECK(json_valid(data)), PRIMARY KEY(kind,id));
CREATE INDEX team_records_team ON team_records(team,kind);
CREATE TABLE effect_actor_contexts(effect_id TEXT PRIMARY KEY REFERENCES effects(id), team TEXT NOT NULL REFERENCES agent_teams(id), data TEXT NOT NULL CHECK(json_valid(data)));
CREATE TRIGGER immutable_team_governance_update BEFORE UPDATE ON team_governance BEGIN SELECT RAISE(ABORT,'immutable team governance'); END;
CREATE TRIGGER immutable_team_governance_delete BEFORE DELETE ON team_governance BEGIN SELECT RAISE(ABORT,'immutable team governance'); END;
CREATE TRIGGER immutable_team_records_update BEFORE UPDATE ON team_records BEGIN SELECT RAISE(ABORT,'immutable team record'); END;
CREATE TRIGGER immutable_team_records_delete BEFORE DELETE ON team_records BEGIN SELECT RAISE(ABORT,'immutable team record'); END;
CREATE TRIGGER immutable_effect_actor_update BEFORE UPDATE ON effect_actor_contexts BEGIN SELECT RAISE(ABORT,'immutable effect actor'); END;
CREATE TRIGGER immutable_effect_actor_delete BEFORE DELETE ON effect_actor_contexts BEGIN SELECT RAISE(ABORT,'immutable effect actor'); END;
