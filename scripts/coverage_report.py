#!/usr/bin/env python3
import argparse
import json
import sys
from pathlib import Path


def read_coverage(path):
    report = json.loads(Path(path).read_text())
    coverage = {}
    for unit in report.get("data", []):
        for source in unit.get("files", []):
            parts = Path(source["filename"]).parts
            if "src" not in parts:
                continue
            src_index = len(parts) - 1 - parts[::-1].index("src")
            if src_index == len(parts) - 1:
                continue
            module = "src/" + "/".join(parts[src_index + 1 :])
            lines = source["summary"]["lines"]
            coverage[module] = (lines["covered"], lines["count"])
    return coverage


def percentage(coverage, module):
    covered, total = coverage.get(module, (0, 0))
    return (covered * 100.0 / total) if total else 100.0


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("current")
    parser.add_argument("--base")
    parser.add_argument("--comment")
    args = parser.parse_args()

    current = read_coverage(args.current)
    base = read_coverage(args.base) if args.base else {}
    thresholds = json.loads(Path("coverage-thresholds.json").read_text())
    missing = sorted(set(thresholds) - set(current))
    failed = [
        (module, percentage(current, module), threshold)
        for module, threshold in thresholds.items()
        if percentage(current, module) < threshold
    ]

    has_base = bool(args.base)
    heading = "| Module | Coverage | Base | Change | Required |" if has_base else "| Module | Coverage | Required |"
    separator = "|---|---:|---:|---:|---:|" if has_base else "|---|---:|---:|"
    lines = ["<!-- coverage-diff -->", "### Coverage report", "", heading, separator]
    for module, threshold in thresholds.items():
        current_pct = percentage(current, module)
        if has_base:
            base_pct = percentage(base, module) if module in base else 0.0
            change = current_pct - base_pct
            lines.append(f"| `{module}` | {current_pct:.1f}% | {base_pct:.1f}% | {change:+.1f} pp | {threshold}% |")
        else:
            lines.append(f"| `{module}` | {current_pct:.1f}% | {threshold}% |")
    if has_base:
        total_covered = sum(pair[0] for pair in current.values())
        total_lines = sum(pair[1] for pair in current.values())
        base_covered = sum(pair[0] for pair in base.values())
        base_lines = sum(pair[1] for pair in base.values())
        now_pct = total_covered * 100.0 / total_lines if total_lines else 100.0
        base_pct = base_covered * 100.0 / base_lines if base_lines else 100.0
        delta = now_pct - base_pct
        lines.extend(["", f"Overall line coverage: **{now_pct:.1f}%** ({delta:+.1f} percentage points vs base)."])
    if missing:
        lines.extend(["", "Modules missing from the coverage report: " + ", ".join(f"`{m}`" for m in missing)])

    if args.comment:
        Path(args.comment).write_text("\n".join(lines) + "\n")
    if missing or failed:
        for module in missing:
            print(f"coverage report missing required module: {module}", file=sys.stderr)
        for module, actual, threshold in failed:
            print(f"{module}: {actual:.1f}% is below the {threshold}% threshold", file=sys.stderr)
        return 1
    print("\n".join(lines))
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
