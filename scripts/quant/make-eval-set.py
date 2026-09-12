"""Turns a corpus of clean texts into simulated raw dictation for `lathe-spike cleanup-eval`.

    python scripts/quant/make-eval-set.py corpus.json assets/cleanup-eval.jsonl [pl]

The recogniser (Cohere Transcribe) already emits punctuation and capitals, so raw
dictation is not lowercase word soup: it is punctuated text with spoken artefacts in it.
Measured over 180 real dictations, roughly half open with "okay" or "so", 40% contain
"like", and self-corrections, repeated words and "I mean" / "you know" appear in a few
percent each. That is what gets injected here, in those proportions, deterministically.
A tenth of the items are additionally flattened to lowercase without punctuation, to
cover a recogniser that does not punctuate.

The clean text is kept on each line: it is not used for scoring (the F16 model's own
output is the reference) but it is the target half of the calibration text for
`llama-imatrix`.
"""

import json
import random
import re
import sys

ARTEFACTS = {
    "en": {
        "openers": ["Okay, so ", "So, ", "Okay, ", "So, um, ", "Alright, so ", "Yeah, so "],
        "fillers": ["like", "I mean,", "you know,", "basically", "actually", "kind of", "sort of"],
        "stray": r"\b(is|was|really|very) ",
        "stray_word": "like",
        "retract": ", no,",
        "capital_i": ("I ", "I'"),
    },
    # The same artefact classes as spoken Polish produces them. Cohere punctuates
    # Polish too, so the shape is the same: punctuated text with speech in it.
    "pl": {
        "openers": ["No więc ", "Okej, ", "Dobra, ", "No dobra, ", "Więc, ", "Okej, więc "],
        "fillers": ["znaczy,", "jakby", "no", "wiesz,", "tak naprawdę", "w sensie,"],
        "stray": r"\b(jest|było|bardzo|są) ",
        "stray_word": "jakby",
        "retract": ", nie,",
        "capital_i": (),
    },
}

# What each kind of text is usually dictated as. The control line is part of the input
# the model sees, so every combination has to appear somewhere in the set.
PRESETS = {
    "prompt": (["semi-formal", "casual", "semi-casual"], ["prose"], ["general"]),
    "email": (["semi-formal", "formal"], ["prose"], ["email"]),
    "message": (["casual", "semi-casual"], ["prose"], ["general"]),
    "note": (["semi-casual", "casual"], ["prose", "lists"], ["general"]),
    "list": (["semi-formal", "casual"], ["lists"], ["general", "email"]),
}


def declean(text, rng, lang="en"):
    a = ARTEFACTS[lang]
    words = text.split()
    out = []
    for i, w in enumerate(words):
        # Repeated word, 2% of words -- "the the file".
        if rng.random() < 0.02 and re.match(r"^[^\W\d_]+$", w) and w.islower():
            out.append(w)
        # Self-correction, 1% of words: say a wrong word, retract it.
        if rng.random() < 0.01 and i > 2 and re.match(r"^\w+,?$", w):
            other = rng.choice(words)
            if other != w:
                out.append(other.rstrip(".,?!") + a["retract"])
        out.append(w)
        # Filler inserted after a comma, 8% of commas.
        if w.endswith(",") and rng.random() < 0.08:
            out.append(rng.choice(a["fillers"]))
    raw = " ".join(out)
    # Opener, half the time. The sentence's own capital goes, except a leading "I".
    if rng.random() < 0.5:
        first = raw[0] if a["capital_i"] and raw.startswith(a["capital_i"]) else raw[0].lower()
        raw = rng.choice(a["openers"]) + first + raw[1:]
    # Stray "like", 20% of items, before an adjective-ish position (any word that
    # follows "is", "was", "really", "very").
    if rng.random() < 0.2:
        raw = re.sub(a["stray"], r"\1 " + a["stray_word"] + " ", raw, count=1)
    # Flattened recogniser, 10% of items.
    if rng.random() < 0.1:
        raw = re.sub(r"[^\w\s']", "", raw).lower()
    return raw


def main(src, dst, lang="en"):
    rng = random.Random(20260912)
    corpus = json.load(open(src, encoding="utf-8"))
    with open(dst, "w", encoding="utf-8", newline="\n") as f:
        for entry in corpus:
            kind = entry["kind"]
            stylings, structures, contexts = PRESETS[kind]
            item = {
                "raw": declean(entry["text"], rng, lang),
                "styling": rng.choice(stylings),
                "structure": rng.choice(structures),
                "context": rng.choice(contexts),
                "kind": kind,
                "clean": entry["text"],
            }
            f.write(json.dumps(item, ensure_ascii=False) + "\n")
    print(f"{len(corpus)} items -> {dst}")


if __name__ == "__main__":
    main(sys.argv[1], sys.argv[2], sys.argv[3] if len(sys.argv) > 3 else "en")
