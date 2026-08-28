-- Strategy requests cannot use the V0.1 experiments/trials tables: those require
-- a prior Lesson and exactly two positions. Experiences/evaluations remain shared.
CREATE TABLE experiment_requests (
    id TEXT PRIMARY KEY NOT NULL,
    request_id TEXT NOT NULL UNIQUE,
    session_id TEXT NOT NULL,
    agent TEXT NOT NULL,
    created_at TEXT NOT NULL,
    status TEXT NOT NULL CHECK(status IN ('accepted','running','completed','cancelled','rejected','failed')),
    cancel_requested INTEGER NOT NULL DEFAULT 0 CHECK(cancel_requested IN (0,1)),
    data TEXT NOT NULL CHECK(json_valid(data))
);
CREATE INDEX experiment_requests_agent ON experiment_requests(agent, created_at);
CREATE INDEX experiment_requests_session ON experiment_requests(session_id, status);
CREATE TABLE experiment_candidates (
    id TEXT PRIMARY KEY NOT NULL,
    experiment_id TEXT NOT NULL REFERENCES experiment_requests(id),
    position INTEGER NOT NULL,
    data TEXT NOT NULL CHECK(json_valid(data)),
    result TEXT CHECK(result IS NULL OR json_valid(result)),
    experience_id TEXT UNIQUE REFERENCES experiences(id),
    reality_id TEXT REFERENCES realities(id),
    UNIQUE(experiment_id, position)
);
CREATE TABLE experiment_variables (
    experiment_id TEXT NOT NULL REFERENCES experiment_requests(id),
    name TEXT NOT NULL,
    data TEXT NOT NULL CHECK(json_valid(data)),
    PRIMARY KEY(experiment_id,name)
);
CREATE TABLE experiment_relations (
    parent TEXT NOT NULL REFERENCES experiment_requests(id),
    child TEXT NOT NULL REFERENCES experiment_requests(id),
    relation TEXT NOT NULL CHECK(relation IN ('replay','extension','revalidation','counterfactual')),
    CHECK(parent != child),
    PRIMARY KEY(parent,child,relation)
);
CREATE TABLE experiment_progress (
    sequence INTEGER PRIMARY KEY AUTOINCREMENT,
    experiment_id TEXT NOT NULL REFERENCES experiment_requests(id),
    data TEXT NOT NULL CHECK(json_valid(data))
);
CREATE INDEX experiment_progress_id ON experiment_progress(experiment_id,sequence);
CREATE TRIGGER experiment_request_immutable BEFORE UPDATE ON experiment_requests
WHEN OLD.status NOT IN ('accepted','running') OR json_extract(NEW.data,'$.request') != json_extract(OLD.data,'$.request')
 OR NEW.id != OLD.id OR NEW.request_id != OLD.request_id OR NEW.session_id != OLD.session_id OR NEW.agent != OLD.agent OR NEW.created_at != OLD.created_at
 OR json_extract(NEW.data,'$.effective_budget') != json_extract(OLD.data,'$.effective_budget')
 OR (OLD.status='running' AND NEW.status='accepted')
BEGIN SELECT RAISE(ABORT, 'requests and terminal experiments are immutable'); END;
CREATE TRIGGER experiment_request_no_delete BEFORE DELETE ON experiment_requests
BEGIN SELECT RAISE(ABORT, 'experiment evidence cannot be deleted'); END;
CREATE TRIGGER experiment_candidate_immutable BEFORE UPDATE ON experiment_candidates
WHEN OLD.result IS NOT NULL OR OLD.data != NEW.data OR OLD.experiment_id != NEW.experiment_id OR OLD.id != NEW.id OR OLD.position != NEW.position
BEGIN SELECT RAISE(ABORT, 'candidate results are immutable'); END;
CREATE TRIGGER experiment_candidate_no_delete BEFORE DELETE ON experiment_candidates
BEGIN SELECT RAISE(ABORT, 'candidate evidence cannot be deleted'); END;
CREATE TRIGGER experiment_progress_no_update BEFORE UPDATE ON experiment_progress
BEGIN SELECT RAISE(ABORT, 'progress is append only'); END;
CREATE TRIGGER experiment_progress_no_delete BEFORE DELETE ON experiment_progress
BEGIN SELECT RAISE(ABORT, 'progress is append only'); END;
CREATE TRIGGER experiment_variables_no_update BEFORE UPDATE ON experiment_variables
BEGIN SELECT RAISE(ABORT, 'variables are immutable'); END;
CREATE TRIGGER experiment_variables_no_delete BEFORE DELETE ON experiment_variables
BEGIN SELECT RAISE(ABORT, 'variables are immutable'); END;
CREATE TRIGGER experiment_relations_no_update BEFORE UPDATE ON experiment_relations
BEGIN SELECT RAISE(ABORT, 'lineage is immutable'); END;
CREATE TRIGGER experiment_relations_no_delete BEFORE DELETE ON experiment_relations
BEGIN SELECT RAISE(ABORT, 'lineage is immutable'); END;
CREATE UNIQUE INDEX experience_candidate_identity ON experiences(json_extract(data,'$.experiment.candidate_id'))
WHERE json_extract(data,'$.experiment.candidate_id') IS NOT NULL;
