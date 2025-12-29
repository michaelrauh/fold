#!/usr/bin/env python3
"""
Generate charts of generation durations across input sizes plus total runtime vs input.

Reads a stats.txt-style file (run summaries with generation_stats blocks) and emits:
- charts/gen_duration_gen{N}.png for each generation that appears in the data
- charts/total_runtime.png for total runtime per input size
"""

from __future__ import annotations

import pathlib
import math
import re
from collections import defaultdict
from typing import Dict, List, Tuple

import matplotlib

# Use a non-interactive backend
matplotlib.use("Agg")
import matplotlib.pyplot as plt  # noqa: E402
import pandas as pd  # noqa: E402


def parse_stats(path: pathlib.Path) -> Tuple[List[int], List[float], Dict[int, List[Tuple[int, float]]]]:
    text = path.read_text()
    blocks = [b.strip() for b in text.split("optimal_ortho:") if b.strip()]

    input_sizes: List[int] = []
    total_runtimes: List[float] = []
    gen_durations: Dict[int, List[Tuple[int, float]]] = defaultdict(list)

    for block in blocks:
        m_words = re.search(r"input_words:\s*(\d+)", block)
        m_runtime = re.search(r"runtime_secs:\s*([0-9.]+)", block)
        if not m_words or not m_runtime:
            continue

        words = int(m_words.group(1))
        runtime = float(m_runtime.group(1))
        input_sizes.append(words)
        total_runtimes.append(runtime)

        for line in block.splitlines():
            m_gen = re.match(r"\s*gen\s+(\d+):\s+duration_secs=([0-9.]+)", line)
            if m_gen:
                gen_idx = int(m_gen.group(1))
                dur = float(m_gen.group(2))
                gen_durations[gen_idx].append((words, dur))

    return input_sizes, total_runtimes, gen_durations


def parse_runs(path: pathlib.Path) -> List[Tuple[int, List[float]]]:
    """Return list of (input_words, [duration per gen]) preserving per-run sequences."""
    text = path.read_text()
    blocks = [b.strip() for b in text.split("optimal_ortho:") if b.strip()]
    runs: List[Tuple[int, List[float]]] = []
    for block in blocks:
        m_words = re.search(r"input_words:\s*(\d+)", block)
        if not m_words:
            continue
        words = int(m_words.group(1))
        gens: List[Tuple[int, float]] = []
        for line in block.splitlines():
            m = re.match(r"\s*gen\s+(\d+):\s+duration_secs=([0-9.]+)", line)
            if m:
                gens.append((int(m.group(1)), float(m.group(2))))
        if gens:
            gens.sort(key=lambda x: x[0])
            max_idx = max(g[0] for g in gens)
            durations = [0.0] * (max_idx + 1)
            for idx, dur in gens:
                durations[idx] = dur
        else:
            durations = []
        runs.append((words, durations))
    return runs


def plot_total_runtime(inputs: List[int], runtimes: List[float], out_dir: pathlib.Path) -> pathlib.Path:
    order = sorted(zip(inputs, runtimes))
    xs, ys = zip(*order)
    plt.figure(figsize=(8, 5))
    plt.plot(xs, ys, marker="o")
    plt.xlabel("Input size (words)")
    plt.ylabel("Total runtime (secs)")
    plt.title("Total runtime vs input size")
    plt.grid(True, alpha=0.3)
    out_path = out_dir / "total_runtime.png"
    plt.tight_layout()
    plt.savefig(out_path)
    plt.close()
    return out_path


def plot_generation_slices(gen_durations: Dict[int, List[Tuple[int, float]]], out_dir: pathlib.Path) -> List[pathlib.Path]:
    paths: List[pathlib.Path] = []
    for gen_idx in sorted(gen_durations.keys()):
        pairs = gen_durations[gen_idx]
        if not pairs:
            continue
        ordered = sorted(pairs)
        xs, ys = zip(*ordered)

        # Skip charts where all durations are zero
        if all(math.isclose(y, 0.0) for y in ys):
            continue

        plt.figure(figsize=(8, 5))
        plt.plot(xs, ys, marker="o")
        plt.xlabel("Input size (words)")
        plt.ylabel(f"Gen {gen_idx} duration (secs)")
        plt.title(f"Generation {gen_idx} duration vs input size")
        plt.grid(True, alpha=0.3)
        out_path = out_dir / f"gen_duration_gen{gen_idx}.png"
        plt.tight_layout()
        plt.savefig(out_path)
        plt.close()
        paths.append(out_path)
    return paths


def plot_stacked_generations(
    runs: List[Tuple[int, List[float]]],
    out_dir: pathlib.Path,
    min_words: int = 0,
) -> pathlib.Path | None:
    filtered = [(w, d) for w, d in runs if w >= min_words and d]
    if not filtered:
        return None

    # Trim trailing zeros per run to avoid huge color counts
    trimmed = []
    for words, durs in filtered:
        trimmed_durs = list(durs)
        while trimmed_durs and abs(trimmed_durs[-1]) < 1e-9:
            trimmed_durs.pop()
        if not trimmed_durs:
            continue
        trimmed.append((words, trimmed_durs))

    if not trimmed:
        return None

    trimmed.sort(key=lambda x: x[0])
    labels = [str(w) for w, _ in trimmed]
    max_len = max(len(d) for _, d in trimmed)
    padded = []
    for _, durs in trimmed:
        row = list(durs) + [0.0] * (max_len - len(durs))
        padded.append(row)

    import numpy as np

    mat = np.array(padded)
    # Use a categorical palette with good contrast; reuse if gens exceed palette
    base_colors = matplotlib.cm.tab20(np.linspace(0, 1, 20))

    plt.figure(figsize=(10, 6))
    bottom = np.zeros(len(trimmed))
    for i in range(max_len):
        # Skip columns that are all zeros to reduce legend spam
        if np.all(mat[:, i] == 0):
            continue
        color = base_colors[i % len(base_colors)]
        plt.bar(range(len(trimmed)), mat[:, i], bottom=bottom, color=color, width=0.6, edgecolor="black", linewidth=0.3)
        bottom += mat[:, i]

    plt.xticks(range(len(trimmed)), labels, rotation=45)
    plt.ylabel("Runtime (secs)")
    plt.xlabel("Input words")
    plt.title(f"Stacked generation runtimes (min {min_words} words)")
    plt.tight_layout()
    out_path = out_dir / "stacked_generation_runtimes.png"
    plt.savefig(out_path)
    plt.close()
    return out_path


def plot_grouped_stacked_pandas(
    runs: List[Tuple[int, List[float]]],
    out_dir: pathlib.Path,
    min_words: int = 0,
) -> pathlib.Path | None:
    filtered = []
    for words, durs in runs:
        if words < min_words or not durs:
            continue
        # trim trailing zeros
        trimmed = list(durs)
        while trimmed and abs(trimmed[-1]) < 1e-9:
            trimmed.pop()
        if not trimmed:
            continue
        filtered.append((words, trimmed))

    if not filtered:
        return None

    filtered.sort(key=lambda x: x[0])
    max_len = max(len(d) for _, d in filtered)
    prune_threshold = 0.01  # drop generations that are essentially zero (e.g., 0.002s)
    data = {}
    for i in range(max_len):
        col = [d[i] if i < len(d) else 0.0 for _, d in filtered]
        if max(abs(v) for v in col) < prune_threshold:
            continue
        data[f"gen_{i}"] = col
    index = [str(w) for w, _ in filtered]
    df = pd.DataFrame(data, index=index)

    ax = df.plot(
        kind="bar",
        stacked=False,
        colormap="tab20",
        figsize=(14, 7),
        edgecolor="black",
        linewidth=0.3,
    )
    ax.set_xlabel("Input words")
    ax.set_ylabel("Runtime (secs)")
    ax.set_title(f"Generation runtimes by run (min {min_words} words)")
    ymax = df.max().max()
    if ymax and ymax > 0:
        ax.set_ylim(0, ymax * 1.05)
    plt.xticks(rotation=45, ha="right")
    plt.tight_layout()
    out_path = out_dir / "grouped_generation_runtimes.png"
    plt.savefig(out_path)
    plt.close()
    return out_path


def main() -> None:
    stats_file = pathlib.Path("stats.txt")
    if not stats_file.exists():
        raise SystemExit("stats.txt not found")

    inputs, runtimes, gen_durations = parse_stats(stats_file)
    runs = parse_runs(stats_file)
    if not inputs:
        raise SystemExit("No records found in stats.txt")

    out_dir = pathlib.Path("charts")
    out_dir.mkdir(exist_ok=True)

    runtime_path = plot_total_runtime(inputs, runtimes, out_dir)
    gen_paths = plot_generation_slices(gen_durations, out_dir)
    stacked_path = plot_stacked_generations(runs, out_dir, min_words=5000)
    grouped_path = plot_grouped_stacked_pandas(runs, out_dir, min_words=5000)

    print(f"Wrote {runtime_path}")
    for p in gen_paths:
        print(f"Wrote {p}")
    if stacked_path:
        print(f"Wrote {stacked_path}")
    if grouped_path:
        print(f"Wrote {grouped_path}")


if __name__ == "__main__":
    main()
