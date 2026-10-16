-- 0002: details produced by the LLM analysis (V1C-101).
--
-- matched_interests: profile topic names the segment matched.
-- mentions: tools, projects, documents explicitly named (input for the
--   cited-references work in V1C-105).
-- profile_fingerprint: which interest profile produced the score, so scores
--   from an older profile can be identified and recomputed.
-- best_segment_id: for item-level rows, the segment whose score the item took
--   (item relevance is the maximum over segments).
-- stats_json: tokens and timings of the call(s), for observability.

ALTER TABLE analyses ADD COLUMN matched_interests_json TEXT NOT NULL DEFAULT '[]'
    CHECK (json_valid(matched_interests_json));
ALTER TABLE analyses ADD COLUMN mentions_json TEXT NOT NULL DEFAULT '[]'
    CHECK (json_valid(mentions_json));
ALTER TABLE analyses ADD COLUMN profile_fingerprint TEXT NOT NULL DEFAULT '';
ALTER TABLE analyses ADD COLUMN best_segment_id INTEGER REFERENCES segments(id) ON DELETE SET NULL;
ALTER TABLE analyses ADD COLUMN stats_json TEXT NOT NULL DEFAULT '{}'
    CHECK (json_valid(stats_json));
