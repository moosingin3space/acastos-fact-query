#!/usr/bin/env python3
"""Render a Markdown diff of a criterion benchmark run against a saved baseline.

Reads criterion's own JSON output (``target/criterion/**/estimates.json``) — the
``new/`` directory (this run) and the named baseline directory (e.g. ``main/``,
written by ``--save-baseline main``) — and prints a sticky PR-comment body to
stdout. Dependency-free: standard library only, so it runs on ubuntu-latest's
stock ``python3``.

Usage:
    criterion_compare.py --baseline main [--criterion-dir target/criterion]
                         [--noise 10.0]

If a benchmark has no baseline measurement, it is reported as new (no
comparison). If *no* benchmark has a baseline, the body says so instead of
showing an empty table; the caller need not special-case the first run.
"""

from __future__ import annotations

import argparse
import json
import os
import re
import sys
from dataclasses import dataclass

# Kept in sync with the marker the workflow greps for to find its sticky comment.
MARKER = "<!-- criterion-bench -->"


@dataclass
class Row:
    """One benchmark: its full id and baseline/current point estimates (ns)."""

    full_id: str
    group_id: str
    base_ns: float | None
    new_ns: float


def _point_estimate(estimates_path: str) -> float | None:
    """Return criterion's headline time (ns): the slope if linear-sampled, else the mean."""
    try:
        with open(estimates_path, encoding="utf-8") as handle:
            data = json.load(handle)
    except (OSError, ValueError):
        return None
    slope = data.get("slope")
    chosen = slope if slope else data.get("mean")
    if not chosen:
        return None
    return chosen.get("point_estimate")


def _fmt_time(ns: float) -> str:
    """Format a nanosecond duration with a human-friendly unit, matching criterion's scale."""
    for unit, scale in (("s", 1e9), ("ms", 1e6), ("µs", 1e3), ("ns", 1.0)):
        if ns >= scale:
            return f"{ns / scale:.3g} {unit}"
    return f"{ns:.3g} ns"


def collect_rows(criterion_dir: str, baseline: str) -> list[Row]:
    """Walk the criterion tree and pair each benchmark's baseline and current estimates.

    A benchmark leaf is a directory whose ``new/`` subdir holds this run's
    ``estimates.json`` (and ``benchmark.json`` metadata); the baseline lives in a
    sibling directory named after the saved baseline (e.g. ``main/``).
    """
    rows: list[Row] = []
    for dirpath, _dirnames, filenames in os.walk(criterion_dir):
        if os.path.basename(dirpath) != "new" or "estimates.json" not in filenames:
            continue
        leaf = os.path.dirname(dirpath)
        new_ns = _point_estimate(os.path.join(dirpath, "estimates.json"))
        if new_ns is None:
            continue
        try:
            with open(os.path.join(dirpath, "benchmark.json"), encoding="utf-8") as handle:
                meta = json.load(handle)
        except (OSError, ValueError):
            meta = {}
        base_ns = _point_estimate(os.path.join(leaf, baseline, "estimates.json"))
        rows.append(
            Row(
                full_id=meta.get("full_id", leaf),
                group_id=meta.get("group_id", ""),
                base_ns=base_ns,
                new_ns=new_ns,
            )
        )
    rows.sort(key=lambda r: _natural_key(r.full_id))
    return rows


def _natural_key(text: str) -> list:
    """Sort key that orders embedded numbers numerically (so ``/8`` precedes ``/64``)."""
    return [int(part) if part.isdigit() else part for part in re.split(r"(\d+)", text)]


def _change_cell(base_ns: float | None, new_ns: float, noise: float) -> str:
    """Percent change from baseline to current, tagged slower/faster/noise."""
    if base_ns is None or base_ns == 0:
        return "— (new)"
    pct = (new_ns - base_ns) / base_ns * 100.0
    sign = "+" if pct >= 0 else ""
    if abs(pct) < noise:
        tag = "noise"
    elif pct > 0:
        tag = "🔴 slower"
    else:
        tag = "🟢 faster"
    return f"{sign}{pct:.1f}% {tag}"


def render(rows: list[Row], baseline: str, noise: float) -> str:
    """Build the full sticky-comment Markdown body (marker included)."""
    lines = [MARKER, "## Benchmark results", ""]
    have_baseline = any(r.base_ns is not None for r in rows)

    if not rows:
        lines.append("_No benchmark results were produced._")
        return "\n".join(lines) + "\n"

    if not have_baseline:
        lines.append(
            f"_No `{baseline}` baseline was available, so this run has nothing to "
            "compare against. Current timings only._"
        )
        lines.append("")

    # One table per criterion group, in id order.
    groups: dict[str, list[Row]] = {}
    for row in rows:
        groups.setdefault(row.group_id, []).append(row)

    for group_id in sorted(groups):
        lines.append(f"### `{group_id}`")
        lines.append("")
        lines.append("| Benchmark | Baseline | Current | Change |")
        lines.append("| --- | ---: | ---: | :--- |")
        for row in groups[group_id]:
            base_cell = _fmt_time(row.base_ns) if row.base_ns is not None else "—"
            lines.append(
                f"| `{row.full_id}` | {base_cell} | {_fmt_time(row.new_ns)} | "
                f"{_change_cell(row.base_ns, row.new_ns, noise)} |"
            )
        lines.append("")

    lines.append(
        f"_CI runners are noisy; treat changes under ~{noise:.0f}% as noise, not signal._"
    )
    return "\n".join(lines) + "\n"


def main(argv: list[str]) -> int:
    """Parse arguments, collect estimates, and print the comment body to stdout."""
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--baseline", default="main", help="saved baseline name (directory)")
    parser.add_argument(
        "--criterion-dir",
        default="target/criterion",
        help="path to the criterion output tree",
    )
    parser.add_argument(
        "--noise",
        type=float,
        default=10.0,
        help="percent change below which a delta is treated as noise",
    )
    args = parser.parse_args(argv)

    rows = collect_rows(args.criterion_dir, args.baseline)
    sys.stdout.write(render(rows, args.baseline, args.noise))
    return 0


if __name__ == "__main__":
    raise SystemExit(main(sys.argv[1:]))
