-- Version history: the content lives in <data dir>/history/<project>.git, this is the metadata.
CREATE TABLE snapshots (
    project_id  TEXT NOT NULL REFERENCES projects (id) ON DELETE CASCADE,
    rev         TEXT NOT NULL,  -- commit id, 40 hex characters
    kind        TEXT NOT NULL,  -- created, beforeAiEdit, aiEdit, compiled, manual, beforeRestore, restored
    label       TEXT,
    summary     TEXT NOT NULL,
    added       INTEGER NOT NULL,
    modified    INTEGER NOT NULL,
    deleted     INTEGER NOT NULL,
    created_at  INTEGER NOT NULL,  -- Unix time in milliseconds
    PRIMARY KEY (project_id, rev)
);

CREATE INDEX snapshots_by_project ON snapshots (project_id, created_at DESC);
