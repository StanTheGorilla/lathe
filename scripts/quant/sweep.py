"""Where is S1-mini fragile? Quantize one thing at a time and measure the damage.

    python scripts/quant/sweep.py [quant/sweep.csv]

Every row is Q8_0 everywhere except the named part at Q4_K, scored by KL divergence
from the F16 model's own logits on `quant/heldout.txt` (built by make-calibration.py,
logits by `llama-perplexity --save-all-logits`). Rows: each of the 28 blocks, each of
the seven per-block tensor kinds across all blocks, the two vocabulary matrices, plain
Q8_0 as the floor, and F16 against itself as the noise floor.

The point is to replace "protect the first and last layers" folklore with a ranking
measured on this model and this text. The recipe in Phase 3 is written from the CSV.
"""

import csv
import os
import re
import subprocess
import sys
import time

TOOLS = os.path.join(".tools", "llama")
F16 = os.path.join("models", "s1-mini-f16.gguf")
IMATRIX = os.path.join("quant", "s1-mini.imatrix.gguf")
HELDOUT = os.path.join("quant", "heldout.txt")
LOGITS = os.path.join("quant", "heldout-f16.logits")
SCRATCH = os.path.join("quant", "sweep.gguf")

KINDS = ["attn_q", "attn_k", "attn_v", "attn_output", "ffn_gate", "ffn_up", "ffn_down"]


def quantize(overrides, base="Q8_0"):
    args = [os.path.join(TOOLS, "llama-quantize.exe"), "--imatrix", IMATRIX]
    for pattern, ty in overrides:
        args += ["--tensor-type", f"{pattern}={ty}"]
    args += [F16, SCRATCH, base]
    subprocess.run(args, check=True, capture_output=True)


def kl(model):
    run = subprocess.run(
        [os.path.join(TOOLS, "llama-perplexity.exe"), "-m", model, "-f", HELDOUT,
         "-c", "512", "-ngl", "99", "--kl-divergence", "--kl-divergence-base", LOGITS],
        check=True, capture_output=True, text=True, encoding="utf-8", errors="replace",
    )
    text = run.stdout + run.stderr
    mean = float(re.search(r"Mean\s+KLD:\s+(-?[\d.]+)", text).group(1))
    p99 = float(re.search(r"99\.0%\s+KLD:\s+(-?[\d.]+)", text).group(1))
    same = float(re.search(r"Same top p:\s+([\d.]+)", text).group(1))
    return mean, p99, same


def main(csv_path):
    rows = [("f16", None), ("q8_0", [])]
    rows += [("token_embd", [(r"token_embd\.weight", "q4_k")])]
    rows += [("output", [(r"output\.weight", "q4_k")])]
    rows += [(f"all.{k}", [(rf"blk\.\d+\.{k}\.weight", "q4_k")]) for k in KINDS]
    rows += [(f"blk.{n}", [(rf"blk\.{n}\..*", "q4_k")]) for n in range(28)]

    with open(csv_path, "w", newline="") as f:
        w = csv.writer(f)
        w.writerow(["part_at_q4_k", "mean_kld", "p99_kld", "same_top_pct", "size_mb"])
        for name, overrides in rows:
            start = time.time()
            if overrides is None:
                model = F16
            else:
                quantize(overrides)
                model = SCRATCH
            mean, p99, same = kl(model)
            size = os.path.getsize(model) / 1e6
            w.writerow([name, f"{mean:.6f}", f"{p99:.6f}", f"{same:.3f}", f"{size:.0f}"])
            f.flush()
            print(f"{name:14} KLD {mean:.6f}  p99 {p99:.5f}  same-top {same:.2f}%  {time.time()-start:.0f}s", flush=True)
    if os.path.exists(SCRATCH):
        os.remove(SCRATCH)


if __name__ == "__main__":
    main(sys.argv[1] if len(sys.argv) > 1 else os.path.join("quant", "sweep.csv"))
