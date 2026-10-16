# GLANCE integration (planned, V1C-103)

GLANCE is the Quickshell/QML dashboard in
[v1cferr/dotfiles](https://github.com/v1cferr/dotfiles). It is SignalSift's
first client, not a dependency: SignalSift runs and is fully usable without it.
**Nothing in the dotfiles is changed in this phase**; this note records the
design so the QML work can start from a stable contract.

## How GLANCE gets data today (surveyed 2026-10-15, read-only)

- QML never makes HTTP requests. `Feed.qml` runs `glance-feed read <name>` and
  parses its stdout as JSON.
- `glance-feed` is a bash script (curl, jq, sqlite3) run by a systemd user
  timer every minute; each source keeps its own interval. It caches one JSON
  document per source in `~/.cache/glance/glance.db` (WAL), uses conditional
  GET with ETags, keeps the last good document on failure (with an `error`
  field) and touches a stamp file only when content changes, which is what
  wakes the QML side.
- Pages rotate every 15 s in the right column of `Dashboard.qml`; hovering
  pauses the rotation; links open with `Qt.openUrlExternally`.
- There is no article feed or DISCOVER page yet, and the design notes keep
  scrolling feeds out of the eyeline: a small, static set of highlights fits.

## Integration design

```
signalsift (daemon) ── GET /v1/feed ──► glance-feed source "signalsift"
                                            │  curl + ETag, every 5–10 min
                                            ▼
                                   ~/.cache/glance/glance.db  (last good copy)
                                            │  glance-feed read signalsift
                                            ▼
                                   QML page "DISCOVER" (3–5 cards)
```

- **Offline behaviour is inherited**: when SignalSift is down, glance-feed
  keeps serving the last document and sets `error`; the page shows the cached
  highlights with a stale marker. No cache logic is needed in QML.
- **No inference in QML or in glance-feed**: they only display what
  SignalSift already computed.
- **Feedback** (`POST /v1/feedback`) is how the ranking will learn the
  reader's taste over time (ADR 0009): a like/dislike on each card, ideally
  on the `moment` segment rather than only the item. It needs the bearer token. QML would launch
  `glance-feed feedback <item> <verdict>` (a process, as GLANCE already does
  for other actions), and the script reads the token from
  `$XDG_STATE_HOME/signalsift/api-token`. The token never appears in QML.

## Contract the page needs from `GET /v1/feed`

One compact document, small enough to store as a single row:

```json
{
  "generated_at": "2026-10-15T23:50:00Z",
  "highlights": [
    {
      "item_id": 2,
      "title": "FABIO AKITA - Flow #663",
      "source": "Flow Podcast",
      "kind": "video",
      "topic": "Local LLM inference",
      "summary": "One or two sentences.",
      "why": "Matches interests: local inference, open models.",
      "relevance": 87,
      "url": "https://www.youtube.com/watch?v=N3eU-2VIByQ",
      "moment": { "start": "1:12:05", "link": "https://www.youtube.com/watch?v=N3eU-2VIByQ&t=4325s" },
      "processing": { "status": "complete", "partial": false, "coverage": 1.0 }
    }
  ]
}
```

Rules: `limit` defaults to 5; each highlight points (`moment`) at the segment
that earned its score, so a relevant five minutes inside a long, mostly
irrelevant video still appears and opens at the right second;
`processing.partial` must be shown, since a partial transcript says nothing
about the uncovered part. **Implemented** since V1C-101: the exact schema is
`Feed` in `contracts/openapi.yaml` (it adds `topics`, `published_at` and
`moment.summary` to the sketch above). Relevance is a preference estimate:
rank by it, do not present it as accuracy.

## Visual fit (for the QML phase)

Reuse `Theme.qml` colors and the JetBrainsMono Nerd Font; follow the "one main
line, at most one secondary line, the rest in a tooltip" rule from
`docs/notes/desktop/dash.md`: title and source on the card, topic and `why` in
the tooltip, click opens `moment.link` (or `url`).
