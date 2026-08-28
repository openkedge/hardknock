-- Add relation variants without rewriting any historical Experience JSON.
DROP TRIGGER relations_immutable_update;
DROP TRIGGER relations_immutable_delete;
ALTER TABLE experience_relations RENAME TO old_experience_relations;
CREATE TABLE experience_relations (
    source_experience_id TEXT NOT NULL REFERENCES experiences(id),
    target_experience_id TEXT NOT NULL REFERENCES experiences(id),
    relation_type TEXT NOT NULL CHECK(relation_type IN ('retry_of','counterfactual_of','transfer_from','chaos_variant_of','recovery_of')),
    PRIMARY KEY(source_experience_id,target_experience_id,relation_type),
    CHECK(source_experience_id != target_experience_id)
);
INSERT INTO experience_relations SELECT * FROM old_experience_relations;
DROP TABLE old_experience_relations;
DROP TRIGGER lesson_evidence_immutable_update;
DROP TRIGGER lesson_evidence_immutable_delete;
ALTER TABLE lesson_evidence RENAME TO old_lesson_evidence;
CREATE TABLE lesson_evidence (
    lesson_id TEXT NOT NULL REFERENCES lessons(id), evidence_key TEXT NOT NULL,
    experience_id TEXT REFERENCES experiences(id), experiment_id TEXT, trial_id TEXT,
    relationship TEXT NOT NULL CHECK(relationship IN ('origin','supports','contradicts','inconclusive','narrows')),
    PRIMARY KEY(lesson_id,evidence_key),
    FOREIGN KEY(experiment_id,trial_id) REFERENCES trials(experiment_id,id),
    CHECK((experience_id IS NOT NULL AND experiment_id IS NULL AND trial_id IS NULL) OR
          (experience_id IS NULL AND experiment_id IS NOT NULL AND trial_id IS NOT NULL))
);
INSERT INTO lesson_evidence SELECT * FROM old_lesson_evidence;
DROP TABLE old_lesson_evidence;
CREATE TABLE perturbations (id TEXT PRIMARY KEY NOT NULL, data TEXT NOT NULL CHECK(json_valid(data)));
CREATE TABLE skills (
    id TEXT PRIMARY KEY NOT NULL, name TEXT NOT NULL UNIQUE,
    source_experience_id TEXT NOT NULL REFERENCES experiences(id),
    data TEXT NOT NULL CHECK(json_valid(data))
);
CREATE TABLE chaos_campaigns (
    id TEXT PRIMARY KEY NOT NULL, created_at TEXT NOT NULL,
    skill_id TEXT REFERENCES skills(id), status TEXT NOT NULL,
    data TEXT NOT NULL CHECK(json_valid(data))
);
CREATE TABLE campaign_perturbations (
    campaign_id TEXT NOT NULL REFERENCES chaos_campaigns(id),
    perturbation_id TEXT NOT NULL REFERENCES perturbations(id),
    trial_index INTEGER NOT NULL CHECK(trial_index > 0),
    PRIMARY KEY(campaign_id,trial_index,perturbation_id)
);
CREATE TABLE chaos_trials (
    id TEXT PRIMARY KEY NOT NULL, campaign_id TEXT NOT NULL REFERENCES chaos_campaigns(id),
    trial_index INTEGER NOT NULL CHECK(trial_index >= 0),
    experience_id TEXT NOT NULL UNIQUE REFERENCES experiences(id),
    control_experience_id TEXT REFERENCES experiences(id),
    reality_id TEXT NOT NULL REFERENCES realities(id),
    execution_id TEXT NOT NULL REFERENCES executions(id),
    evaluation_id TEXT NOT NULL REFERENCES evaluations(id),
    data TEXT NOT NULL CHECK(json_valid(data)),
    UNIQUE(campaign_id,trial_index), UNIQUE(campaign_id,id),
    CHECK((trial_index=0 AND control_experience_id IS NULL) OR (trial_index>0 AND control_experience_id IS NOT NULL))
);
CREATE TABLE experience_perturbations (
    experience_id TEXT NOT NULL REFERENCES experiences(id),
    perturbation_id TEXT NOT NULL REFERENCES perturbations(id),
    PRIMARY KEY(experience_id,perturbation_id)
);
CREATE TABLE chaos_trial_lessons (
    trial_id TEXT NOT NULL REFERENCES chaos_trials(id), lesson_id TEXT NOT NULL REFERENCES lessons(id),
    PRIMARY KEY(trial_id,lesson_id)
);
CREATE TABLE operating_envelopes (
    id TEXT PRIMARY KEY NOT NULL, campaign_id TEXT NOT NULL UNIQUE REFERENCES chaos_campaigns(id),
    version INTEGER NOT NULL CHECK(version>0), data TEXT NOT NULL CHECK(json_valid(data))
);
CREATE TABLE operating_envelope_versions (
    envelope_id TEXT NOT NULL REFERENCES operating_envelopes(id), version INTEGER NOT NULL,
    data TEXT NOT NULL CHECK(json_valid(data)), PRIMARY KEY(envelope_id,version)
);
CREATE TABLE operating_envelope_observations (
    envelope_id TEXT NOT NULL REFERENCES operating_envelopes(id),
    trial_id TEXT NOT NULL REFERENCES chaos_trials(id),
    experience_id TEXT NOT NULL REFERENCES experiences(id),
    outcome TEXT NOT NULL, data TEXT NOT NULL CHECK(json_valid(data)),
    PRIMARY KEY(envelope_id,trial_id)
);
CREATE TABLE reflexes (
    id TEXT PRIMARY KEY NOT NULL, source_trial TEXT NOT NULL REFERENCES chaos_trials(id),
    version INTEGER NOT NULL CHECK(version>0), data TEXT NOT NULL CHECK(json_valid(data))
);
CREATE TABLE reflex_versions (
    reflex_id TEXT NOT NULL REFERENCES reflexes(id), version INTEGER NOT NULL,
    data TEXT NOT NULL CHECK(json_valid(data)), PRIMARY KEY(reflex_id,version)
);
CREATE TABLE reflex_lessons (
    reflex_id TEXT NOT NULL REFERENCES reflexes(id), lesson_id TEXT NOT NULL REFERENCES lessons(id),
    PRIMARY KEY(reflex_id,lesson_id)
);
CREATE TABLE reflex_evidence (
    reflex_id TEXT NOT NULL REFERENCES reflexes(id), experience_id TEXT NOT NULL REFERENCES experiences(id),
    relationship TEXT NOT NULL, PRIMARY KEY(reflex_id,experience_id)
);
CREATE TABLE reflex_matches (
    experience_id TEXT NOT NULL REFERENCES experiences(id), position INTEGER NOT NULL,
    reflex_id TEXT NOT NULL, reflex_version INTEGER NOT NULL, data TEXT NOT NULL CHECK(json_valid(data)),
    PRIMARY KEY(experience_id,position), FOREIGN KEY(reflex_id,reflex_version) REFERENCES reflex_versions(reflex_id,version)
);
CREATE TABLE recoveries (
    id TEXT PRIMARY KEY NOT NULL, source_trial TEXT NOT NULL REFERENCES chaos_trials(id),
    version INTEGER NOT NULL CHECK(version>0), data TEXT NOT NULL CHECK(json_valid(data))
);
CREATE TABLE recovery_versions (
    recovery_id TEXT NOT NULL REFERENCES recoveries(id), version INTEGER NOT NULL,
    data TEXT NOT NULL CHECK(json_valid(data)), PRIMARY KEY(recovery_id,version)
);
CREATE TABLE recovery_steps (
    recovery_id TEXT NOT NULL, recovery_version INTEGER NOT NULL, position INTEGER NOT NULL,
    data TEXT NOT NULL CHECK(json_valid(data)), PRIMARY KEY(recovery_id,recovery_version,position),
    FOREIGN KEY(recovery_id,recovery_version) REFERENCES recovery_versions(recovery_id,version)
);
CREATE TABLE recovery_evidence (
    recovery_id TEXT NOT NULL REFERENCES recoveries(id), experience_id TEXT NOT NULL REFERENCES experiences(id),
    relationship TEXT NOT NULL, PRIMARY KEY(recovery_id,experience_id)
);
CREATE TABLE recovery_attempts (
    experience_id TEXT PRIMARY KEY NOT NULL REFERENCES experiences(id), recovery_id TEXT NOT NULL,
    recovery_version INTEGER NOT NULL, data TEXT NOT NULL CHECK(json_valid(data)),
    FOREIGN KEY(recovery_id,recovery_version) REFERENCES recovery_versions(recovery_id,version)
);
CREATE TABLE resilience_tests (
    id TEXT PRIMARY KEY NOT NULL, created_at TEXT NOT NULL,
    reflex_id TEXT REFERENCES reflexes(id), recovery_id TEXT REFERENCES recoveries(id),
    source_trial TEXT NOT NULL REFERENCES chaos_trials(id),
    without_experience_id TEXT REFERENCES experiences(id), with_experience_id TEXT REFERENCES experiences(id),
    status TEXT NOT NULL, data TEXT NOT NULL CHECK(json_valid(data)),
    CHECK((reflex_id IS NOT NULL AND recovery_id IS NULL) OR (reflex_id IS NULL AND recovery_id IS NOT NULL))
);
CREATE TRIGGER experience_relations_immutable_update BEFORE UPDATE ON experience_relations
BEGIN SELECT RAISE(ABORT, 'experience_relations records are immutable'); END;
CREATE TRIGGER experience_relations_immutable_delete BEFORE DELETE ON experience_relations
BEGIN SELECT RAISE(ABORT, 'experience_relations records are immutable'); END;
CREATE TRIGGER lesson_evidence_immutable_update BEFORE UPDATE ON lesson_evidence
BEGIN SELECT RAISE(ABORT, 'lesson_evidence records are immutable'); END;
CREATE TRIGGER lesson_evidence_immutable_delete BEFORE DELETE ON lesson_evidence
BEGIN SELECT RAISE(ABORT, 'lesson_evidence records are immutable'); END;
CREATE TRIGGER perturbations_immutable_update BEFORE UPDATE ON perturbations
BEGIN SELECT RAISE(ABORT, 'perturbations records are immutable'); END;
CREATE TRIGGER perturbations_immutable_delete BEFORE DELETE ON perturbations
BEGIN SELECT RAISE(ABORT, 'perturbations records are immutable'); END;
CREATE TRIGGER skills_immutable_update BEFORE UPDATE ON skills
BEGIN SELECT RAISE(ABORT, 'skills records are immutable'); END;
CREATE TRIGGER skills_immutable_delete BEFORE DELETE ON skills
BEGIN SELECT RAISE(ABORT, 'skills records are immutable'); END;
CREATE TRIGGER campaign_perturbations_immutable_update BEFORE UPDATE ON campaign_perturbations
BEGIN SELECT RAISE(ABORT, 'campaign_perturbations records are immutable'); END;
CREATE TRIGGER campaign_perturbations_immutable_delete BEFORE DELETE ON campaign_perturbations
BEGIN SELECT RAISE(ABORT, 'campaign_perturbations records are immutable'); END;
CREATE TRIGGER chaos_trials_immutable_update BEFORE UPDATE ON chaos_trials
BEGIN SELECT RAISE(ABORT, 'chaos_trials records are immutable'); END;
CREATE TRIGGER chaos_trials_immutable_delete BEFORE DELETE ON chaos_trials
BEGIN SELECT RAISE(ABORT, 'chaos_trials records are immutable'); END;
CREATE TRIGGER experience_perturbations_immutable_update BEFORE UPDATE ON experience_perturbations
BEGIN SELECT RAISE(ABORT, 'experience_perturbations records are immutable'); END;
CREATE TRIGGER experience_perturbations_immutable_delete BEFORE DELETE ON experience_perturbations
BEGIN SELECT RAISE(ABORT, 'experience_perturbations records are immutable'); END;
CREATE TRIGGER chaos_trial_lessons_immutable_update BEFORE UPDATE ON chaos_trial_lessons
BEGIN SELECT RAISE(ABORT, 'chaos_trial_lessons records are immutable'); END;
CREATE TRIGGER chaos_trial_lessons_immutable_delete BEFORE DELETE ON chaos_trial_lessons
BEGIN SELECT RAISE(ABORT, 'chaos_trial_lessons records are immutable'); END;
CREATE TRIGGER operating_envelopes_immutable_update BEFORE UPDATE ON operating_envelopes
BEGIN SELECT RAISE(ABORT, 'operating_envelopes records are immutable'); END;
CREATE TRIGGER operating_envelopes_immutable_delete BEFORE DELETE ON operating_envelopes
BEGIN SELECT RAISE(ABORT, 'operating_envelopes records are immutable'); END;
CREATE TRIGGER operating_envelope_versions_immutable_update BEFORE UPDATE ON operating_envelope_versions
BEGIN SELECT RAISE(ABORT, 'operating_envelope_versions records are immutable'); END;
CREATE TRIGGER operating_envelope_versions_immutable_delete BEFORE DELETE ON operating_envelope_versions
BEGIN SELECT RAISE(ABORT, 'operating_envelope_versions records are immutable'); END;
CREATE TRIGGER operating_envelope_observations_immutable_update BEFORE UPDATE ON operating_envelope_observations
BEGIN SELECT RAISE(ABORT, 'operating_envelope_observations records are immutable'); END;
CREATE TRIGGER operating_envelope_observations_immutable_delete BEFORE DELETE ON operating_envelope_observations
BEGIN SELECT RAISE(ABORT, 'operating_envelope_observations records are immutable'); END;
CREATE TRIGGER reflex_versions_immutable_update BEFORE UPDATE ON reflex_versions
BEGIN SELECT RAISE(ABORT, 'reflex_versions records are immutable'); END;
CREATE TRIGGER reflex_versions_immutable_delete BEFORE DELETE ON reflex_versions
BEGIN SELECT RAISE(ABORT, 'reflex_versions records are immutable'); END;
CREATE TRIGGER reflex_lessons_immutable_update BEFORE UPDATE ON reflex_lessons
BEGIN SELECT RAISE(ABORT, 'reflex_lessons records are immutable'); END;
CREATE TRIGGER reflex_lessons_immutable_delete BEFORE DELETE ON reflex_lessons
BEGIN SELECT RAISE(ABORT, 'reflex_lessons records are immutable'); END;
CREATE TRIGGER reflex_evidence_immutable_update BEFORE UPDATE ON reflex_evidence
BEGIN SELECT RAISE(ABORT, 'reflex_evidence records are immutable'); END;
CREATE TRIGGER reflex_evidence_immutable_delete BEFORE DELETE ON reflex_evidence
BEGIN SELECT RAISE(ABORT, 'reflex_evidence records are immutable'); END;
CREATE TRIGGER reflex_matches_immutable_update BEFORE UPDATE ON reflex_matches
BEGIN SELECT RAISE(ABORT, 'reflex_matches records are immutable'); END;
CREATE TRIGGER reflex_matches_immutable_delete BEFORE DELETE ON reflex_matches
BEGIN SELECT RAISE(ABORT, 'reflex_matches records are immutable'); END;
CREATE TRIGGER recovery_versions_immutable_update BEFORE UPDATE ON recovery_versions
BEGIN SELECT RAISE(ABORT, 'recovery_versions records are immutable'); END;
CREATE TRIGGER recovery_versions_immutable_delete BEFORE DELETE ON recovery_versions
BEGIN SELECT RAISE(ABORT, 'recovery_versions records are immutable'); END;
CREATE TRIGGER recovery_steps_immutable_update BEFORE UPDATE ON recovery_steps
BEGIN SELECT RAISE(ABORT, 'recovery_steps records are immutable'); END;
CREATE TRIGGER recovery_steps_immutable_delete BEFORE DELETE ON recovery_steps
BEGIN SELECT RAISE(ABORT, 'recovery_steps records are immutable'); END;
CREATE TRIGGER recovery_evidence_immutable_update BEFORE UPDATE ON recovery_evidence
BEGIN SELECT RAISE(ABORT, 'recovery_evidence records are immutable'); END;
CREATE TRIGGER recovery_evidence_immutable_delete BEFORE DELETE ON recovery_evidence
BEGIN SELECT RAISE(ABORT, 'recovery_evidence records are immutable'); END;
CREATE TRIGGER recovery_attempts_immutable_update BEFORE UPDATE ON recovery_attempts
BEGIN SELECT RAISE(ABORT, 'recovery_attempts records are immutable'); END;
CREATE TRIGGER recovery_attempts_immutable_delete BEFORE DELETE ON recovery_attempts
BEGIN SELECT RAISE(ABORT, 'recovery_attempts records are immutable'); END;
CREATE TRIGGER chaos_campaigns_terminal_update BEFORE UPDATE ON chaos_campaigns WHEN OLD.status != 'running'
BEGIN SELECT RAISE(ABORT, 'terminal records are immutable'); END;
CREATE TRIGGER chaos_campaigns_immutable_delete BEFORE DELETE ON chaos_campaigns
BEGIN SELECT RAISE(ABORT, 'evidence cannot be deleted'); END;
CREATE TRIGGER resilience_tests_terminal_update BEFORE UPDATE ON resilience_tests WHEN OLD.status != 'running'
BEGIN SELECT RAISE(ABORT, 'terminal records are immutable'); END;
CREATE TRIGGER resilience_tests_immutable_delete BEFORE DELETE ON resilience_tests
BEGIN SELECT RAISE(ABORT, 'evidence cannot be deleted'); END;
