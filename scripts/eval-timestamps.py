#!/usr/bin/env python3
"""Compares ASR cue start times against a caption track of the same item.

    python3 -I scripts/eval-timestamps.py DB_PATH ITEM_ID [REFERENCE_ORIGIN]

REFERENCE_ORIGIN defaults to auto_subtitles. For each ASR cue, its first
three words are looked up in the reference within +-15 s; the offset is the
ASR start minus the interpolated reference time of that word. Reference word
times are interpolated linearly inside each caption line, so the method has
about +-1 s of error of its own: treat results as an upper bound on
misalignment, not as millisecond precision. Read-only: opens the database
with mode=ro.
"""
import re
import sqlite3
import statistics
import sys


def words(text):
    return [w for w in re.findall(r"\w+", text.lower()) if w]


def load(conn, item_id, origin):
    row = conn.execute(
        "SELECT id FROM transcripts WHERE item_id = ? AND origin = ? AND status = 'complete'",
        (item_id, origin),
    ).fetchone()
    if not row:
        sys.exit(f"no complete {origin} transcript for item {item_id}")
    return conn.execute(
        "SELECT start_ms, end_ms, text FROM transcript_cues WHERE transcript_id = ? ORDER BY seq",
        (row[0],),
    ).fetchall()


def main():
    db, item_id = sys.argv[1], int(sys.argv[2])
    ref_origin = sys.argv[3] if len(sys.argv) > 3 else "auto_subtitles"
    conn = sqlite3.connect(f"file:{db}?mode=ro", uri=True)
    ref, asr = load(conn, item_id, ref_origin), load(conn, item_id, "asr")

    timeline = []  # (word, interpolated ms)
    for start, end, text in ref:
        ws = words(text)
        for i, w in enumerate(ws):
            timeline.append((w, start + (end - start) * i / max(len(ws), 1)))

    offsets, unmatched = [], 0
    for start, _end, text in asr:
        key = words(text)[:3]
        if len(key) < 3:
            continue
        best = None
        for i in range(len(timeline) - 2):
            if [timeline[i][0], timeline[i + 1][0], timeline[i + 2][0]] == key:
                delta = start - timeline[i][1]
                if abs(delta) <= 15000 and (best is None or abs(delta) < abs(best)):
                    best = delta
        if best is None:
            unmatched += 1
        else:
            offsets.append(best)

    if not offsets:
        sys.exit("no matching phrases")
    absolute = sorted(abs(o) for o in offsets)
    p = lambda q: absolute[min(len(absolute) - 1, int(q * len(absolute)))]
    print(
        {
            "item_id": item_id,
            "reference": ref_origin,
            "asr_cues": len(asr),
            "matched": len(offsets),
            "unmatched": unmatched,
            "median_signed_ms": round(statistics.median(offsets)),
            "median_abs_ms": round(statistics.median(absolute)),
            "p90_abs_ms": round(p(0.9)),
            "max_abs_ms": round(absolute[-1]),
            "within_2s": round(sum(a <= 2000 for a in absolute) / len(absolute), 3),
        }
    )


if __name__ == "__main__":
    main()
