CREATE TABLE downloads (
    channel_id       INTEGER NOT NULL,
    message_id       INTEGER NOT NULL,
    document_id      INTEGER NOT NULL,
    file_name        TEXT NOT NULL,
    local_path       TEXT NOT NULL,
    status           TEXT NOT NULL CHECK (status IN ('queued', 'downloading', 'completed', 'failed')),
    downloaded_bytes INTEGER NOT NULL CHECK (downloaded_bytes >= 0),
    total_bytes      INTEGER CHECK (total_bytes IS NULL OR total_bytes >= downloaded_bytes),
    created_at       INTEGER NOT NULL DEFAULT (unixepoch()),
    PRIMARY KEY (channel_id, message_id, document_id)
);
