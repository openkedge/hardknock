CREATE TABLE curricula (
 id TEXT PRIMARY KEY, created_at TEXT NOT NULL, session_id TEXT,
 status TEXT NOT NULL, revision INTEGER NOT NULL, cancel_requested INTEGER NOT NULL DEFAULT 0,
 data TEXT NOT NULL CHECK(json_valid(data))
);
CREATE INDEX curricula_session ON curricula(session_id,created_at);
CREATE TABLE curriculum_goals (
 id TEXT PRIMARY KEY, curriculum_id TEXT NOT NULL REFERENCES curricula(id),
 data TEXT NOT NULL CHECK(json_valid(data))
);
CREATE TABLE evidence_gaps (
 goal_id TEXT PRIMARY KEY REFERENCES curriculum_goals(id), data TEXT NOT NULL CHECK(json_valid(data))
);
CREATE TABLE curriculum_trials (
 id TEXT PRIMARY KEY, curriculum_id TEXT NOT NULL REFERENCES curricula(id),
 goal_id TEXT NOT NULL REFERENCES curriculum_goals(id), skill_id TEXT NOT NULL REFERENCES skills(id),
 fingerprint TEXT NOT NULL, data TEXT NOT NULL CHECK(json_valid(data))
);
CREATE INDEX curriculum_novelty ON curriculum_trials(skill_id,fingerprint);
CREATE TABLE curriculum_engine_links (trial_id TEXT PRIMARY KEY REFERENCES curriculum_trials(id), kind TEXT NOT NULL, record_id TEXT NOT NULL);
CREATE TRIGGER curriculum_link_immutable BEFORE UPDATE ON curriculum_engine_links BEGIN SELECT RAISE(ABORT,'Engine reference is immutable'); END;
CREATE TABLE curriculum_events (
 sequence INTEGER PRIMARY KEY AUTOINCREMENT, curriculum_id TEXT NOT NULL REFERENCES curricula(id),
 data TEXT NOT NULL CHECK(json_valid(data))
);
CREATE TABLE task_families (id TEXT PRIMARY KEY, name TEXT NOT NULL UNIQUE, data TEXT NOT NULL CHECK(json_valid(data)));
CREATE TABLE skill_coverage (skill_id TEXT NOT NULL REFERENCES skills(id), profile TEXT NOT NULL, data TEXT NOT NULL CHECK(json_valid(data)), PRIMARY KEY(skill_id,profile));
CREATE TABLE skill_usage (skill_id TEXT PRIMARY KEY REFERENCES skills(id), data TEXT NOT NULL CHECK(json_valid(data)));
CREATE TABLE experience_packages (skill_id TEXT NOT NULL REFERENCES skills(id), profile TEXT NOT NULL, created_at TEXT NOT NULL, data TEXT NOT NULL CHECK(json_valid(data)), PRIMARY KEY(skill_id,profile,created_at));
CREATE TABLE curriculum_reviews (lesson_id TEXT NOT NULL REFERENCES lessons(id), trial_id TEXT NOT NULL REFERENCES curriculum_trials(id), reason TEXT NOT NULL, PRIMARY KEY(lesson_id,trial_id));
CREATE TRIGGER curriculum_identity BEFORE UPDATE ON curricula
 WHEN OLD.id != NEW.id OR OLD.created_at != NEW.created_at OR OLD.session_id IS NOT NEW.session_id
 OR json_extract(OLD.data,'$.target') != json_extract(NEW.data,'$.target')
 OR json_extract(OLD.data,'$.budget') != json_extract(NEW.data,'$.budget')
 BEGIN SELECT RAISE(ABORT,'Curriculum identity and budget are immutable'); END;
CREATE TRIGGER curriculum_terminal BEFORE UPDATE ON curricula
 WHEN OLD.status NOT IN ('planned','running') BEGIN SELECT RAISE(ABORT,'Curriculum is terminal'); END;
CREATE TRIGGER curriculum_no_reset BEFORE UPDATE ON curricula
 WHEN OLD.status='running' AND NEW.status='planned' BEGIN SELECT RAISE(ABORT,'Cannot reset a running curriculum'); END;
CREATE TRIGGER curriculum_trial_plan BEFORE UPDATE ON curriculum_trials
 WHEN OLD.id != NEW.id OR OLD.curriculum_id != NEW.curriculum_id OR OLD.goal_id != NEW.goal_id OR OLD.skill_id != NEW.skill_id OR OLD.fingerprint != NEW.fingerprint
 OR json_extract(OLD.data,'$.execution') != json_extract(NEW.data,'$.execution')
 OR (json_extract(OLD.data,'$.result') IS NOT NULL AND json_extract(OLD.data,'$.result') IS NOT json_extract(NEW.data,'$.result'))
 OR (json_extract(OLD.data,'$.result') IS NOT NULL AND OLD.data != NEW.data)
 BEGIN SELECT RAISE(ABORT,'Trial plan and recorded evidence are immutable'); END;
CREATE TRIGGER curriculum_event_immutable BEFORE UPDATE ON curriculum_events BEGIN SELECT RAISE(ABORT,'Events are immutable'); END;
CREATE TRIGGER curriculum_event_no_delete BEFORE DELETE ON curriculum_events BEGIN SELECT RAISE(ABORT,'Events are immutable'); END;
CREATE TRIGGER curriculum_gap_immutable BEFORE UPDATE ON evidence_gaps BEGIN SELECT RAISE(ABORT,'Evidence gap rationale is immutable'); END;
CREATE TRIGGER package_immutable BEFORE UPDATE ON experience_packages BEGIN SELECT RAISE(ABORT,'Package snapshots are immutable'); END;
CREATE TRIGGER package_no_delete BEFORE DELETE ON experience_packages BEGIN SELECT RAISE(ABORT,'Package snapshots are immutable'); END;
CREATE TRIGGER curriculum_no_delete BEFORE DELETE ON curricula BEGIN SELECT RAISE(ABORT,'Curriculum history is retained'); END;
CREATE TRIGGER curriculum_trial_no_delete BEFORE DELETE ON curriculum_trials BEGIN SELECT RAISE(ABORT,'Trial history is retained'); END;
CREATE TRIGGER curriculum_goal_no_delete BEFORE DELETE ON curriculum_goals BEGIN SELECT RAISE(ABORT,'Goal history is retained'); END;
CREATE TRIGGER curriculum_gap_no_delete BEFORE DELETE ON evidence_gaps BEGIN SELECT RAISE(ABORT,'Evidence gap history is retained'); END;
CREATE TRIGGER curriculum_link_no_delete BEFORE DELETE ON curriculum_engine_links BEGIN SELECT RAISE(ABORT,'Engine provenance is retained'); END;
