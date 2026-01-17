#!/usr/bin/env python3
import argparse
from pathlib import Path
import re

import matplotlib.pyplot as plt
import pandas as pd

RUN_RE = re.compile(r"run_(\d+)_role=.*\.txt")


def parse_run(path: Path) -> dict:
    data = {
        "file": path.name,
        "timestamp": None,
        "ortho_count": None,
        "disk_space_bytes": None,
        "runtime_secs": None,
        "input_words": None,
        "pruned_total": 0,
        "expanded_total": 0,
        "pruned_root_span": 0,
        "pruned_bound": 0,
        "compaction_pruned": 0,
        "compaction_kept": 0,
        "impacted_pruned_a": 0,
        "impacted_pruned_b": 0,
        "gen_samples": [],
    }
    m = RUN_RE.match(path.name)
    if m:
        data["timestamp"] = int(m.group(1))

    lines = path.read_text().splitlines()
    prune_section = False
    for line in lines:
        line = line.strip()
        if line.startswith("ortho_count:"):
            data["ortho_count"] = int(line.split(":")[1].strip())
        elif line.startswith("disk_space_bytes:"):
            data["disk_space_bytes"] = int(line.split(":")[1].strip())
        elif line.startswith("runtime_secs:"):
            data["runtime_secs"] = float(line.split(":")[1].strip())
        elif line.startswith("input_words:"):
            data["input_words"] = int(line.split(":")[1].strip())
        elif line.startswith("pruning:"):
            prune_section = True
            continue
        elif line.startswith("optimal_ortho:"):
            prune_section = False
        if prune_section:
            if line.startswith("compaction:"):
                parts = dict(kv.split("=") for kv in line.split(":")[1].strip().split())
                data["compaction_kept"] = int(parts.get("kept", 0))
                data["compaction_pruned"] = int(parts.get("pruned", 0))
            elif line.startswith("impacted_pruned:"):
                parts = dict(kv.split("=") for kv in line.split(":")[1].strip().split())
                data["impacted_pruned_a"] = int(parts.get("A", 0))
                data["impacted_pruned_b"] = int(parts.get("B", 0))
            elif line.startswith("gen "):
                # gen N: pruned=X expanded=Y ... root_span=Z bound=W
                kvs = dict(
                    kv.split("=")
                    for kv in line.split(":")[1]
                    .strip()
                    .replace("%", "")
                    .split()
                    if "=" in kv
                )
                gen_num = int(line.split()[1].strip(":"))
                pruned = int(kvs.get("pruned", 0))
                expanded = int(kvs.get("expanded", 0))
                root_span = int(kvs.get("root_span", 0))
                bound = int(kvs.get("bound", 0))
                data["pruned_total"] += pruned
                data["expanded_total"] += expanded
                data["pruned_root_span"] += root_span
                data["pruned_bound"] += bound
                data["gen_samples"].append(
                    {
                        "generation": gen_num,
                        "pruned": pruned,
                        "expanded": expanded,
                        "root_span": root_span,
                        "bound": bound,
                        "file": path.name,
                    }
                )
    return data


def main():
    ap = argparse.ArgumentParser(description="Aggregate fold_history pruning summaries.")
    ap.add_argument(
        "history_dir",
        nargs="?",
        default="fold_history",
        help="Directory with run_*.txt files (default: fold_history)",
    )
    ap.add_argument(
        "--out",
        default="pruning_summary.png",
        help="Output plot file (default: pruning_summary.png)",
    )
    ap.add_argument(
        "--per-gen-out",
        default="pruning_per_generation.png",
        help="Output plot for per-generation totals (default: pruning_per_generation.png)",
    )
    args = ap.parse_args()

    paths = sorted(Path(args.history_dir).glob("run_*.txt"))
    if not paths:
        raise SystemExit(f"No run_*.txt files found in {args.history_dir}")

    rows = [parse_run(p) for p in paths]
    df = pd.DataFrame(rows)
    all_gen_samples = [
        sample for row in rows for sample in row.get("gen_samples", [])
    ]

    print("Aggregated runs:", len(df))
    total_pruned = df["pruned_total"].sum()
    total_bound = df["pruned_bound"].sum()
    total_root = df["pruned_root_span"].sum()
    total_compaction = df["compaction_pruned"].sum()
    total_impacted = df["impacted_pruned_a"].sum() + df["impacted_pruned_b"].sum()

    summary = pd.DataFrame(
        {
            "root_span": [total_root],
            "bound": [total_bound],
            "compaction": [total_compaction],
            "impacted": [total_impacted],
            "total_pruned": [total_pruned],
        }
    ).T.rename(columns={0: "count"})
    print(summary)

    fig, ax = plt.subplots(figsize=(10, 6))
    categories = ["root_span", "bound", "compaction", "impacted"]
    values = [total_root, total_bound, total_compaction, total_impacted]
    bars = ax.bar(categories, values, color=["#6baed6", "#3182bd", "#31a354", "#e6550d"])
    ax.bar_label(bars, fmt="%.0f")
    ax.set_ylabel("Count")
    ax.set_title("Pruning counts by type (aggregated)")
    ax.grid(True, axis="y", alpha=0.3)
    plt.tight_layout()
    plt.savefig(args.out)
    print(f"Wrote plot to {args.out}")

    if all_gen_samples:
        gen_df = pd.DataFrame(all_gen_samples)
        gen_grouped = (
            gen_df.groupby("generation")[["pruned", "expanded", "root_span", "bound"]]
            .sum()
            .sort_index()
        )
        print("\nPruned per generation:\n", gen_grouped[["pruned"]])

        fig2, ax2 = plt.subplots(figsize=(12, 6))
        bars = ax2.bar(
            gen_grouped.index,
            gen_grouped["pruned"],
            color="#636efa",
            label="pruned",
        )
        ax2.bar_label(bars, fmt="%.0f", rotation=90, padding=2)
        ax2.set_xlabel("Generation")
        ax2.set_ylabel("Pruned (count)")
        ax2.set_title("Pruned completions per generation (aggregated)")
        ax2.grid(True, axis="y", alpha=0.3)
        plt.tight_layout()
        plt.savefig(args.per_gen_out)
        print(f"Wrote per-generation plot to {args.per_gen_out}")


if __name__ == "__main__":
    main()
