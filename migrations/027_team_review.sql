-- SPDX-License-Identifier: Apache-2.0
CREATE TABLE team_reviews(id TEXT PRIMARY KEY, team TEXT NOT NULL REFERENCES agent_teams(id), action_hash TEXT NOT NULL, data TEXT NOT NULL CHECK(json_valid(data)));
CREATE INDEX team_review_target ON team_reviews(team,action_hash);
CREATE TABLE agent_contributions(id TEXT PRIMARY KEY, review TEXT NOT NULL REFERENCES team_reviews(id), data TEXT NOT NULL CHECK(json_valid(data)));
CREATE TABLE review_findings(id TEXT PRIMARY KEY, review TEXT NOT NULL REFERENCES team_reviews(id), contribution TEXT NOT NULL REFERENCES agent_contributions(id), data TEXT NOT NULL CHECK(json_valid(data)));
CREATE TABLE review_finding_resolutions(finding TEXT PRIMARY KEY REFERENCES review_findings(id), data TEXT NOT NULL CHECK(json_valid(data)));
CREATE TRIGGER immutable_team_reviews_update BEFORE UPDATE ON team_reviews BEGIN SELECT RAISE(ABORT,'immutable team_reviews'); END;
CREATE TRIGGER immutable_team_reviews_delete BEFORE DELETE ON team_reviews BEGIN SELECT RAISE(ABORT,'immutable team_reviews'); END;
CREATE TRIGGER immutable_agent_contributions_update BEFORE UPDATE ON agent_contributions BEGIN SELECT RAISE(ABORT,'immutable agent_contributions'); END;
CREATE TRIGGER immutable_agent_contributions_delete BEFORE DELETE ON agent_contributions BEGIN SELECT RAISE(ABORT,'immutable agent_contributions'); END;
CREATE TRIGGER immutable_review_findings_update BEFORE UPDATE ON review_findings BEGIN SELECT RAISE(ABORT,'immutable review_findings'); END;
CREATE TRIGGER immutable_review_findings_delete BEFORE DELETE ON review_findings BEGIN SELECT RAISE(ABORT,'immutable review_findings'); END;
CREATE TRIGGER immutable_review_finding_resolutions_update BEFORE UPDATE ON review_finding_resolutions BEGIN SELECT RAISE(ABORT,'immutable review_finding_resolutions'); END;
CREATE TRIGGER immutable_review_finding_resolutions_delete BEFORE DELETE ON review_finding_resolutions BEGIN SELECT RAISE(ABORT,'immutable review_finding_resolutions'); END;
CREATE INDEX agent_contributions_review ON agent_contributions(review);
CREATE INDEX review_findings_review ON review_findings(review);
