"""Regression tests for the independent byte-oriented conformance consumer."""

from __future__ import annotations

import json
import subprocess
import sys
import unittest
from pathlib import Path

from generate_projections import AVAILABLE_EXTENSIONS, MANIFEST, discover_sources, projection_name
from marksheet_projection import (
    MAX_CSV_FIELD_BYTES,
    MAX_CSV_FIELDS_PER_ROW,
    MAX_CSV_ROWS,
    MAX_DIAGNOSTICS,
    MAX_INPUT_BYTES,
    MAX_TOKEN_BYTES,
    dump_projection,
    project_bytes,
)


ROOT = Path(__file__).resolve().parents[2]


def codes(projection: dict) -> list[str]:
    return [diagnostic["code"] for diagnostic in projection["diagnostics"]]


class ExistingCorpusTests(unittest.TestCase):
    def test_valid_fixtures_have_no_errors_and_expected_warnings(self) -> None:
        for fixture in sorted((ROOT / "tests" / "conformance" / "valid").glob("*.ms")):
            with self.subTest(fixture=fixture.name):
                projection = project_bytes(fixture.read_bytes())
                self.assertFalse(
                    [item for item in projection["diagnostics"] if item["severity"] == "error"],
                    dump_projection(projection),
                )
                expected = fixture.with_suffix(".diagnostics").read_text(encoding="utf-8").split()
                self.assertEqual(codes(projection), expected)

    def test_invalid_fixtures_match_the_normative_diagnostic_sidecars_exactly(self) -> None:
        for fixture in sorted((ROOT / "tests" / "conformance" / "invalid").glob("*.ms")):
            with self.subTest(fixture=fixture.name):
                expected = fixture.with_suffix(".diagnostics").read_text(encoding="utf-8").split()
                actual = codes(project_bytes(fixture.read_bytes()))
                self.assertEqual(actual, expected)

    def test_roundtrip_sources_remain_structurally_parseable(self) -> None:
        for fixture in sorted((ROOT / "tests" / "roundtrip").glob("*.ms")):
            with self.subTest(fixture=fixture.name):
                projection = project_bytes(fixture.read_bytes())
                self.assertFalse([item for item in projection["diagnostics"] if item["severity"] == "error"])

    def test_checked_in_projections_match_generator(self) -> None:
        result = subprocess.run(
            [sys.executable, "conformance/python/generate_projections.py", "--check"],
            cwd=ROOT,
            text=True,
            capture_output=True,
            check=False,
        )
        self.assertEqual(result.returncode, 0, result.stderr)

    def test_manifest_is_a_bijection_over_the_full_declared_corpus(self) -> None:
        manifest = json.loads(MANIFEST.read_text(encoding="utf-8"))
        expected_sources = [source.relative_to(ROOT).as_posix() for source in discover_sources()]
        self.assertEqual([fixture["source"] for fixture in manifest["fixtures"]], expected_sources)
        self.assertEqual(manifest["available_extensions"], list(AVAILABLE_EXTENSIONS))
        self.assertEqual(
            [fixture["projection"] for fixture in manifest["fixtures"]],
            [projection_name(source) for source in discover_sources()],
        )
        self.assertEqual(len({fixture["projection"] for fixture in manifest["fixtures"]}), len(expected_sources))

    def test_invalid_recovery_keeps_only_semantically_lowered_objects(self) -> None:
        invalid = ROOT / "tests" / "conformance" / "invalid"
        bad_property = project_bytes((invalid / "bad_property.ms").read_bytes())
        self.assertEqual(
            bad_property["workbook"]["book"],
            {"span": [16, 49], "properties": {"locale": "en-US"}},
        )

        style = project_bytes((invalid / "invalid_style_geometry_scalar.ms").read_bytes())
        self.assertEqual(style["workbook"]["styles"][0]["properties"], {"number": "currency"})
        self.assertEqual([item["kind"] for item in style["workbook"]["sheets"][0]["items"]], ["block"])

        malformed = project_bytes((invalid / "malformed_csv.ms").read_bytes())
        recovered = malformed["workbook"]["sheets"][0]["items"][0]["rows"][0][0]
        self.assertEqual(recovered["source"]["raw"], '"unterminated\n@end\n')
        self.assertEqual(recovered["value"], {"kind": "text", "value": "unterminated\n@end\n"})

        missing_header = project_bytes((invalid / "missing_version.ms").read_bytes())
        self.assertEqual(missing_header["workbook"]["sheets"][0]["id"], "main")
        self.assertEqual(missing_header["workbook"]["sheets"][0]["span"], [0, 18])

        self.assertEqual(
            project_bytes((invalid / "nonrectangular_block.ms").read_bytes())["workbook"]["sheets"][0]["items"],
            [],
        )
        overlap = project_bytes((invalid / "overlap.ms").read_bytes())
        self.assertEqual(len(overlap["workbook"]["sheets"][0]["items"]), 1)
        self.assertEqual(
            project_bytes((invalid / "unresolved_name.ms").read_bytes())["workbook"]["names"], []
        )


class ByteLevelTests(unittest.TestCase):
    def test_bom_invalid_utf8_and_bare_cr_are_diagnosed(self) -> None:
        bom = project_bytes(b"\xef\xbb\xbf#!marksheet 0.1\n@sheet s \"S\"\n")
        self.assertIn("MS1001", codes(bom))
        invalid_utf8 = project_bytes(b"#!marksheet 0.1\n@sheet s \"\xff\"\n")
        self.assertIn("MS1001", codes(invalid_utf8))
        bare_cr = project_bytes(b"#!marksheet 0.1\r@sheet s \"S\"\n")
        self.assertIn("MS1101", codes(bare_cr))

    def test_csv_quoted_end_and_multiline_do_not_terminate_the_block(self) -> None:
        data = (
            b'#!marksheet 0.1\r\n@sheet s "S"\r\n@block A1 csv\r\n'
            b'"@end","one\r\ntwo"\r\n@end\r\n'
        )
        projection = project_bytes(data)
        block = projection["workbook"]["sheets"][0]["items"][0]
        self.assertEqual(block["rows"][0][0]["value"], {"kind": "text", "value": "@end"})
        self.assertEqual(block["rows"][0][1]["value"], {"kind": "text", "value": "one\ntwo"})
        self.assertEqual(block["rows"][0][1]["source"]["raw"], '"one\r\ntwo"')
        self.assertEqual({line["ending"] for line in projection["source"]["physical_lines"]}, {"crlf"})

    def test_extension_payload_is_bounded_by_its_physical_terminator(self) -> None:
        data = b'#!marksheet 0.1\n@sheet s "S"\n@extension demo@1 "x"\nkey=value\n@end\n'
        projection = project_bytes(data)
        extension = projection["workbook"]["extensions"][0]
        self.assertEqual(extension["payload"]["byte_length"], len(b"key=value\n"))
        self.assertIn("MS3103", codes(projection))

    def test_extension_declaration_uniqueness_and_registry_policy(self) -> None:
        data = (
            b'#!marksheet 0.1\n@use charts@1\n@require charts@2\n@require risk@1\n'
            b'@sheet s "S"\n@extension chart@1 "orphan"\n@end\n'
        )
        projection = project_bytes(data)
        self.assertIn("MS1301", codes(projection))
        self.assertIn("MS3101", codes(projection))
        self.assertIn("MS3102", codes(projection))
        self.assertIn("MS3103", codes(projection))
        self.assertFalse(projection["completeness"]["calculation_complete"])
        supported = project_bytes(data, available_extensions=["charts@1", "risk@1"])
        self.assertNotIn("MS3101", codes(supported))
        self.assertNotIn("MS3102", codes(supported))

    def test_coordinate_bounds_and_authored_blank_are_distinct(self) -> None:
        data = b'#!marksheet 0.1\n@sheet s "S"\n@block ZZZZ1000000 csv\n,\n@end\n'
        projection = project_bytes(data)
        block = projection["workbook"]["sheets"][0]["items"][0]
        self.assertEqual(block["anchor"], {"column": 475254, "row": 1000000})
        self.assertEqual(block["rows"][0][0]["value"], {"kind": "blank"})
        self.assertEqual(block["rows"][0][1]["value"], {"kind": "blank"})
        overflow = project_bytes(
            b'#!marksheet 0.1\n@sheet s "S"\n@block A18446744073709551616 csv\nx\n@end\n'
        )
        self.assertIn("MS1202", codes(overflow))

    def test_hostile_numeric_tokens_are_rejected_without_integer_conversion_failures(self) -> None:
        coordinate = (
            b'#!marksheet 0.1\n@sheet s "S"\n@block A'
            + b"9" * 5000
            + b" csv\nx\n@end\n"
        )
        self.assertEqual(codes(project_bytes(coordinate)), ["MS1202"])
        extension = b'#!marksheet 0.1\n@use x@' + b"9" * 5000 + b'\n@sheet s "S"\n'
        self.assertEqual(codes(project_bytes(extension)), ["MS1101"])
        geometry = b'#!marksheet 0.1\n@sheet s "S"\n@column ' + b"A" * 5000 + b" width=1\n"
        self.assertEqual(codes(project_bytes(geometry)), ["MS1202"])

    def test_explicit_input_token_csv_and_diagnostic_caps_are_visible(self) -> None:
        oversized_input = b"x" * (MAX_INPUT_BYTES + 1)
        self.assertEqual(codes(project_bytes(oversized_input)), ["MS1101"])

        oversized_token = b'#!marksheet 0.1\n@sheet s "' + b"x" * (MAX_TOKEN_BYTES + 1) + b'"\n'
        self.assertEqual(codes(project_bytes(oversized_token)), ["MS1101"])

        oversized_field = b'#!marksheet 0.1\n@sheet s "S"\n@block A1 csv\n' + b"x" * (MAX_CSV_FIELD_BYTES + 1) + b"\n@end\n"
        self.assertIn("MS1102", codes(project_bytes(oversized_field)))

        oversized_field_count = (
            b'#!marksheet 0.1\n@sheet s "S"\n@block A1 csv\n'
            + b"," * MAX_CSV_FIELDS_PER_ROW
            + b"\n@end\n"
        )
        self.assertIn("MS1102", codes(project_bytes(oversized_field_count)))

        oversized_rows = b'#!marksheet 0.1\n@sheet s "S"\n@block A1 csv\n' + b"x\n" * (MAX_CSV_ROWS + 1) + b"@end\n"
        self.assertIn("MS1102", codes(project_bytes(oversized_rows)))

        noisy = b"#!marksheet 0.1" + b"\r" * (MAX_DIAGNOSTICS + 64)
        diagnostics = project_bytes(noisy)["diagnostics"]
        self.assertEqual(len(diagnostics), MAX_DIAGNOSTICS)
        self.assertEqual(diagnostics[-1]["code"], "MS1101")

    def test_spans_are_utf8_byte_offsets_not_character_offsets(self) -> None:
        data = '#!marksheet 0.1\n@sheet s "S"\n@block A1 csv\né\n@end\n'.encode("utf-8")
        projection = project_bytes(data)
        field = projection["workbook"]["sheets"][0]["items"][0]["rows"][0][0]
        self.assertEqual(field["source"]["raw"], "é")
        self.assertEqual(field["source"]["span"], [len(data) - len(b"\xc3\xa9\n@end\n"), len(data) - len(b"\n@end\n")])

    def test_projection_is_json_deterministic(self) -> None:
        data = (ROOT / "tests" / "conformance" / "valid" / "all_core.ms").read_bytes()
        self.assertEqual(dump_projection(project_bytes(data)), dump_projection(project_bytes(data)))
        json.loads(dump_projection(project_bytes(data)))


if __name__ == "__main__":
    unittest.main()
