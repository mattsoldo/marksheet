# Contributing to Marksheet

Thanks for helping shape Marksheet. It is a public Draft 0.1 project: the
format and reference implementation are actively evolving, and compatibility
is not guaranteed before 1.0.

## Before you start

Read the [product](PRODUCT.md), [format](SPEC.md), and
[implementation](IMPLEMENTATION.md) specifications. For proposals that change
syntax, semantics, or interoperability, please open an issue first so the
trade-offs can be discussed before implementation work begins.

Keep pull requests focused. Include tests for behavior changes, update the
relevant specification when a format decision changes, and avoid mixing
unrelated refactors with a proposal or bug fix.

## Development checks

Use Rust 1.85 or a newer compatible stable toolchain. From the repository root,
run:

```sh
cargo fmt --check
cargo test --workspace
cargo clippy --workspace --all-targets --all-features -- -D warnings
RUSTDOCFLAGS="-D warnings" cargo doc --workspace --no-deps
cargo build --workspace --release --all-features
```

Please run the checks relevant to your change before opening a pull request.
The CI workflow runs the complete set.

## Pull requests

Describe the problem, the approach, and how you verified the result. New or
changed `.ms` behavior should include a focused fixture or test when practical.
By submitting a contribution, you agree that it may be distributed under this
repository's [MIT License](LICENSE).

## Community expectations

Be constructive, specific, and respectful. Discuss ideas on their technical
merits, welcome good-faith questions, and assume collaborators are working
toward a useful, interoperable format.
