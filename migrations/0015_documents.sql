-- Details of documents: what HearthDrive read from their text (`found_*`)
-- and what people typed (`title`, `doc_date`, `sender`, `amount`, `note`).
-- A typed value wins over a found one; clearing it shows the found one again.
CREATE TABLE doc_meta (
    file_id      TEXT PRIMARY KEY REFERENCES files(id) ON DELETE CASCADE,
    root_id      TEXT NOT NULL REFERENCES storage_roots(id) ON DELETE CASCADE,
    found_date   TEXT,                    -- YYYY-MM-DD
    found_sender TEXT,
    found_amount TEXT,                    -- as written, with its currency
    title        TEXT,
    doc_date     TEXT,                    -- YYYY-MM-DD
    sender       TEXT,
    amount       TEXT,
    note         TEXT,
    edited_by    TEXT,
    edited_at    INTEGER
);
CREATE INDEX idx_doc_meta_root ON doc_meta (root_id);
