# Marksheet round-trip fixtures

`lossless_unknown_extension.ms` must be emitted byte-for-byte by a no-op
lossless save, including comments, blank lines, and extension payload
indentation. `crlf_input.ms` must preserve CRLF in a lossless no-op and produce
LF through canonical formatting. `canonical_mixed_input.ms` is intentionally noncanonical;
`canonical_mixed_input.canonical.ms` is the expected canonical LF output.
`quoted_end_multiline.ms` verifies that a quoted CSV field equal to `@end` and
multiline CSV do not terminate a block early.
