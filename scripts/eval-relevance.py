#!/usr/bin/env python3
"""Scores segment-relevance output against labels.

    python3 -I scripts/eval-relevance.py tests/eval/relevance-labels.toml OUT.json [OUT.json ...]

OUT.json files come from `examples/bench_llm`. For each, reports:
- recall@50 / recall@20: share of labelled-relevant segments (label >= 1)
  the model scored at or above 50 / 20; the complement is the false-negative
  rate the project cares most about;
- strong recall@50: the same for label == 2;
- precision@50: share of segments scored >= 50 that are labelled relevant;
- Recall@K: relevant segments among the model's top K by score;
- AUC (relevant vs not) and AUC (label 2 vs the rest): threshold-free
  ranking quality, the probability that a randomly picked positive outscores
  a randomly picked negative (ties count half). With few negatives, prefer
  this over Recall@K, which is trivially high when most segments are
  relevant;
- mean score per label and the number of failed analyses.
"""
import json
import sys
import tomllib


def auc(pos, neg):
    if not pos or not neg:
        return None
    wins = sum((p > n) + 0.5 * (p == n) for p in pos for n in neg)
    return round(wins / (len(pos) * len(neg)), 3)


def main():
    with open(sys.argv[1], "rb") as f:
        labels = {(s["video"], s["start_ms"]): s["label"] for s in tomllib.load(f)["segment"]}
    rows = []
    for path in sys.argv[2:]:
        doc = json.load(open(path))
        got = {}
        failed = 0
        for r in doc["results"]:
            key = (r["video_id"], r["start_ms"])
            if key not in labels:
                continue
            if "analysis" in r:
                got[key] = r["analysis"]["relevance"]
            else:
                failed += 1
        scored = [(labels[k], s) for k, s in got.items()]
        rel = [s for l, s in scored if l >= 1]
        strong = [s for l, s in scored if l == 2]
        flagged = [l for l, s in scored if s >= 50]
        ranked = [l for l, s in sorted(scored, key=lambda x: -x[1])]
        n_rel = sum(1 for l in labels.values() if l >= 1)

        def recall_at_k(k):
            return sum(1 for l in ranked[:k] if l >= 1) / min(k, n_rel)

        def mean(xs):
            return round(sum(xs) / len(xs), 1) if xs else None

        rows.append({
            "model": doc["summary"]["model"],
            "scored": len(scored),
            "failed": failed,
            "recall@50": round(sum(s >= 50 for s in rel) / max(len(rel), 1), 3),
            "recall@20": round(sum(s >= 20 for s in rel) / max(len(rel), 1), 3),
            "strong_recall@50": round(sum(s >= 50 for s in strong) / max(len(strong), 1), 3),
            "precision@50": round(sum(l >= 1 for l in flagged) / max(len(flagged), 1), 3),
            "auc_relevant": auc(rel, [s for l, s in scored if l == 0]),
            "auc_strong": auc(strong, [s for l, s in scored if l <= 1]),
            "recall@5": round(recall_at_k(5), 3),
            "recall@10": round(recall_at_k(10), 3),
            "mean_score_label": {lbl: mean([s for l, s in scored if l == lbl]) for lbl in (0, 1, 2)},
            "mean_segment_s": round(doc["summary"]["mean_segment_s"], 2),
            "peak_vram_mib": doc["summary"]["server_peak_vram_mib"],
        })
    for r in rows:
        print(json.dumps(r, ensure_ascii=False))


if __name__ == "__main__":
    main()
