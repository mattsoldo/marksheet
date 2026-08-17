# Marksheet agent guidance

Read [PRODUCT.md](PRODUCT.md), [SPEC.md](SPEC.md), and
[IMPLEMENTATION.md](IMPLEMENTATION.md) before making changes that affect the
format, semantics, or interoperability.

## Pull request review

Review the pull request's stated intent against its diff and the existing
specifications. Report only actionable regressions introduced by the change.

Prioritize:

- Rust correctness, error handling, and public API compatibility;
- Marksheet `.ms` format and serialization compatibility;
- focused fixtures or tests for changed behavior;
- Wasm ABI and protocol compatibility; and
- browser viewer behavior and test coverage.

Do not request unrelated refactors. Treat the required CI checks as
authoritative for formatting, linting, and test results.

## Contributions

Keep changes focused. When behavior changes, update the relevant
specification and add focused tests or fixtures where practical. Pull request
descriptions must disclose AI agents and oppositional reviews as required by
[CONTRIBUTING.md](CONTRIBUTING.md).
