CREATE TABLE IF NOT EXISTS blob (
    hash       TEXT    NOT NULL PRIMARY KEY
                       CHECK (length(hash) > 0 AND hash NOT GLOB '*[^0-9a-f]*'),
    format     TEXT    NOT NULL,
    size_bytes INTEGER NOT NULL CHECK (size_bytes >= 0),
    added_at   INTEGER NOT NULL
) STRICT, WITHOUT ROWID;
