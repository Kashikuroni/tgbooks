ALTER TABLE app_settings ADD COLUMN system_notifications INTEGER NOT NULL DEFAULT 0
    CHECK (system_notifications IN (0, 1));
CREATE TABLE monitor_preferences (
    account_id INTEGER PRIMARY KEY CHECK (account_id > 0),
    enabled INTEGER NOT NULL CHECK (enabled IN (0, 1))
);
