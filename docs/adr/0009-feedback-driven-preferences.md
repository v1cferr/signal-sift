# 0009 — Preferences that adapt from like/dislike feedback

**Status:** Proposed (2026-10-16). Not implemented yet. Tracked in V1C-106.

## Context

The owner's main wish for the product: as recommendations reach them
(GLANCE, CLI, API), they like or dislike them, and the ranking adapts over
time without hand-editing `interests.toml`.

Already in place:
- `POST /v1/feedback` stores verdicts (`interested`, `not_interested`,
  `already_known`, `wrong_topic`) per item **and per segment**.
- `/v1/feed` hides items whose latest verdict is `not_interested` or
  `already_known`; nothing is deleted.
- Every analysis stores its topics, matched interests and the
  interest-profile fingerprint.

None of this changes scores yet.

## Proposed decision

In order, each step measured before the next:

1. **Per-topic weights in feed ranking.** Feedback adjusts a weight per topic
   or matched interest, and the feed ranks by the model's score combined with
   that weight. This needs no LLM call, is explainable ("ranked higher: you
   liked 4 items on local inference") and can be reverted by clearing the
   weights.
2. **Liked and disliked examples in the scoring prompt.** A few recent
   examples join the interest profile so the model picks up nuance that topic
   weights miss. They change the profile fingerprint, so older scores can be
   identified and recomputed.
3. **Votes become the owner-labelled evaluation set**, replacing the
   provisional labels in `tests/eval/relevance-labels.toml`, so every change
   above is judged on recall (false negatives first) instead of intuition.

Guardrails:
- an exploration slot in the feed, so dislikes do not narrow it into a
  bubble;
- gradual updates with a minimum number of votes, so one vote cannot
  dominate;
- feedback never deletes items.

## Consequences

- Stays inside SQLite. No vector database unless a measured retrieval need
  appears (ADR 0003).
- GLANCE needs like/dislike buttons that call `glance-feed feedback ...`
  (V1C-103, see docs/integration/glance.md).
- The choice between steps 1 and 2 is revisited once enough votes exist to
  measure them.
