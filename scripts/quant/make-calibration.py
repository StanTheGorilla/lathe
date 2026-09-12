"""Builds the text files `llama-imatrix` and `llama-perplexity` read.

    python scripts/quant/make-calibration.py

    quant/calibration.txt  history-calib.jsonl + assets/cleanup-eval.jsonl, for the imatrix
    quant/heldout.txt      the F16 outputs on both eval sets, for KL divergence

Each item is written exactly as the model sees it at runtime -- the S1-mini prompt from
`crates/core/src/cleanup.rs` followed by the expected answer and the end-of-turn token --
so the activation statistics describe dictation cleanup and not English in general. An
imatrix built on Wikipedia would protect the wrong weights: this model never sees
Wikipedia, it sees a control line, a transcript, and a cleaned transcript.
"""

import json
import os
import subprocess

SYSTEM = (
    "You are a text normalizer for speech-to-text transcripts. The input begins with a "
    "control line specifying the styling, structure, and context settings; clean the "
    "transcript to match those settings and output only the cleaned text."
)


def prompt(item):
    return (
        f"<|im_start|>system\n{SYSTEM}<|im_end|>\n"
        f"<|im_start|>user\n[Styling: {item['styling']}] [Structure: {item['structure']}] "
        f"[Context: {item['context']}]\n{item['raw']}<|im_end|>\n"
        "<|im_start|>assistant\n<think>\n\n</think>\n\n"
    )


def read(path):
    with open(path, encoding="utf-8") as f:
        return [json.loads(l) for l in f if l.strip()]


def write(path, items, answer_key):
    with open(path, "w", encoding="utf-8", newline="\n") as f:
        for item in items:
            f.write(prompt(item) + item[answer_key] + "<|im_end|>\n")
    print(f"{len(items)} items -> {path}")


def check_prompt_matches_app():
    """The format above is a copy; make sure it has not drifted from the Rust."""
    item = {"raw": "hello there", "styling": "formal", "structure": "lists", "context": "email"}
    exe = os.path.join("target", "release", "lathe-spike.exe")
    app = subprocess.run(
        [exe, "clean", item["raw"], "--styling", "formal", "--structure", "lists",
         "--context", "email", "--show-prompt"],
        capture_output=True, text=True, check=True, encoding="utf-8",
    ).stdout
    if app != prompt(item):
        raise SystemExit("prompt format differs from lathe-spike clean --show-prompt")


if __name__ == "__main__":
    check_prompt_matches_app()
    write(
        "quant/calibration.txt",
        read("quant/history-calib.jsonl") + read("assets/cleanup-eval.jsonl"),
        "clean",
    )
    write(
        "quant/heldout.txt",
        read("quant/history-f16.jsonl") + read("quant/synthetic-f16.jsonl"),
        "cleaned",
    )
