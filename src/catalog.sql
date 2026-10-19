PRAGMA journal_mode = WAL;
PRAGMA synchronous = FULL;
PRAGMA foreign_keys = ON;
CREATE TABLE IF NOT EXISTS root_info (key TEXT PRIMARY KEY, value BLOB NOT NULL);
CREATE TABLE IF NOT EXISTS dirs (
    dir_id INTEGER PRIMARY KEY,
    parent_id INTEGER REFERENCES dirs(dir_id),
    name BLOB NOT NULL,
    path BLOB NOT NULL UNIQUE,
    UNIQUE(parent_id, name)
);
CREATE TABLE IF NOT EXISTS files (
    dir_id INTEGER NOT NULL REFERENCES dirs(dir_id),
    name BLOB NOT NULL,
    file_id INTEGER NOT NULL UNIQUE,
    size INTEGER NOT NULL,
    mtime_ns INTEGER NOT NULL,
    ctime_ns INTEGER NOT NULL,
    ino BLOB,
    content_status INTEGER NOT NULL CHECK(content_status BETWEEN 0 AND 3),
    PRIMARY KEY(dir_id, name)
) WITHOUT ROWID;
INSERT OR IGNORE INTO dirs(dir_id, parent_id, name, path) VALUES(1, NULL, X'', X'');
PRAGMA user_version = 1;
