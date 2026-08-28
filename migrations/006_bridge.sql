-- Bridge telemetry is separate from immutable empirical Experiences.
CREATE TABLE bridge_sessions (
    id TEXT PRIMARY KEY NOT NULL,
    revision INTEGER NOT NULL,
    data TEXT NOT NULL CHECK(json_valid(data))
);
CREATE TABLE bridge_events (
    sequence INTEGER PRIMARY KEY AUTOINCREMENT,
    created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
    session_id TEXT NOT NULL,
    kind TEXT NOT NULL,
    data TEXT NOT NULL CHECK(json_valid(data))
);
CREATE TABLE bridge_runs (
    session_id TEXT NOT NULL,
    run_id TEXT NOT NULL,
    experience_id TEXT UNIQUE REFERENCES experiences(id),
    data TEXT NOT NULL CHECK(json_valid(data)),
    PRIMARY KEY(session_id,run_id)
);
CREATE TABLE lesson_agent_feedback (
    lesson_id TEXT NOT NULL REFERENCES lessons(id),
    session_id TEXT NOT NULL,
    agent TEXT NOT NULL,
    reason TEXT NOT NULL,
    PRIMARY KEY(lesson_id,session_id)
);
CREATE TABLE lesson_review_flags (
    lesson_id TEXT PRIMARY KEY REFERENCES lessons(id),
    needs_revalidation INTEGER NOT NULL,
    reason TEXT NOT NULL
);
