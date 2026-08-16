#!/usr/bin/env bash
set -euo pipefail

corpus_dir=$(CDPATH='' cd -- "$(dirname -- "$0")" && pwd)

mapfile -d '' json_files < <(find "$corpus_dir" -type f -name '*.json' ! -name 'schema.json' -print0 | sort -z)
if ((${#json_files[@]} == 0)); then
  echo "formula corpus contains no JSON fixtures" >&2
  exit 1
fi

jq empty "${json_files[@]}" >/dev/null

for fixture in "${json_files[@]}"; do
  jq -e '
    def only_keys($allowed): ((keys - $allowed) | length) == 0;
    def valid_formula_expect:
      type == "object" and
      (if (keys | sort) == ["ast"] then (.ast | type == "string")
       elif (keys | sort) == ["diagnostic"] then .diagnostic == "MS2202"
       elif (keys | sort) == ["value"] then true
       elif (keys | sort) == ["canonical"] then
         (.canonical | type == "string" and startswith("="))
       else false
       end);
    def valid_formula_case:
      type == "object" and
      only_keys(["id", "description", "formula", "sheet", "cell", "cells", "expect"]) and
      (.id | test("^[a-z][a-z0-9_.-]*$")) and
      (.formula | type == "string" and startswith("=")) and
      (.expect | valid_formula_expect);
    (.schema == "marksheet.formula-conformance@1" and
      .profile == "portable-a1@1" and
      (keys | sort) == ["cases", "profile", "schema"] and
      (.cases | type == "array" and length > 0) and
      all(.cases[]; valid_formula_case)) or
    (.schema == "marksheet.calculation-scenario@1" and
      .profile == "portable-a1@1" and
      (.source | endswith(".ms")) and
      (.expect.cells | type == "object" and length > 0) and
      (.expect.diagnostics | type == "array"))
  ' "$fixture" >/dev/null
done

duplicate_ids=$(
  jq -rs '
    [ .[] | select(.schema == "marksheet.formula-conformance@1") | .cases[].id ]
    | group_by(.)
    | map(select(length > 1) | .[0])
    | .[]
  ' "${json_files[@]}"
)
if [[ -n "$duplicate_ids" ]]; then
  echo "duplicate formula case IDs:" >&2
  echo "$duplicate_ids" >&2
  exit 1
fi

jq -e '
  def valid_value:
    (.kind == "blank" and keys == ["kind"]) or
    (.kind == "number" and keys == ["kind", "value"] and (.value | type) == "number") or
    (.kind == "boolean" and keys == ["kind", "value"] and (.value | type) == "boolean") or
    ((.kind == "text" or .kind == "date" or .kind == "datetime" or .kind == "error")
      and keys == ["kind", "value"] and (.value | type) == "string");
  if .schema == "marksheet.formula-conformance@1" then
    all(.cases[];
      (all((.cells // {})[]; valid_value)) and
      (if .expect.value then (.expect.value | valid_value) else true end))
  else
    all(.expect.cells[]; valid_value)
  end
' "${json_files[@]}" >/dev/null

while IFS= read -r sidecar; do
  source_name=$(jq -r '.source' "$sidecar")
  if [[ ! -f "$(dirname -- "$sidecar")/$source_name" ]]; then
    echo "missing scenario source $source_name for $sidecar" >&2
    exit 1
  fi
done < <(find "$corpus_dir/scenarios" -type f -name '*.calc.json' | sort)

echo "validated ${#json_files[@]} formula corpus documents"
