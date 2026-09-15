-- SPDX-License-Identifier: Apache-2.0
CREATE TABLE execution_plans(id TEXT PRIMARY KEY, revision INTEGER NOT NULL, name TEXT NOT NULL, data TEXT NOT NULL CHECK(json_valid(data)));
CREATE TABLE plan_revisions(id TEXT NOT NULL, revision INTEGER NOT NULL, hash TEXT NOT NULL, data TEXT NOT NULL CHECK(json_valid(data)), PRIMARY KEY(id,revision));
CREATE TABLE plan_runs(id TEXT PRIMARY KEY, plan TEXT NOT NULL REFERENCES execution_plans(id), data TEXT NOT NULL CHECK(json_valid(data)));
CREATE TABLE plan_observations(id TEXT PRIMARY KEY, run TEXT NOT NULL REFERENCES plan_runs(id), data TEXT NOT NULL CHECK(json_valid(data)));
CREATE TABLE plan_validity_assessments(id TEXT PRIMARY KEY, run TEXT NOT NULL REFERENCES plan_runs(id), data TEXT NOT NULL CHECK(json_valid(data)));
CREATE TABLE plan_step_runs(run TEXT NOT NULL REFERENCES plan_runs(id), step TEXT NOT NULL, data TEXT NOT NULL CHECK(json_valid(data)), PRIMARY KEY(run,step));
CREATE TABLE plan_checkpoint_snapshots(id TEXT PRIMARY KEY, run TEXT NOT NULL REFERENCES plan_runs(id), data TEXT NOT NULL CHECK(json_valid(data)));
CREATE TABLE plan_events(id INTEGER PRIMARY KEY, plan TEXT NOT NULL REFERENCES execution_plans(id), run TEXT, kind TEXT NOT NULL, data TEXT NOT NULL CHECK(json_valid(data)), created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP);
CREATE TRIGGER immutable_plan_revision_update BEFORE UPDATE ON plan_revisions BEGIN SELECT RAISE(ABORT,'immutable plan revision'); END;
CREATE TRIGGER immutable_plan_revision_delete BEFORE DELETE ON plan_revisions BEGIN SELECT RAISE(ABORT,'immutable plan revision'); END;
CREATE TRIGGER immutable_plan_observation_update BEFORE UPDATE ON plan_observations BEGIN SELECT RAISE(ABORT,'immutable plan observation'); END;
CREATE TRIGGER immutable_plan_observation_delete BEFORE DELETE ON plan_observations BEGIN SELECT RAISE(ABORT,'immutable plan observation'); END;
CREATE TRIGGER immutable_plan_assessment_update BEFORE UPDATE ON plan_validity_assessments BEGIN SELECT RAISE(ABORT,'immutable plan assessment'); END;
CREATE TRIGGER immutable_plan_assessment_delete BEFORE DELETE ON plan_validity_assessments BEGIN SELECT RAISE(ABORT,'immutable plan assessment'); END;
CREATE TRIGGER immutable_plan_step_update BEFORE UPDATE ON plan_step_runs BEGIN SELECT RAISE(ABORT,'immutable plan step'); END;
CREATE TRIGGER immutable_plan_step_delete BEFORE DELETE ON plan_step_runs BEGIN SELECT RAISE(ABORT,'immutable plan step'); END;
CREATE TRIGGER immutable_plan_checkpoint_update BEFORE UPDATE ON plan_checkpoint_snapshots BEGIN SELECT RAISE(ABORT,'immutable plan checkpoint'); END;
CREATE TRIGGER immutable_plan_checkpoint_delete BEFORE DELETE ON plan_checkpoint_snapshots BEGIN SELECT RAISE(ABORT,'immutable plan checkpoint'); END;
