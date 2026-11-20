CREATE TABLE IF NOT EXISTS derived_signals (
    id              TEXT PRIMARY KEY NOT NULL,
    label           TEXT NOT NULL DEFAULT '',
    expression      TEXT NOT NULL,
    unit            TEXT NOT NULL DEFAULT '',
    updated_at_ms   BIGINT NOT NULL DEFAULT 0,
    deleted         BOOLEAN NOT NULL DEFAULT 0
);

CREATE INDEX IF NOT EXISTS idx_derived_signals_updated_at ON derived_signals(updated_at_ms);
