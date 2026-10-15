-- 0001: initial schema.
--
-- Conventions:
--   * Every table is STRICT, so a TEXT written into an INTEGER column is an
--     error instead of a silently stored string.
--   * Instants are INTEGER milliseconds since the Unix epoch (UTC). They sort
--     correctly, cost 8 bytes, and `datetime(x / 1000, 'unixepoch')` makes them
--     readable in the sqlite3 shell.
--   * Media positions are INTEGER milliseconds from the start of the media.
--   * Nothing is deleted for being irrelevant: low-scoring items stay, because
--     relevance is an estimate and a later model or profile may disagree.

CREATE TABLE creators (
    id          INTEGER PRIMARY KEY,
    name        TEXT NOT NULL UNIQUE,
    created_at  INTEGER NOT NULL
) STRICT;

-- One creator, many sources: a blog feed, a YouTube channel and a podcast feed
-- of the same person all point to the same creator row.
CREATE TABLE sources (
    id                    INTEGER PRIMARY KEY,
    creator_id            INTEGER REFERENCES creators(id) ON DELETE SET NULL,
    kind                  TEXT NOT NULL CHECK (kind IN ('feed', 'youtube_channel', 'podcast', 'newsletter')),
    url                   TEXT NOT NULL UNIQUE,  -- canonical form
    title                 TEXT,
    enabled               INTEGER NOT NULL DEFAULT 1 CHECK (enabled IN (0, 1)),
    poll_interval_secs    INTEGER NOT NULL DEFAULT 3600 CHECK (poll_interval_secs >= 300),
    next_poll_at          INTEGER NOT NULL DEFAULT 0,
    last_success_at       INTEGER,
    last_error            TEXT,
    consecutive_failures  INTEGER NOT NULL DEFAULT 0,
    created_at            INTEGER NOT NULL
) STRICT;

CREATE INDEX sources_due ON sources (next_poll_at) WHERE enabled = 1;

CREATE TABLE items (
    id             INTEGER PRIMARY KEY,
    source_id      INTEGER REFERENCES sources(id) ON DELETE SET NULL,
    kind           TEXT NOT NULL CHECK (kind IN ('article', 'video', 'podcast_episode')),
    canonical_url  TEXT NOT NULL UNIQUE,
    -- Platform id when there is one (YouTube video id, feed entry guid). The
    -- same video reached through two URLs must still be one item.
    external_id    TEXT,
    title          TEXT NOT NULL,
    author         TEXT,
    published_at   INTEGER,
    discovered_at  INTEGER NOT NULL,
    duration_ms    INTEGER,
    -- Plain text extracted from an article or a video description. Raw HTML is
    -- never stored: it is untrusted and much larger.
    content_text   TEXT,
    metadata_json  TEXT NOT NULL DEFAULT '{}' CHECK (json_valid(metadata_json))
) STRICT;

CREATE UNIQUE INDEX items_external ON items (kind, external_id) WHERE external_id IS NOT NULL;
CREATE INDEX items_published ON items (published_at DESC);

-- A transcript is one text track of one item, from one origin. Coverage is
-- explicit: a transcript whose covered_ms is below the item's duration is
-- partial and is reported as such, never presented as complete.
CREATE TABLE transcripts (
    id           INTEGER PRIMARY KEY,
    item_id      INTEGER NOT NULL REFERENCES items(id) ON DELETE CASCADE,
    origin       TEXT NOT NULL CHECK (origin IN ('manual_subtitles', 'auto_subtitles', 'asr')),
    language     TEXT NOT NULL,
    engine       TEXT NOT NULL,     -- e.g. "yt-dlp 2026.08.19" or "whisper.cpp 1.9.2 ggml-base"
    status       TEXT NOT NULL CHECK (status IN ('in_progress', 'partial', 'complete', 'failed')),
    covered_ms   INTEGER NOT NULL DEFAULT 0,
    duration_ms  INTEGER,
    created_at   INTEGER NOT NULL,
    updated_at   INTEGER NOT NULL,
    UNIQUE (item_id, origin, language)
) STRICT;

-- Normalized cues: overlapping rolling captions merged, boundaries deduplicated.
CREATE TABLE transcript_cues (
    transcript_id  INTEGER NOT NULL REFERENCES transcripts(id) ON DELETE CASCADE,
    seq            INTEGER NOT NULL,
    start_ms       INTEGER NOT NULL,
    end_ms         INTEGER NOT NULL CHECK (end_ms >= start_ms),
    text           TEXT NOT NULL,
    PRIMARY KEY (transcript_id, seq)
) STRICT, WITHOUT ROWID;

-- Topical windows over a transcript (or sections of an article). These are the
-- units the relevance model scores individually, so five relevant minutes in a
-- two-hour video survive even when the item as a whole scores low.
CREATE TABLE segments (
    id             INTEGER PRIMARY KEY,
    item_id        INTEGER NOT NULL REFERENCES items(id) ON DELETE CASCADE,
    transcript_id  INTEGER REFERENCES transcripts(id) ON DELETE CASCADE,
    seq            INTEGER NOT NULL,
    start_ms       INTEGER,
    end_ms         INTEGER,
    text           TEXT NOT NULL,
    UNIQUE (item_id, transcript_id, seq)
) STRICT;

CREATE TABLE analyses (
    id                 INTEGER PRIMARY KEY,
    item_id            INTEGER NOT NULL REFERENCES items(id) ON DELETE CASCADE,
    segment_id         INTEGER REFERENCES segments(id) ON DELETE CASCADE,  -- NULL: whole item
    model              TEXT NOT NULL,
    prompt_version     TEXT NOT NULL,
    summary            TEXT NOT NULL,
    topics_json        TEXT NOT NULL DEFAULT '[]' CHECK (json_valid(topics_json)),
    relevance          INTEGER NOT NULL CHECK (relevance BETWEEN 0 AND 100),
    rationale          TEXT NOT NULL,
    applications_json  TEXT NOT NULL DEFAULT '[]' CHECK (json_valid(applications_json)),
    created_at         INTEGER NOT NULL
) STRICT;

-- NULLs are distinct in UNIQUE constraints, so item-level analyses need the
-- IFNULL to stay idempotent across re-runs.
CREATE UNIQUE INDEX analyses_once ON analyses (item_id, IFNULL(segment_id, 0), model, prompt_version);
CREATE INDEX analyses_relevance ON analyses (relevance DESC);

-- URLs explicitly present in a description or transcript. `item_references`
-- because REFERENCES is an SQL keyword.
CREATE TABLE item_references (
    id          INTEGER PRIMARY KEY,
    item_id     INTEGER NOT NULL REFERENCES items(id) ON DELETE CASCADE,
    segment_id  INTEGER REFERENCES segments(id) ON DELETE SET NULL,
    url         TEXT NOT NULL,
    label       TEXT,
    at_ms       INTEGER,
    origin      TEXT NOT NULL CHECK (origin IN ('description', 'transcript', 'article')),
    UNIQUE (item_id, url)
) STRICT;

-- Persistent job queue. dedup_key makes enqueueing idempotent: the same work
-- requested twice is one row.
CREATE TABLE jobs (
    id               INTEGER PRIMARY KEY,
    kind             TEXT NOT NULL,
    dedup_key        TEXT NOT NULL UNIQUE,
    work_class       TEXT NOT NULL CHECK (work_class IN ('light', 'heavy')),
    state            TEXT NOT NULL CHECK (state IN ('queued', 'running', 'succeeded', 'failed', 'cancelled')),
    payload_json     TEXT NOT NULL DEFAULT '{}' CHECK (json_valid(payload_json)),
    checkpoint_json  TEXT CHECK (checkpoint_json IS NULL OR json_valid(checkpoint_json)),
    attempts         INTEGER NOT NULL DEFAULT 0,
    max_attempts     INTEGER NOT NULL DEFAULT 5,
    run_after        INTEGER NOT NULL DEFAULT 0,
    last_error       TEXT,
    created_at       INTEGER NOT NULL,
    updated_at       INTEGER NOT NULL
) STRICT;

CREATE INDEX jobs_ready ON jobs (work_class, run_after) WHERE state = 'queued';

CREATE TABLE feedback (
    id          INTEGER PRIMARY KEY,
    item_id     INTEGER NOT NULL REFERENCES items(id) ON DELETE CASCADE,
    segment_id  INTEGER REFERENCES segments(id) ON DELETE SET NULL,
    verdict     TEXT NOT NULL CHECK (verdict IN ('interested', 'not_interested', 'already_known', 'wrong_topic')),
    note        TEXT,
    client      TEXT,
    created_at  INTEGER NOT NULL
) STRICT;

-- Conditional GET state per URL.
CREATE TABLE http_cache (
    url            TEXT PRIMARY KEY,
    etag           TEXT,
    last_modified  TEXT,
    status         INTEGER NOT NULL,
    content_hash   TEXT,
    fetched_at     INTEGER NOT NULL
) STRICT;
