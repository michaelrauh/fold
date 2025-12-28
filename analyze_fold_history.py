#!/usr/bin/env python3
"""
Analyze fold_history run summaries: fit best-fit lines for disk usage and runtime
versus input size (words), extrapolate to a target file, and emit charts.
"""

from __future__ import annotations

import argparse
import pathlib
from dataclasses import dataclass
from typing import List, Sequence

try:
    import matplotlib.pyplot as plt
    import numpy as np
except ModuleNotFoundError as exc:
    missing = exc.name or "required module"
    raise SystemExit(
        f"Missing dependency '{missing}'. Install requirements with "
        "'python3 -m pip install matplotlib numpy'."
    )


@dataclass
class RunSummary:
    input_words: int
    disk_space_bytes: int
    runtime_secs: float
    ortho_count: int


def parse_run_summary(path: pathlib.Path) -> RunSummary | None:
    """Parse a single fold_history file; returns None if required fields are missing."""
    fields = {}
    for line in path.read_text().splitlines():
        if ":" not in line:
            continue
        key, val = line.split(":", 1)
        fields[key.strip()] = val.strip()

    required = ("input_words", "disk_space_bytes", "runtime_secs", "ortho_count")
    if not all(k in fields for k in required):
        return None

    try:
        return RunSummary(
            input_words=int(fields["input_words"]),
            disk_space_bytes=int(fields["disk_space_bytes"]),
            runtime_secs=float(fields["runtime_secs"]),
            ortho_count=int(fields["ortho_count"]),
        )
    except ValueError:
        return None


def load_history(dir_path: pathlib.Path) -> List[RunSummary]:
    runs: List[RunSummary] = []
    if not dir_path.exists():
        return runs
    for path in sorted(dir_path.glob("*.txt")):
        summary = parse_run_summary(path)
        if summary:
            runs.append(summary)
    return runs


def count_words(path: pathlib.Path) -> int:
    text = path.read_text(encoding="utf-8", errors="ignore")
    return len(text.split())


def fit_line(x: Sequence[float], y: Sequence[float]) -> tuple[np.poly1d, np.ndarray]:
    coeffs = np.polyfit(x, y, 1)
    return np.poly1d(coeffs), coeffs


def plot_fit(
    x: np.ndarray,
    y: np.ndarray,
    fit_fn: np.poly1d,
    target_x: float,
    ylabel: str,
    title: str,
    output_path: pathlib.Path,
) -> None:
    xs = np.linspace(0, max(target_x, x.max()) * 1.05, 100)
    plt.figure()
    plt.scatter(x, y, label="observed")
    plt.plot(xs, fit_fn(xs), color="orange", label="best fit")
    plt.axvline(target_x, color="gray", linestyle="--", linewidth=1, label="target input")
    plt.xlabel("input size (words)")
    plt.ylabel(ylabel)
    plt.title(title)
    plt.legend()
    plt.tight_layout()
    output_path.parent.mkdir(parents=True, exist_ok=True)
    plt.savefig(output_path)
    plt.close()


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument(
        "--history-dir",
        type=pathlib.Path,
        default=pathlib.Path("fold_history"),
        help="Directory containing run_*.txt summaries (default: fold_history)",
    )
    parser.add_argument(
        "--input-file",
        type=pathlib.Path,
        default=pathlib.Path("e.txt"),
        help="Path to target input file for extrapolation (default: e.txt)",
    )
    parser.add_argument(
        "--out-dir",
        type=pathlib.Path,
        default=pathlib.Path("charts"),
        help="Directory to write chart images",
    )
    args = parser.parse_args()

    runs = load_history(args.history_dir)
    if not runs:
        raise SystemExit(f"No run summaries found in {args.history_dir}")

    target_words = count_words(args.input_file)

    words = np.array([r.input_words for r in runs], dtype=float)
    disk = np.array([r.disk_space_bytes for r in runs], dtype=float)
    runtime = np.array([r.runtime_secs for r in runs], dtype=float)

    disk_fit, disk_coeffs = fit_line(words, disk)
    runtime_fit, runtime_coeffs = fit_line(words, runtime)

    pred_disk = float(disk_fit(target_words))
    pred_runtime = float(runtime_fit(target_words))

    print(f"Loaded {len(runs)} runs from {args.history_dir}")
    print(f"Target input ({args.input_file}): {target_words} words")
    print(f"Disk fit: y = {disk_coeffs[0]:.6g} * words + {disk_coeffs[1]:.6g}")
    print(f"Runtime fit: y = {runtime_coeffs[0]:.6g} * words + {runtime_coeffs[1]:.6g}")
    print(f"Predicted disk at target: {pred_disk:.0f} bytes")
    print(f"Predicted runtime at target: {pred_runtime:.3f} seconds")

    plot_fit(
        words,
        disk,
        disk_fit,
        target_words,
        ylabel="disk usage (bytes)",
        title="Disk usage vs input size",
        output_path=args.out_dir.joinpath("disk_vs_input.png"),
    )
    plot_fit(
        words,
        runtime,
        runtime_fit,
        target_words,
        ylabel="runtime (seconds)",
        title="Runtime vs input size",
        output_path=args.out_dir.joinpath("runtime_vs_input.png"),
    )


if __name__ == "__main__":
    main()
