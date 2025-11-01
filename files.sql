-- Table for tracked content-addressable files
CREATE TABLE files (
    hash          BLOB PRIMARY KEY,     -- SHA-256, stored as binary
    size          INTEGER NOT NULL,
    extension     TEXT NOT NULL,        -- e.g., "mp3", "jpg"
    mime_type     TEXT,                 -- e.g., "audio/mpeg"
    original_name TEXT,                 -- user-friendly filename
    added_at      TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP
);

-- Tags
CREATE TABLE tags (
    id   INTEGER PRIMARY KEY AUTOINCREMENT,
    name TEXT NOT NULL UNIQUE
);

-- Many-to-many relationship: files <-> tags
CREATE TABLE file_tags (
    file_hash BLOB NOT NULL,
    tag_id    INTEGER NOT NULL,
    PRIMARY KEY (file_hash, tag_id),
    FOREIGN KEY (file_hash) REFERENCES files(hash),
    FOREIGN KEY (tag_id)    REFERENCES tags(id)
);
