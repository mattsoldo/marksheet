#!/usr/bin/env python3
"""Strict pass/fail gate over the vendored real-world corpus.

Every file must match its recorded outcome in `expectations.json`, in both
directions: a regression fails, and an improvement fails too, with a message
saying which record to promote. That keeps the records honest instead of
silently drifting from behavior.

Per importable file this runs the round trip the corpus exists to prove:
import (`xlsx -> A.ms`), export (`A.ms -> B.xlsx`), re-import
(`B.xlsx -> B.ms`), then holds the file to its recorded stability class.

Usage: gate.py [path-to-marksheet-binary]
"""

from __future__ import annotations

import json
import subprocess
import sys
import tempfile
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
CORPUS = ROOT / "test-corpus"
TIMEOUT_SECONDS = 300


def convert(binary: str, source: Path, output: Path, target: str) -> bool:
    completed = subprocess.run(
        [binary, "convert", "--to", target, "--output", str(output), str(source)],
        capture_output=True,
        timeout=TIMEOUT_SECONDS,
    )
    return completed.returncode == 0


def main() -> int:
    binary = sys.argv[1] if len(sys.argv) > 1 else str(ROOT / "target/release/marksheet")
    if not Path(binary).is_file():
        print(f"no marksheet binary at {binary}; run: cargo build --release -p marksheet-cli")
        return 2
    expected = json.loads((CORPUS / "expectations.json").read_text())
    assert expected["version"] == "marksheet-corpus-expectations@1"
    classes: dict[str, str] = {}
    for name in expected["refused"]:
        classes[name] = "refused"
    for name in expected["known_gap_refused"]:
        classes[name] = "known_gap_refused"
    for outcome, names in expected["roundtrip"].items():
        for name in names:
            classes[name] = outcome

    on_disk = {
        str(path.relative_to(CORPUS))
        for pattern in ("*.xlsx", "*.xlsm")
        for path in (CORPUS / "real-world/xlsx").rglob(pattern)
    }
    failures: list[str] = []
    for name in sorted(on_disk - set(classes)):
        failures.append(f"{name}: on disk but not in expectations.json")
    for name in sorted(set(classes) - on_disk):
        failures.append(f"{name}: in expectations.json but not on disk")

    counts = {"refused": 0, "known_gap_refused": 0, "stable": 0, "converges": 0, "diverges": 0}
    for name in sorted(on_disk & set(classes)):
        outcome = classes[name]
        source = CORPUS / name
        with tempfile.TemporaryDirectory() as scratch_name:
            scratch = Path(scratch_name)
            imported = convert(binary, source, scratch / "a.ms", "marksheet")
            if outcome in ("refused", "known_gap_refused"):
                if imported:
                    verb = (
                        "the recorded gap closed; promote the record"
                        if outcome == "known_gap_refused"
                        else "a deliberately defective input imported"
                    )
                    failures.append(f"{name}: expected refusal but import succeeded ({verb})")
                else:
                    counts[outcome] += 1
                continue
            if not imported:
                failures.append(f"{name}: recorded {outcome} but import failed")
                continue
            if not convert(binary, scratch / "a.ms", scratch / "b.xlsx", "xlsx"):
                failures.append(f"{name}: export of imported workbook failed")
                continue
            if not convert(binary, scratch / "b.xlsx", scratch / "b.ms", "marksheet"):
                failures.append(f"{name}: re-import of exported workbook failed")
                continue
            a = (scratch / "a.ms").read_bytes()
            b = (scratch / "b.ms").read_bytes()
            if outcome == "stable":
                if a != b:
                    failures.append(f"{name}: recorded byte-stable but the round trip changed it")
                else:
                    counts["stable"] += 1
                continue
            if a == b:
                failures.append(
                    f"{name}: recorded {outcome} but is now byte-stable; promote the record"
                )
                continue
            if not convert(binary, scratch / "b.ms", scratch / "c.xlsx", "xlsx") or not convert(
                binary, scratch / "c.xlsx", scratch / "c.ms", "marksheet"
            ):
                failures.append(f"{name}: second-pass conversion failed")
                continue
            settled = b == (scratch / "c.ms").read_bytes()
            if outcome == "converges" and not settled:
                failures.append(f"{name}: recorded one-pass convergence but kept changing")
            elif outcome == "diverges" and settled:
                failures.append(f"{name}: recorded divergent but now converges; promote the record")
            else:
                counts[outcome] += 1

    total = sum(counts.values())
    print(
        f"corpus gate: {total} matched their records "
        f"({counts['stable']} stable, {counts['converges']} converge, "
        f"{counts['diverges']} diverge, {counts['refused']} refused, "
        f"{counts['known_gap_refused']} known gaps)"
    )
    if failures:
        print(f"\n{len(failures)} mismatches against expectations.json:")
        for failure in failures:
            print(f"  {failure}")
        return 1
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
