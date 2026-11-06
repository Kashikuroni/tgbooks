CREATE TABLE app_settings (
    singleton INTEGER PRIMARY KEY CHECK (singleton = 1),
    download_directory TEXT NOT NULL,
    formats INTEGER NOT NULL CHECK (formats BETWEEN 1 AND 63)
);
