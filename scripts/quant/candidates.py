"""Builds the candidate quantizations and scores every one the same way.

    python scripts/quant/candidates.py [name ...]

For each recipe: quantize from the F16 with the domain imatrix, run `lathe-spike
cleanup-eval` on the synthetic set and the held-out history set against the F16
references, run the KL divergence, and print one summary table. Weights land in
`quant/s1-mini-<name>.gguf`, eval outputs beside them.

The recipes are written from `quant/sweep.csv`: `output.weight` is the single most
fragile tensor and stays at Q8_0 wherever possible; `token_embd` is a lookup that never
touches decode bandwidth and goes to Q4_K everywhere; among the per-block tensors,
attn_v / ffn_up / ffn_down / attn_output lose the most at 4-bit and attn_q / attn_k /
ffn_gate the least; blocks 0, 11 and 16-19 lose noticeably more than the rest.
"""

import json
import os
import re
import subprocess
import sys

TOOLS = os.path.join(".tools", "llama")
SPIKE = os.path.join("target", "release", "lathe-spike.exe")
F16 = os.path.join("models", "s1-mini-f16.gguf")
IMATRIX = os.path.join("quant", "s1-mini.imatrix.gguf")

FRAGILE = r"blk\.(0|11|16|17|18|19)\..*"
SENSITIVE = r"blk\.\d+\.(attn_v|ffn_up|ffn_down|attn_output)\.weight"
TOLERANT = r"blk\.\d+\.(attn_q|attn_k|ffn_gate)\.weight"

# name -> (base ftype, use imatrix, [(pattern, type)])
RECIPES = {
    # The bar: what a plain 8-bit conversion gives.
    "q8_0": ("Q8_0", False, []),
    # Q8 quality at ~60% of the bytes on the decode path.
    "balanced": ("Q6_K", True, [
        (r"token_embd\.weight", "q4_k"),
        (r"output\.weight", "q8_0"),
        (FRAGILE, "q8_0"),
        (SENSITIVE, "q6_k"),
        (TOLERANT, "q5_k"),
    ]),
    # Q4_K_M's size with the fragile parts protected. Where does it break?
    "tight": ("Q5_K_M", True, [
        (r"token_embd\.weight", "q4_k"),
        (r"output\.weight", "q6_k"),
        (FRAGILE, "q8_0"),
        (SENSITIVE, "q5_k"),
        (TOLERANT, "q4_k"),
    ]),
    # Above Q8: spend bytes where the sweep said Q8 itself still loses, since the
    # decode path is not bandwidth-bound below ~1 GB and a few ms is worth fidelity.
    "q8-output-f16": ("Q8_0", False, [
        (r"output\.weight", "f16"),
    ]),
    "q8-plus": ("Q8_0", False, [
        (r"output\.weight", "f16"),
        (FRAGILE, "f16"),
    ]),
    "q8-max": ("Q8_0", False, [
        (r"output\.weight", "f16"),
        (FRAGILE, "f16"),
        (SENSITIVE, "f16"),
    ]),
    # Trying to beat plain Q8 on both axes: fewer bytes on the decode path where the
    # sweep said 4-bit already costs least, and the output head at F16 where it costs most.
    "lean": ("Q8_0", True, [
        (r"output\.weight", "f16"),
        (TOLERANT, "q6_k"),
    ]),
    "lean6": ("Q6_K", True, [
        (r"output\.weight", "f16"),
        (FRAGILE, "q8_0"),
        (SENSITIVE, "q8_0"),
    ]),
    "q6-out16": ("Q6_K", True, [
        (r"output\.weight", "f16"),
        (FRAGILE, "q8_0"),
    ]),
    # Output head at Q8 (F16 there costs 150 MB on the decode path for nothing), the
    # three tolerant tensor kinds at Q6_K with the imatrix, everything else Q8.
    "q8-lean": ("Q8_0", True, [
        (TOLERANT, "q6_k"),
    ]),
    "q8-lean-embd": ("Q8_0", True, [
        (TOLERANT, "q6_k"),
        (r"token_embd\.weight", "q6_k"),
    ]),
    # Balanced without the imatrix: how much the calibration itself buys.
    "balanced-noimatrix": ("Q6_K", False, [
        (r"token_embd\.weight", "q4_k"),
        (r"output\.weight", "q8_0"),
        (FRAGILE, "q8_0"),
        (SENSITIVE, "q6_k"),
        (TOLERANT, "q5_k"),
    ]),
}

SETS = {
    "synthetic": ("assets/cleanup-eval.jsonl", "quant/synthetic-f16.jsonl"),
    "history": ("quant/history-eval.jsonl", "quant/history-f16.jsonl"),
}


def quantize(name):
    base, imatrix, overrides = RECIPES[name]
    out = os.path.join("quant", f"s1-mini-{name}.gguf")
    args = [os.path.join(TOOLS, "llama-quantize.exe")]
    if imatrix:
        args += ["--imatrix", IMATRIX]
    for pattern, ty in overrides:
        args += ["--tensor-type", f"{pattern}={ty}"]
    args += [F16, out, base]
    subprocess.run(args, check=True, capture_output=True)
    return out


def evaluate(model, set_name):
    set_path, reference = SETS[set_name]
    out = os.path.join("quant", f"{set_name}-{os.path.basename(model)}.jsonl")
    run = subprocess.run(
        [SPIKE, "--models", os.path.dirname(model), "--cleanup-model", os.path.basename(model),
         "cleanup-eval", "--set", set_path, "--out", out, "--reference", reference],
        check=True, capture_output=True, text=True, encoding="utf-8", errors="replace",
    )
    text = run.stdout
    with open(out.replace(".jsonl", ".report.txt"), "w", encoding="utf-8") as f:
        f.write(text)
    m = re.search(r"decode ([\d.]+)ms \(([\d.]+)ms/token", text)
    e = re.search(r"exact (\d+)/(\d+) \(([\d.]+)%\), WER ([\d.]+)%, content lost in (\d+)", text)
    return {
        "decode_ms": float(m.group(1)),
        "ms_per_token": float(m.group(2)),
        "exact_pct": float(e.group(3)),
        "wer_pct": float(e.group(4)),
        "lost": int(e.group(5)),
    }


def kl(model):
    run = subprocess.run(
        [os.path.join(TOOLS, "llama-perplexity.exe"), "-m", model, "-f", "quant/heldout.txt",
         "-c", "512", "-ngl", "99", "--kl-divergence", "--kl-divergence-base", "quant/heldout-f16.logits"],
        check=True, capture_output=True, text=True, encoding="utf-8", errors="replace",
    )
    text = run.stdout + run.stderr
    return {
        "kld": float(re.search(r"Mean\s+KLD:\s+(-?[\d.]+)", text).group(1)),
        "same_top": float(re.search(r"Same top p:\s+([\d.]+)", text).group(1)),
    }


def main(names):
    results = []
    for name in names:
        model = quantize(name)
        row = {"name": name, "size_mb": os.path.getsize(model) / 1e6}
        row.update(kl(model))
        for set_name in SETS:
            r = evaluate(model, set_name)
            row.update({f"{set_name}_{k}": v for k, v in r.items()})
        results.append(row)
        print(json.dumps(row), flush=True)
    with open(os.path.join("quant", "candidates.json"), "w") as f:
        json.dump(results, f, indent=1)
    print()
    print(f"{'candidate':20} {'MB':>5} {'KLD':>8} {'top%':>6} | {'synth exact':>11} {'WER':>6} {'lost':>4} | {'hist exact':>10} {'WER':>6} {'lost':>4} | {'ms/tok':>6}")
    for r in results:
        print(f"{r['name']:20} {r['size_mb']:5.0f} {r['kld']:8.5f} {r['same_top']:6.2f} | "
              f"{r['synthetic_exact_pct']:10.1f}% {r['synthetic_wer_pct']:5.2f}% {r['synthetic_lost']:4} | "
              f"{r['history_exact_pct']:9.1f}% {r['history_wer_pct']:5.2f}% {r['history_lost']:4} | "
              f"{r['synthetic_ms_per_token']:6.2f}")


if __name__ == "__main__":
    main(sys.argv[1:] or list(RECIPES))
