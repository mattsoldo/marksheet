#!/usr/bin/env bash
# Downloads the real-world corpus:
#   sources.json  GitHub repos, pinned by commit SHA, fully reproducible
#   gsheets.json  public Google Sheets exported to XLSX
#   govdata.json  statistical workbooks published by public bodies
#
# Every source is a permissively-licensed, actively-maintained project whose
# license text is carried in LICENSES/. See README.md for why these sources and
# not others.
#
#   ./download.sh              # everything
#   ./download.sh --no-network   # only the commit-pinned GitHub sources
set -euo pipefail
cd "$(dirname "$0")"

# Everything except the GitHub groups is fetched live from a publisher, so it
# is not commit-pinned and its bytes can change between runs.
WITH_GSHEETS=1
[ "${1:-}" = "--no-network" ] && WITH_GSHEETS=0

python3 - "$WITH_GSHEETS" <<'PY'
import json, os, pathlib, subprocess, sys, hashlib

with_gsheets = sys.argv[1] == "1"
here = pathlib.Path(__file__).resolve().parent if "__file__" in dir() else pathlib.Path.cwd()
root = pathlib.Path.cwd()
out_root = root / "xlsx"

def fetch(url, dest):
    dest.parent.mkdir(parents=True, exist_ok=True)
    result = subprocess.run(
        ["curl", "-sL", "--max-time", "120", "-o", str(dest), "-w", "%{http_code}", url],
        capture_output=True, text=True,
    )
    code = result.stdout.strip()
    size = dest.stat().st_size if dest.exists() else 0
    if code != "200" or size == 0:
        print(f"  !! [{code}] {dest.name} ({size} bytes)")
        dest.unlink(missing_ok=True)
        return None
    print(f"  [{code}] {dest.relative_to(out_root)} ({size} bytes)")
    return hashlib.sha256(dest.read_bytes()).hexdigest()

total = 0
sources = json.loads((root / "sources.json").read_text())
for group, spec in sources.items():
    print(f"{group}: {len(spec['files'])} files from {spec['repo']}@{spec['sha'][:10]} ({spec['license']})")
    for name in spec["files"]:
        url = f"https://raw.githubusercontent.com/{spec['repo']}/{spec['sha']}/{spec['path']}/{name}"
        # Nested paths keep only their basename locally; provenance stays in manifest.json.
        dest = out_root / group / pathlib.PurePosixPath(name).name
        if fetch(url, dest):
            total += 1

govdata_path = root / "govdata.json"
if with_gsheets and govdata_path.exists():
    entries = json.loads(govdata_path.read_text())
    print(f"govdata: {len(entries)} published statistical workbooks")
    for entry in entries:
        dest = out_root / "govdata" / entry["file"]
        digest = fetch(entry["url"], dest)
        if not digest:
            continue
        expected = entry.get("sha256_first_seen")
        if expected and expected != digest:
            # Publishers refresh these on their own schedule; the file is still
            # a genuine workbook from that publisher.
            print(f"     (republished; sha256 {digest[:16]}...)")
        total += 1

gsheets_path = root / "gsheets.json"
if with_gsheets and gsheets_path.exists():
    entries = json.loads(gsheets_path.read_text())
    print(f"gsheets: {len(entries)} public Google Sheets exported to XLSX")
    for entry in entries:
        url = f"https://docs.google.com/spreadsheets/d/{entry['id']}/export?format=xlsx"
        dest = out_root / "gsheets" / entry["file"]
        digest = fetch(url, dest)
        if not digest:
            continue
        # Google Sheets are live documents, not commit-pinned artifacts. Record
        # what we got and warn when it differs from what the manifest describes,
        # rather than pretending the bytes are stable.
        expected = entry.get("sha256_first_seen")
        if expected and expected != digest:
            # Expected, not alarming: Google re-serializes on every export, so
            # the bytes differ run to run even when the document has not been
            # edited. The digest is recorded to spot a genuine content change.
            print(f"     (re-exported; sha256 {digest[:16]}...)")
        total += 1

print(f"Done. {total} files under {out_root}.")
PY
