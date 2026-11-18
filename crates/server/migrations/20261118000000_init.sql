-- Projects: the files themselves live in <data dir>/projects/<id>/.
CREATE TABLE projects (
    id          TEXT PRIMARY KEY NOT NULL,
    name        TEXT NOT NULL,
    main_file   TEXT NOT NULL,
    engine      TEXT NOT NULL DEFAULT 'auto',
    created_at  INTEGER NOT NULL,  -- Unix time in milliseconds
    updated_at  INTEGER NOT NULL
);

-- Finished compilations, newest few per project; outputs live in <data dir>/runs/<project>/<run>/.
CREATE TABLE compile_runs (
    id          TEXT PRIMARY KEY NOT NULL,
    project_id  TEXT NOT NULL REFERENCES projects (id) ON DELETE CASCADE,
    result_json TEXT NOT NULL,
    created_at  INTEGER NOT NULL
);

CREATE INDEX compile_runs_by_project ON compile_runs (project_id, created_at DESC);
