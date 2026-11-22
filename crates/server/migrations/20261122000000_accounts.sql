-- Server mode (`--mode server`): accounts, sessions, one-time links and project members. In
-- single-user mode none of these tables is used and every owner column below stays NULL, so a
-- data dir can switch modes either way.

CREATE TABLE users (
    id            TEXT PRIMARY KEY NOT NULL,             -- uuid v7
    username      TEXT NOT NULL UNIQUE COLLATE NOCASE,
    display_name  TEXT NOT NULL,
    role          TEXT NOT NULL,                         -- admin, user
    password_hash TEXT,                                  -- argon2id (PHC string); NULL until set
    disabled      INTEGER NOT NULL DEFAULT 0,            -- accounts are locked, never deleted
    created_at    INTEGER NOT NULL,                      -- Unix time in milliseconds
    last_login_at INTEGER
);

-- Signed-in browsers. Only a hash of the cookie is stored, so the database cannot sign anyone in.
CREATE TABLE sessions (
    id_hash      TEXT PRIMARY KEY NOT NULL,              -- SHA-256 of the cookie value, hex
    user_id      TEXT NOT NULL REFERENCES users (id) ON DELETE CASCADE,
    user_agent   TEXT,
    created_at   INTEGER NOT NULL,
    last_used_at INTEGER NOT NULL,
    expires_at   INTEGER NOT NULL
);

CREATE INDEX sessions_by_user ON sessions (user_id);

-- One-time links: creating the first admin, or setting a password.
CREATE TABLE setup_links (
    token_hash TEXT PRIMARY KEY NOT NULL,                -- SHA-256 of the token in the link, hex
    kind       TEXT NOT NULL,                            -- firstAdmin, setPassword
    user_id    TEXT REFERENCES users (id) ON DELETE CASCADE,  -- NULL for firstAdmin
    created_at INTEGER NOT NULL,
    expires_at INTEGER NOT NULL,
    used_at    INTEGER
);

CREATE TABLE project_members (
    project_id TEXT NOT NULL REFERENCES projects (id) ON DELETE CASCADE,
    user_id    TEXT NOT NULL REFERENCES users (id) ON DELETE CASCADE,
    role       TEXT NOT NULL,                            -- owner, manager, member
    added_at   INTEGER NOT NULL,
    PRIMARY KEY (project_id, user_id)
);

CREATE UNIQUE INDEX project_members_one_owner ON project_members (project_id) WHERE role = 'owner';
CREATE INDEX project_members_by_user ON project_members (user_id);

-- Each user's own model per role. The `roles` table holds the defaults an admin sets (and the
-- only choice in single-user mode).
CREATE TABLE user_roles (
    user_id     TEXT NOT NULL REFERENCES users (id) ON DELETE CASCADE,
    role        TEXT NOT NULL,
    provider_id TEXT NOT NULL REFERENCES providers (id) ON DELETE CASCADE,
    model       TEXT NOT NULL,
    reasoning   TEXT,
    PRIMARY KEY (user_id, role)
);

-- Owners. NULL in single-user mode; for providers, NULL also means shared by everyone.
ALTER TABLE providers ADD COLUMN owner_user_id TEXT REFERENCES users (id) ON DELETE CASCADE;
ALTER TABLE ai_threads ADD COLUMN user_id TEXT REFERENCES users (id) ON DELETE CASCADE;
ALTER TABLE jobs ADD COLUMN user_id TEXT REFERENCES users (id) ON DELETE SET NULL;
ALTER TABLE snapshots ADD COLUMN author_id TEXT REFERENCES users (id) ON DELETE SET NULL;

CREATE INDEX ai_threads_by_user ON ai_threads (user_id, project_id);
CREATE INDEX jobs_by_user ON jobs (user_id, created_at);
