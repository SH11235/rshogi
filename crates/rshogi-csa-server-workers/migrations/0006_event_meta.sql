CREATE TABLE event_meta (
    event TEXT PRIMARY KEY NOT NULL,
    title TEXT NOT NULL,
    organizer TEXT,
    official_url TEXT,
    kifu_source_url TEXT,
    terms_note TEXT,
    updated_at_ms INTEGER NOT NULL
);
