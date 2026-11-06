CREATE TABLE channel_sources (
    account_id INTEGER NOT NULL CHECK (account_id > 0),
    channel_id INTEGER NOT NULL CHECK (channel_id != 0),
    title TEXT NOT NULL,
    username TEXT,
    protected INTEGER NOT NULL CHECK (protected IN (0, 1)),
    PRIMARY KEY (account_id, channel_id)
);
