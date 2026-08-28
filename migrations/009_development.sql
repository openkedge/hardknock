CREATE INDEX development_agent_time ON experiences(json_extract(data,'$.agent.kind'),created_at,id);
CREATE INDEX development_repository_time ON experiences(json_extract(data,'$.context.repository.path'),created_at,id);
CREATE VIEW development_observations AS SELECT id,created_at,json_object(
 'id',id,'created_at',created_at,'agent',json_extract(data,'$.agent'),'context',json_extract(data,'$.context'),
 'outcome',json_extract(data,'$.outcome'),'goal',json_extract(data,'$.goal'),'tree_hash',json_extract(data,'$.starting_state.tree_hash'),
 'perturbed',json(CASE WHEN json_array_length(data,'$.perturbations')>0 OR json_array_length(data,'$.resilience.perturbation_ids')>0 THEN 'true' ELSE 'false' END),
 'task',json(CASE WHEN json_extract(data,'$.experiment') IS NULL AND json_extract(data,'$.resilience.origin') IS NULL
 AND NOT EXISTS(SELECT 1 FROM json_each(data,'$.relations') r WHERE json_extract(r.value,'$.kind') IN ('counterfactual_of','chaos_variant_of','recovery_of')) THEN 'true' ELSE 'false' END),
 'audited',json(CASE WHEN json_array_length(data,'$.observed_actions')>0 THEN 'true' ELSE 'false' END),
 'repeated_mistake',json(CASE WHEN json_array_length(data,'$.repeated_mistakes')>0 THEN 'true' ELSE 'false' END),
 'failure_signatures',(SELECT json_group_array(json_extract(f.value,'$.signature')) FROM json_each(data,'$.failure_signatures') f),
 'applications',(SELECT json_group_array(json_object('lesson_id',json_extract(a.value,'$.lesson_id'),'lesson_version',json_extract(a.value,'$.lesson_version'),'influence',json_extract(a.value,'$.influence'),'verification',json_extract(a.value,'$.verification'),'delivered',json(CASE WHEN json_extract(a.value,'$.delivered') THEN 'true' ELSE 'false' END))) FROM json_each(data,'$.lesson_applications') a),
 'recovery',json_extract(data,'$.resilience.recovery_attempt'),
 'reflex_firings',coalesce(json_array_length(data,'$.resilience.reflex_matches'),0)
) AS data FROM experiences;
CREATE TABLE experience_profiles (id TEXT PRIMARY KEY, subject TEXT NOT NULL, policy_hash TEXT NOT NULL, updated_at TEXT NOT NULL, data TEXT NOT NULL CHECK(json_valid(data)));
CREATE TABLE profile_snapshots (id TEXT PRIMARY KEY, profile_id TEXT NOT NULL REFERENCES experience_profiles(id), captured_at TEXT NOT NULL, data TEXT NOT NULL CHECK(json_valid(data)));
CREATE INDEX profile_history ON profile_snapshots(profile_id,captured_at,id);
CREATE TABLE snapshot_evidence (snapshot_id TEXT NOT NULL REFERENCES profile_snapshots(id),experience_id TEXT NOT NULL REFERENCES experiences(id),PRIMARY KEY(snapshot_id,experience_id));
CREATE TABLE development_episodes (id TEXT PRIMARY KEY,started_at TEXT NOT NULL,ended_at TEXT,data TEXT NOT NULL CHECK(json_valid(data)));
CREATE TABLE episode_evidence (episode_id TEXT NOT NULL REFERENCES development_episodes(id),experience_id TEXT NOT NULL REFERENCES experiences(id),PRIMARY KEY(episode_id,experience_id));
CREATE TABLE skill_revisions (skill_id TEXT NOT NULL REFERENCES skills(id),revision INTEGER NOT NULL,created_at TEXT NOT NULL,source_experience TEXT NOT NULL REFERENCES experiences(id),data TEXT NOT NULL CHECK(json_valid(data)),PRIMARY KEY(skill_id,revision));
INSERT INTO skill_revisions SELECT s.id,1,e.created_at,s.source_experience_id,json_object('skill_id',s.id,'revision',1,'created_at',e.created_at,'source_experience',s.source_experience_id,'procedure',json_extract(s.data,'$.procedure'),'context',json_extract(s.data,'$.context'),'evidence',json_extract(s.data,'$.evidence'),'parent_revision',NULL) FROM skills s JOIN experiences e ON e.id=s.source_experience_id;
CREATE TRIGGER skill_initial_revision AFTER INSERT ON skills BEGIN
 INSERT INTO skill_revisions SELECT NEW.id,1,e.created_at,NEW.source_experience_id,json_object('skill_id',NEW.id,'revision',1,'created_at',e.created_at,'source_experience',NEW.source_experience_id,'procedure',json_extract(NEW.data,'$.procedure'),'context',json_extract(NEW.data,'$.context'),'evidence',json_extract(NEW.data,'$.evidence'),'parent_revision',NULL) FROM experiences e WHERE e.id=NEW.source_experience_id;
END;
CREATE TABLE revalidation_queue (id TEXT PRIMARY KEY, dedup_key TEXT NOT NULL UNIQUE, created_at TEXT NOT NULL,status TEXT NOT NULL, data TEXT NOT NULL CHECK(json_valid(data)));
CREATE TABLE development_regressions (from_snapshot TEXT NOT NULL REFERENCES profile_snapshots(id),to_snapshot TEXT NOT NULL REFERENCES profile_snapshots(id),metric TEXT NOT NULL,data TEXT NOT NULL CHECK(json_valid(data)),PRIMARY KEY(from_snapshot,to_snapshot,metric));
CREATE TABLE experience_package_revisions (package_id TEXT NOT NULL,skill_id TEXT NOT NULL REFERENCES skills(id),revision INTEGER NOT NULL,skill_revision INTEGER NOT NULL,created_at TEXT NOT NULL,evidence_hash TEXT NOT NULL,data TEXT NOT NULL CHECK(json_valid(data)),PRIMARY KEY(package_id,revision),FOREIGN KEY(skill_id,skill_revision) REFERENCES skill_revisions(skill_id,revision));
CREATE INDEX package_revision_skill ON experience_package_revisions(skill_id,created_at,revision);
CREATE TABLE benchmark_runs (id TEXT PRIMARY KEY,created_at TEXT NOT NULL,status TEXT NOT NULL,data TEXT NOT NULL CHECK(json_valid(data)));
CREATE TABLE benchmark_metrics (run_id TEXT NOT NULL REFERENCES benchmark_runs(id),arm TEXT NOT NULL,episode INTEGER NOT NULL,metric TEXT NOT NULL,sample_count INTEGER NOT NULL,value REAL,PRIMARY KEY(run_id,arm,episode,metric));
CREATE TRIGGER snapshot_no_update BEFORE UPDATE ON profile_snapshots BEGIN SELECT RAISE(ABORT,'Snapshots are immutable'); END;
CREATE TRIGGER snapshot_no_delete BEFORE DELETE ON profile_snapshots BEGIN SELECT RAISE(ABORT,'Snapshots are immutable'); END;
CREATE TRIGGER snapshot_evidence_no_update BEFORE UPDATE ON snapshot_evidence BEGIN SELECT RAISE(ABORT,'Snapshot evidence is immutable'); END;
CREATE TRIGGER snapshot_evidence_no_delete BEFORE DELETE ON snapshot_evidence BEGIN SELECT RAISE(ABORT,'Snapshot evidence is immutable'); END;
CREATE TRIGGER snapshot_evidence_membership BEFORE INSERT ON snapshot_evidence WHEN NOT EXISTS(SELECT 1 FROM profile_snapshots s,json_each(s.data,'$.evidence_ids') e WHERE s.id=NEW.snapshot_id AND e.value=NEW.experience_id) BEGIN SELECT RAISE(ABORT,'Evidence is not in the immutable snapshot'); END;
CREATE TRIGGER skill_revision_no_update BEFORE UPDATE ON skill_revisions BEGIN SELECT RAISE(ABORT,'Skill revisions are immutable'); END;
CREATE TRIGGER skill_revision_no_delete BEFORE DELETE ON skill_revisions BEGIN SELECT RAISE(ABORT,'Skill revisions are immutable'); END;
CREATE TRIGGER episode_terminal BEFORE UPDATE ON development_episodes WHEN OLD.ended_at IS NOT NULL BEGIN SELECT RAISE(ABORT,'Completed episodes are immutable'); END;
CREATE TRIGGER episode_no_delete BEFORE DELETE ON development_episodes BEGIN SELECT RAISE(ABORT,'Episode history is retained'); END;
CREATE TRIGGER episode_evidence_no_update BEFORE UPDATE ON episode_evidence BEGIN SELECT RAISE(ABORT,'Episode evidence is immutable'); END;
CREATE TRIGGER episode_evidence_no_delete BEFORE DELETE ON episode_evidence BEGIN SELECT RAISE(ABORT,'Episode evidence is immutable'); END;
CREATE TRIGGER episode_evidence_membership BEFORE INSERT ON episode_evidence WHEN NOT EXISTS(SELECT 1 FROM development_episodes s,json_each(s.data,'$.experiences') e WHERE s.id=NEW.episode_id AND e.value=NEW.experience_id) BEGIN SELECT RAISE(ABORT,'Evidence is not in the episode'); END;
CREATE TRIGGER revalidation_terminal BEFORE UPDATE ON revalidation_queue WHEN OLD.status!='pending' BEGIN SELECT RAISE(ABORT,'Revalidation result is immutable'); END;
CREATE TRIGGER revalidation_no_delete BEFORE DELETE ON revalidation_queue BEGIN SELECT RAISE(ABORT,'Revalidation history is retained'); END;
CREATE TRIGGER package_revision_no_update BEFORE UPDATE ON experience_package_revisions BEGIN SELECT RAISE(ABORT,'Package revisions are immutable'); END;
CREATE TRIGGER package_revision_no_delete BEFORE DELETE ON experience_package_revisions BEGIN SELECT RAISE(ABORT,'Package revisions are immutable'); END;
CREATE TRIGGER regression_no_update BEFORE UPDATE ON development_regressions BEGIN SELECT RAISE(ABORT,'Regression observations are immutable'); END;
CREATE TRIGGER regression_no_delete BEFORE DELETE ON development_regressions BEGIN SELECT RAISE(ABORT,'Regression observations are immutable'); END;
CREATE TRIGGER benchmark_terminal BEFORE UPDATE ON benchmark_runs WHEN OLD.status!='running' BEGIN SELECT RAISE(ABORT,'Benchmark result is immutable'); END;
CREATE TRIGGER benchmark_no_delete BEFORE DELETE ON benchmark_runs BEGIN SELECT RAISE(ABORT,'Benchmark history is retained'); END;
CREATE TRIGGER benchmark_metrics_no_update BEFORE UPDATE ON benchmark_metrics BEGIN SELECT RAISE(ABORT,'Benchmark metrics are immutable'); END;
CREATE TRIGGER benchmark_metrics_no_delete BEFORE DELETE ON benchmark_metrics BEGIN SELECT RAISE(ABORT,'Benchmark metrics are immutable'); END;
CREATE TRIGGER benchmark_metrics_terminal BEFORE INSERT ON benchmark_metrics WHEN (SELECT status FROM benchmark_runs WHERE id=NEW.run_id)!='running' BEGIN SELECT RAISE(ABORT,'Benchmark is terminal'); END;
