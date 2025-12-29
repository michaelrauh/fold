#!/usr/bin/env python3
"""
Per-generation curve fitting of runtimes.
Parses stats.txt, fits the same model type to each generation (chosen by best average R^2),
and writes a report with per-generation R^2 and predicted durations at the target input size
(defaults to word count of e.txt). Also outputs the summed total prediction.
"""
from __future__ import annotations

import pathlib
import re
from typing import Dict, List, Tuple

import numpy as np
from sklearn.linear_model import LinearRegression
from sklearn.pipeline import make_pipeline
from sklearn.preprocessing import PolynomialFeatures

STATS_PATH = pathlib.Path("stats.txt")
TARGET_FILE = pathlib.Path("e.txt")
MIN_WORDS = 5000  # skip small runs


def parse_runs(path: pathlib.Path) -> Dict[int, List[Tuple[int, float]]]:
    text = path.read_text()
    blocks = [b.strip() for b in text.split("optimal_ortho:") if b.strip()]
    by_gen: Dict[int, List[Tuple[int, float]]] = {}
    for block in blocks:
        m_words = re.search(r"input_words:\s*(\d+)", block)
        if not m_words:
            continue
        words = int(m_words.group(1))
        if words < MIN_WORDS:
            continue
        for line in block.splitlines():
            m = re.match(r"\s*gen\s+(\d+):\s+processing_secs=([0-9.]+)\s+transition_secs=([0-9.]+)", line)
            if m:
                gen = int(m.group(1))
                proc = float(m.group(2))
                trans = float(m.group(3))
                by_gen.setdefault(gen, []).append((words, proc + trans))
    # sort points per gen
    for gen in by_gen:
        by_gen[gen].sort(key=lambda x: x[0])
    return by_gen


def build_models() -> Dict[str, object]:
    return {
        "linear": LinearRegression(),
        "poly2": make_pipeline(PolynomialFeatures(2, include_bias=False), LinearRegression()),
        "log-linear": LinearRegression(),
        "power": LinearRegression(),
    }


def fit_and_score(model_name: str, model, X: np.ndarray, y: np.ndarray) -> Tuple[float, object]:
    if model_name in ("linear", "poly2"):
        model.fit(X, y)
        y_pred = model.predict(X)
    elif model_name == "log-linear":
        if np.any(y <= 0) or np.any(X <= 0):
            return -1e9, model
        model.fit(np.log(X), y)
        y_pred = model.predict(np.log(X))
    elif model_name == "power":
        if np.any(y <= 0) or np.any(X <= 0):
            return -1e9, model
        model.fit(np.log(X), np.log(y))
        y_pred = np.exp(model.predict(np.log(X)))
    else:
        raise ValueError(model_name)
    ss_res = float(np.sum((y - y_pred) ** 2))
    ss_tot = float(np.sum((y - y.mean()) ** 2))
    r2 = 1 - ss_res / ss_tot if ss_tot else 0.0
    return r2, model


def predict(model_name: str, model, X: np.ndarray) -> np.ndarray:
    if model_name in ("linear", "poly2"):
        return model.predict(X)
    if model_name == "log-linear":
        return model.predict(np.log(X))
    if model_name == "power":
        return np.exp(model.predict(np.log(X)))
    raise ValueError(model_name)


def main() -> None:
    if not STATS_PATH.exists():
        raise SystemExit("stats.txt not found")
    runs = parse_runs(STATS_PATH)
    if not runs:
        raise SystemExit("No data found")

    if TARGET_FILE.exists():
        target_words = int(TARGET_FILE.read_text().split().__len__())
    else:
        target_words = max(w for pts in runs.values() for w, _ in pts)

    model_defs = build_models()
    usable_gens = {g: pts for g, pts in runs.items() if len(pts) >= 2}
    if not usable_gens:
        raise SystemExit("Not enough points to fit any generation")

    # choose best model by mean R^2 across gens with >=2 points
    model_scores = {}
    fitted_models: Dict[int, Tuple[str, object, float]] = {}
    for name, model in model_defs.items():
        r2_list = []
        for pts in usable_gens.values():
            X = np.array([w for w, _ in pts], dtype=float)[:, None]
            y = np.array([d for _, d in pts], dtype=float)
            r2, _ = fit_and_score(name, model, X, y)
            r2_list.append(r2)
        if r2_list:
            model_scores[name] = float(np.mean(r2_list))
    best_model_name = max(model_scores, key=model_scores.get)

    report_lines = []
    total_pred = 0.0
    for gen, pts in sorted(runs.items()):
        X = np.array([w for w, _ in pts], dtype=float)[:, None]
        y = np.array([d for _, d in pts], dtype=float)
        r2, fitted = fit_and_score(best_model_name, model_defs[best_model_name], X, y)
        pred = float(predict(best_model_name, fitted, np.array([[target_words]], dtype=float))[0])
        total_pred += pred
        report_lines.append(
            f"gen {gen}: R2={r2:.4f}, pred_at_{target_words}w={pred:.3f}s, points={len(pts)}"
        )

    report_lines.append(f"total_pred_seconds: {total_pred:.3f}")
    report_lines.append(f"total_pred_hours: {total_pred/3600:.3f}")
    report_lines.append(f"model_used: {best_model_name} (avg R2={model_scores[best_model_name]:.4f})")

    out_path = pathlib.Path("charts") / "per_gen_fit.txt"
    out_path.parent.mkdir(exist_ok=True)
    out_path.write_text("\n".join(report_lines))
    print(f"Wrote {out_path}")


if __name__ == "__main__":
    main()
