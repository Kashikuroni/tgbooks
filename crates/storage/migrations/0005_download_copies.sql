-- Keep previous successful copies when explicitly downloading to another folder.
CREATE TABLE download_copies (
    channel_id INTEGER NOT NULL,
    message_id INTEGER NOT NULL,
    document_id INTEGER NOT NULL,
    file_name TEXT NOT NULL,
    local_path TEXT NOT NULL,
    status TEXT NOT NULL CHECK (status IN ('queued', 'downloading', 'completed', 'failed')),
    downloaded_bytes INTEGER NOT NULL CHECK (downloaded_bytes >= 0),
    total_bytes INTEGER CHECK (total_bytes IS NULL OR total_bytes >= downloaded_bytes),
    created_at INTEGER NOT NULL DEFAULT (unixepoch()),
    source_title TEXT,
    PRIMARY KEY (channel_id, message_id, document_id, local_path)
);
INSERT INTO download_copies SELECT channel_id, message_id, document_id, file_name,
    local_path, status, downloaded_bytes, total_bytes, created_at, source_title FROM downloads;
DROP TABLE downloads;
ALTER TABLE download_copies RENAME TO downloads;

ALTER TABLE download_queue ADD COLUMN redownload INTEGER NOT NULL DEFAULT 0
    CHECK (redownload IN (0, 1));
