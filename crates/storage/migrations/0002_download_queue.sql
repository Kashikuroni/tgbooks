-- Intent stays separate from history: waiting jobs have no local file yet.
CREATE TABLE download_queue (
    account_id   INTEGER NOT NULL CHECK (account_id > 0),
    position     INTEGER NOT NULL CHECK (position >= 0),
    channel_id   INTEGER NOT NULL,
    message_id   INTEGER NOT NULL,
    document_id  INTEGER NOT NULL,
    file_name    TEXT NOT NULL,
    total_bytes  INTEGER CHECK (total_bytes IS NULL OR total_bytes >= 0),
    format       TEXT NOT NULL CHECK (format IN ('pdf', 'epub', 'fb2', 'mobi', 'djvu', 'zip')),
    directory    TEXT NOT NULL,
    interrupted INTEGER NOT NULL CHECK (interrupted IN (0, 1)),
    PRIMARY KEY (account_id, position),
    UNIQUE (account_id, channel_id, message_id, document_id)
);
