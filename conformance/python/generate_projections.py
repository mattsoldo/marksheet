#!/usr/bin/env python3
"""Generate or verify the complete independent Marksheet projection corpus."""

from __future__ import annotations

import argparse
import json
import sys
from pathlib import Path

from marksheet_projection import dump_projection, project_bytes


ROOT = Path(__file__).resolve().parents[2]
OUTPUT = ROOT / "tests" / "conformance" / "projections"
MANIFEST = OUTPUT / "manifest.json"
# Keep this list narrow and explicit: it is the public parity contract between
# the independently implemented Python consumer and the Rust reference test.
CORPUS_ROOTS = (
    "tests/conformance/valid",
    "tests/conformance/invalid",
    "tests/roundtrip",
    "tests/extensions",
    "tests/conversion/sources",
)
MANIFEST_SCHEMA = "marksheet.conformance-projection-manifest@1"
# The independent structural parser deliberately uses an empty, host-owned
# registry. This makes MS3101/MS3102/MS3103 availability deterministic without
# smuggling extension implementation behavior into the projection corpus.
AVAILABLE_EXTENSIONS: tuple[str, ...] = ()


def discover_sources() -> list[Path]:
    """Discover every Marksheet source in the declared parity roots."""

    sources: list[Path] = []
    for relative_root in CORPUS_ROOTS:
        root = ROOT / relative_root
        if not root.is_dir():
            raise ValueError(f"missing corpus root: {relative_root}")
        sources.extend(path for path in root.rglob("*.ms") if path.is_file())
    sources.sort(key=lambda path: path.relative_to(ROOT).as_posix())
    stems: dict[str, Path] = {}
    for source in sources:
        stem = source.stem
        previous = stems.setdefault(stem, source)
        if previous != source:
            raise ValueError(
                "duplicate projection stem "
                f"{stem!r}: {previous.relative_to(ROOT)} and {source.relative_to(ROOT)}"
            )
    return sources


def projection_name(source: Path) -> str:
    return f"{source.stem}.json"


def manifest_text(sources: list[Path]) -> str:
    payload = {
        "schema": MANIFEST_SCHEMA,
        "available_extensions": list(AVAILABLE_EXTENSIONS),
        "fixtures": [
            {
                "id": source.relative_to(ROOT).with_suffix("").as_posix(),
                "source": source.relative_to(ROOT).as_posix(),
                "projection": projection_name(source),
            }
            for source in sources
        ],
    }
    return json.dumps(payload, ensure_ascii=False, indent=2, sort_keys=True) + "\n"


def expected_outputs() -> dict[Path, str]:
    sources = discover_sources()
    outputs = {
        OUTPUT / projection_name(source): dump_projection(project_bytes(source.read_bytes(), AVAILABLE_EXTENSIONS))
        for source in sources
    }
    outputs[MANIFEST] = manifest_text(sources)
    return outputs


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--check", action="store_true", help="fail when checked-in output is stale or incomplete")
    args = parser.parse_args(argv)
    try:
        expected = expected_outputs()
    except ValueError as error:
        print(f"invalid projection corpus: {error}", file=sys.stderr)
        return 1
    OUTPUT.mkdir(parents=True, exist_ok=True)
    if not args.check:
        for target, text in expected.items():
            target.write_text(text, encoding="utf-8", newline="\n")
        return 0

    actual = set(OUTPUT.glob("*.json"))
    expected_paths = set(expected)
    stale = [
        target
        for target in sorted(expected_paths, key=lambda path: path.name)
        if not target.is_file() or target.read_text(encoding="utf-8") != expected[target]
    ]
    unexpected = sorted(actual - expected_paths, key=lambda path: path.name)
    for target in stale:
        print(f"stale projection: {target.relative_to(ROOT)}", file=sys.stderr)
    for target in unexpected:
        print(f"unexpected projection: {target.relative_to(ROOT)}", file=sys.stderr)
    return int(bool(stale or unexpected))


if __name__ == "__main__":
    raise SystemExit(main())
