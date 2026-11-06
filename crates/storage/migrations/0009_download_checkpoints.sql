CREATE TABLE download_checkpoints (
    channel_id INTEGER NOT NULL,
    message_id INTEGER NOT NULL,
    document_id INTEGER NOT NULL,
    local_path TEXT NOT NULL,
    part_path TEXT NOT NULL,
    file_name TEXT NOT NULL,
    total_bytes INTEGER CHECK (total_bytes >= 0),
    downloaded_bytes INTEGER NOT NULL CHECK (downloaded_bytes >= 0),
    sha256 BLOB NOT NULL CHECK (length(sha256) = 32),
    finished INTEGER NOT NULL CHECK (finished IN (0, 1)),
    PRIMARY KEY (channel_id, message_id, document_id, local_path),
    FOREIGN KEY (channel_id, message_id, document_id, local_path)
        REFERENCES downloads (channel_id, message_id, document_id, local_path),
    CHECK (total_bytes IS NULL OR downloaded_bytes <= total_bytes)
);
