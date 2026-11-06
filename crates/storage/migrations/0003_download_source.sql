-- Original names use file_name; safe disk names remain in local_path.
-- Legacy records keep their existing name and a NULL source.
ALTER TABLE downloads ADD COLUMN source_title TEXT;
