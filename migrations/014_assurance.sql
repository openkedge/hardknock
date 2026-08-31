CREATE TABLE behavioral_contract_revisions (
  contract_id TEXT NOT NULL,
  revision INTEGER NOT NULL CHECK(revision > 0),
  name TEXT NOT NULL,
  subject_kind TEXT NOT NULL,
  subject_id TEXT NOT NULL,
  created_at TEXT NOT NULL,
  data TEXT NOT NULL CHECK(json_valid(data)),
  PRIMARY KEY(contract_id, revision)
);
CREATE INDEX behavioral_contract_name_revision
  ON behavioral_contract_revisions(name, revision);
CREATE INDEX behavioral_contract_subject
  ON behavioral_contract_revisions(subject_kind, subject_id, revision);

CREATE TABLE skill_contract_bindings (
  skill_id TEXT NOT NULL REFERENCES skills(id),
  binding_revision INTEGER NOT NULL CHECK(binding_revision > 0),
  contract_id TEXT NOT NULL,
  contract_revision INTEGER NOT NULL,
  created_at TEXT NOT NULL,
  data TEXT NOT NULL CHECK(json_valid(data)),
  PRIMARY KEY(skill_id, binding_revision),
  FOREIGN KEY(contract_id, contract_revision)
    REFERENCES behavioral_contract_revisions(contract_id, revision)
);

CREATE TABLE evidence_manifests (
  id TEXT PRIMARY KEY,
  subject_kind TEXT NOT NULL,
  subject_id TEXT NOT NULL,
  subject_revision INTEGER,
  evidence_hash TEXT NOT NULL,
  generated_at TEXT NOT NULL,
  data TEXT NOT NULL CHECK(json_valid(data))
);
CREATE INDEX evidence_manifest_subject
  ON evidence_manifests(subject_kind, subject_id, subject_revision, generated_at, id);

CREATE TABLE skill_certifications (
  id TEXT PRIMARY KEY,
  skill_id TEXT NOT NULL REFERENCES skills(id),
  skill_revision INTEGER NOT NULL,
  contract_id TEXT NOT NULL,
  contract_revision INTEGER NOT NULL,
  profile_id TEXT NOT NULL,
  profile_version TEXT NOT NULL,
  evidence_manifest_id TEXT NOT NULL REFERENCES evidence_manifests(id),
  issued_at TEXT NOT NULL,
  expires_at TEXT,
  status TEXT NOT NULL,
  data TEXT NOT NULL CHECK(json_valid(data)),
  FOREIGN KEY(skill_id, skill_revision) REFERENCES skill_revisions(skill_id, revision),
  FOREIGN KEY(contract_id, contract_revision)
    REFERENCES behavioral_contract_revisions(contract_id, revision)
);
CREATE INDEX skill_certification_history
  ON skill_certifications(skill_id, issued_at, id);

CREATE TABLE certification_revocations (
  certification_id TEXT PRIMARY KEY REFERENCES skill_certifications(id),
  revoked_at TEXT NOT NULL,
  reason TEXT NOT NULL,
  data TEXT NOT NULL CHECK(json_valid(data))
);

CREATE TABLE external_certification_artifacts (
  artifact_hash TEXT PRIMARY KEY,
  producer TEXT,
  received_at TEXT NOT NULL,
  authentic INTEGER NOT NULL,
  local_certification_established INTEGER NOT NULL DEFAULT 0,
  data TEXT NOT NULL CHECK(json_valid(data))
);

CREATE TRIGGER behavioral_contract_revision_no_update BEFORE UPDATE ON behavioral_contract_revisions
BEGIN SELECT RAISE(ABORT, 'Behavioral Contract revisions are immutable'); END;
CREATE TRIGGER behavioral_contract_revision_no_delete BEFORE DELETE ON behavioral_contract_revisions
BEGIN SELECT RAISE(ABORT, 'Behavioral Contract revisions are retained'); END;
CREATE TRIGGER skill_contract_binding_no_update BEFORE UPDATE ON skill_contract_bindings
BEGIN SELECT RAISE(ABORT, 'Skill contract bindings are immutable'); END;
CREATE TRIGGER skill_contract_binding_no_delete BEFORE DELETE ON skill_contract_bindings
BEGIN SELECT RAISE(ABORT, 'Skill contract binding history is retained'); END;
CREATE TRIGGER evidence_manifest_no_update BEFORE UPDATE ON evidence_manifests
BEGIN SELECT RAISE(ABORT, 'Evidence Manifests are immutable'); END;
CREATE TRIGGER evidence_manifest_no_delete BEFORE DELETE ON evidence_manifests
BEGIN SELECT RAISE(ABORT, 'Evidence Manifests are retained'); END;
CREATE TRIGGER skill_certification_no_update BEFORE UPDATE ON skill_certifications
BEGIN SELECT RAISE(ABORT, 'Skill certifications are immutable'); END;
CREATE TRIGGER skill_certification_no_delete BEFORE DELETE ON skill_certifications
BEGIN SELECT RAISE(ABORT, 'Skill certification history is retained'); END;
CREATE TRIGGER certification_revocation_no_update BEFORE UPDATE ON certification_revocations
BEGIN SELECT RAISE(ABORT, 'Certification revocations are immutable'); END;
CREATE TRIGGER certification_revocation_no_delete BEFORE DELETE ON certification_revocations
BEGIN SELECT RAISE(ABORT, 'Certification revocations are retained'); END;
CREATE TRIGGER external_certification_no_update BEFORE UPDATE ON external_certification_artifacts
BEGIN SELECT RAISE(ABORT, 'External certification observations are immutable'); END;
CREATE TRIGGER external_certification_no_delete BEFORE DELETE ON external_certification_artifacts
BEGIN SELECT RAISE(ABORT, 'External certification observations are retained'); END;
