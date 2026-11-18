-- LLM providers. `api_key` is write-only through the API and never returned.
CREATE TABLE providers (
    id          TEXT PRIMARY KEY NOT NULL,
    name        TEXT NOT NULL,
    kind        TEXT NOT NULL,
    base_url    TEXT,
    api_key     TEXT,
    created_at  INTEGER NOT NULL,
    updated_at  INTEGER NOT NULL
);

-- What was learned about a model while using it.
CREATE TABLE models (
    provider_id     TEXT NOT NULL REFERENCES providers (id) ON DELETE CASCADE,
    model           TEXT NOT NULL,
    supports_tools  INTEGER,            -- NULL: unknown, 0: no, 1: yes
    PRIMARY KEY (provider_id, model)
);

-- Which model plays which role: `agent` (chat, edits) and `fast` (quick actions).
CREATE TABLE roles (
    role        TEXT PRIMARY KEY NOT NULL,
    provider_id TEXT NOT NULL REFERENCES providers (id) ON DELETE CASCADE,
    model       TEXT NOT NULL
);

CREATE TABLE ai_threads (
    id             TEXT PRIMARY KEY NOT NULL,
    project_id     TEXT NOT NULL REFERENCES projects (id) ON DELETE CASCADE,
    title          TEXT NOT NULL,
    provider_id    TEXT,
    model          TEXT,
    edit_mode      TEXT,                -- mode of the turn awaiting decisions
    pending_json   TEXT,                -- tool results waiting for the user's decisions
    input_tokens   INTEGER NOT NULL DEFAULT 0,
    output_tokens  INTEGER NOT NULL DEFAULT 0,
    created_at     INTEGER NOT NULL,
    updated_at     INTEGER NOT NULL
);
CREATE INDEX ai_threads_by_project ON ai_threads (project_id, updated_at DESC);

CREATE TABLE ai_messages (
    id            INTEGER PRIMARY KEY AUTOINCREMENT,
    thread_id     TEXT NOT NULL REFERENCES ai_threads (id) ON DELETE CASCADE,
    message_json  TEXT NOT NULL,
    created_at    INTEGER NOT NULL
);
CREATE INDEX ai_messages_by_thread ON ai_messages (thread_id, id);

CREATE TABLE ai_patches (
    id          TEXT PRIMARY KEY NOT NULL,
    thread_id   TEXT NOT NULL REFERENCES ai_threads (id) ON DELETE CASCADE,
    patch_json  TEXT NOT NULL,
    status      TEXT NOT NULL,
    created_at  INTEGER NOT NULL,
    decided_at  INTEGER
);
CREATE INDEX ai_patches_by_thread ON ai_patches (thread_id, created_at);

-- One row per model call, for token totals.
CREATE TABLE usage (
    id             INTEGER PRIMARY KEY AUTOINCREMENT,
    thread_id      TEXT REFERENCES ai_threads (id) ON DELETE SET NULL,
    provider_id    TEXT,
    model          TEXT NOT NULL,
    input_tokens   INTEGER NOT NULL,
    output_tokens  INTEGER NOT NULL,
    at             INTEGER NOT NULL
);
