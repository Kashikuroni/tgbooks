CREATE TABLE monitor_cursors (
    account_id INTEGER NOT NULL,
    channel_id INTEGER NOT NULL,
    after_id INTEGER NOT NULL CHECK (after_id >= 0),
    before_id INTEGER,
    target_id INTEGER,
    CHECK ((before_id IS NULL AND target_id IS NULL) OR
           (before_id IS NOT NULL AND target_id IS NOT NULL AND before_id > after_id AND target_id >= before_id)),
    PRIMARY KEY (account_id, channel_id),
    FOREIGN KEY (account_id, channel_id) REFERENCES channel_sources (account_id, channel_id) ON DELETE CASCADE
);
CREATE TABLE recommendations (
    account_id INTEGER NOT NULL CHECK (account_id > 0),
    channel_id INTEGER NOT NULL,
    message_id INTEGER NOT NULL CHECK (message_id > 0),
    document_id INTEGER NOT NULL,
    file_name TEXT NOT NULL,
    format TEXT NOT NULL CHECK (format IN ('pdf', 'epub', 'fb2', 'mobi', 'djvu', 'zip')),
    total_bytes INTEGER CHECK (total_bytes IS NULL OR total_bytes >= 0),
    source_title TEXT NOT NULL,
    message_date INTEGER NOT NULL,
    status TEXT NOT NULL CHECK (status IN ('unread', 'read', 'hidden')),
    PRIMARY KEY (account_id, channel_id, message_id, document_id)
);
