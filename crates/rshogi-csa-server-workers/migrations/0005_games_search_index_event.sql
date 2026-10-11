ALTER TABLE games_search_index ADD COLUMN event TEXT;
ALTER TABLE games_search_index ADD COLUMN origin_url TEXT;

CREATE INDEX games_search_index_event_source
    ON games_search_index (event, source) WHERE event IS NOT NULL;
