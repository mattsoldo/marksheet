# Marksheet extension conformance fixtures

This corpus specifies Milestone 5 extension-host behavior. Registry entries are
exact `id@major` values. Duplicate exact registrations are host configuration
errors, while distinct major versions are independent capabilities.

The `assertions@1` cases exercise success, failure, malformed payloads, bounded
work, and workbook-versus-sheet scope. The availability cases ensure an absent
optional declaration reports `MS3102`, an absent required declaration reports
`MS3101` and makes calculation/rendering incomplete, and an undeclared opaque
instance reports `MS3103` while remaining preserved. `opaque_crlf.json` embeds
an original CRLF source byte sequence: lossless output is byte-identical;
canonical output uses LF while preserving all other payload bytes.

Each non-registry fixture records every public completeness flag, validity,
ordered diagnostics, and ordered per-instance outcomes. Registry construction
errors are separate host-configuration cases and therefore record only the
expected registry error. This keeps the executable conformance runner fully
data-driven.

Run `./tests/extensions/validate.sh` to validate the fixture contracts. These
validators intentionally do not execute an extension. They ensure fixtures are
well-formed, bounded, source-safe, and internally consistent so implementations
can consume the same contracts independently.

`cargo test -p marksheet-extensions --test manifest_fixtures` discovers every
manifest case and executes it through the real extension registry. It also
checks configured limits and the lossless/canonical byte expectations.
