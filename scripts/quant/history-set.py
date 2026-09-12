"""Splits the dictation history into a held-out eval set and a calibration set.

    python scripts/quant/history-set.py [quant/]

Reads `%APPDATA%\\Lathe\\history.db` and `config.toml` (for the control line each preset
maps to) and writes, into the given directory (default `quant/`, which is untracked):

    history-eval.jsonl   a third of the dictations, `cleanup-eval` input format
    history-calib.jsonl  the other two thirds, with the app's own cleaned output

Real dictation is messier than anything synthesised, so the eval third is the honest
half of the score. The split is by row id, so it is stable across runs and no dictation
ever crosses from calibration into eval. Nothing here leaves the machine: the eval file
is scored locally and the calibration file only ever feeds `llama-imatrix`, whose output
is activation statistics, not text.
"""

import json
import os
import sqlite3
import sys
import tomllib

# The preset a row was recorded under may since have been renamed or deleted; fall back
# to the app's own default control line.
DEFAULT = ("semi-formal", "prose", "general")


def main(out_dir):
    appdata = os.path.join(os.environ["APPDATA"], "Lathe")
    with open(os.path.join(appdata, "config.toml"), "rb") as f:
        config = tomllib.load(f)
    presets = {
        p["name"]: (p["styling"], p["structure"], p["context"])
        for p in config.get("presets", [])
    }

    db = sqlite3.connect(os.path.join(appdata, "history.db"))
    rows = db.execute(
        "SELECT id, preset, raw, cleaned FROM dictations WHERE raw <> '' ORDER BY id"
    ).fetchall()

    os.makedirs(out_dir, exist_ok=True)
    counts = {"eval": 0, "calib": 0}
    with open(os.path.join(out_dir, "history-eval.jsonl"), "w", encoding="utf-8", newline="\n") as ev, \
            open(os.path.join(out_dir, "history-calib.jsonl"), "w", encoding="utf-8", newline="\n") as cal:
        for row_id, preset, raw, cleaned in rows:
            styling, structure, context = presets.get(preset, DEFAULT)
            item = {"raw": raw, "styling": styling, "structure": structure, "context": context}
            if row_id % 3 == 0:
                ev.write(json.dumps(item, ensure_ascii=False) + "\n")
                counts["eval"] += 1
            else:
                item["clean"] = cleaned
                cal.write(json.dumps(item, ensure_ascii=False) + "\n")
                counts["calib"] += 1
    print(f"{len(rows)} dictations: {counts['eval']} eval, {counts['calib']} calibration -> {out_dir}")


if __name__ == "__main__":
    main(sys.argv[1] if len(sys.argv) > 1 else "quant")
