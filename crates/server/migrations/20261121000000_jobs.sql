-- Background jobs: long work in steps (for example one PDF page per step) that survives a
-- restart. A step is either pending or done; "running" lives only in memory, so after a restart
-- every step that is not done runs again. Step results are files in <data dir>/jobs/<id>/.
CREATE TABLE jobs (
    id          TEXT PRIMARY KEY,   -- uuid v7, so ids sort by creation time
    kind        TEXT NOT NULL,      -- for example 'pdfImport'
    title       TEXT NOT NULL,
    state       TEXT NOT NULL,      -- queued, running, succeeded, failed, cancelled
    params      TEXT NOT NULL,      -- JSON, for the job's kind
    steps_total INTEGER,            -- NULL until the job has planned its steps
    steps_done  INTEGER NOT NULL DEFAULT 0,
    message     TEXT,               -- what it is doing, for display
    error       TEXT,               -- why it failed
    project_id  TEXT,               -- the project it made; no foreign key, it may be deleted later
    created_at  INTEGER NOT NULL,   -- Unix time in milliseconds
    updated_at  INTEGER NOT NULL,
    finished_at INTEGER
);

CREATE INDEX jobs_by_state ON jobs (state, created_at);

CREATE TABLE job_steps (
    job_id   TEXT NOT NULL REFERENCES jobs (id) ON DELETE CASCADE,
    idx      INTEGER NOT NULL,
    state    TEXT NOT NULL,         -- pending, done
    attempts INTEGER NOT NULL DEFAULT 0,
    PRIMARY KEY (job_id, idx)
);
