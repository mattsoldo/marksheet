//! Shared tokenization of directive arguments.
//!
//! Lowering and canonical formatting must agree about where an argument ends,
//! so both consume this scanner instead of repeating its quote, bracket, and
//! `]]` escape rules.

/// One raw argument slice together with its byte offset inside the arguments.
#[derive(Clone, Copy, Debug)]
pub(crate) struct RawToken<'a> {
    pub text: &'a str,
    pub start: usize,
}

impl RawToken<'_> {
    pub fn end(self) -> usize {
        self.start + self.text.len()
    }
}

/// Splits arguments on the spaces that separate them, keeping JSON strings and
/// structured references containing spaces as single tokens. A `]` inside a
/// structured-reference header is escaped as `]]` and does not close it.
///
/// # Errors
///
/// Returns a message when a JSON string is left unterminated.
pub(crate) fn split_tokens(arguments: &str) -> Result<Vec<RawToken<'_>>, &'static str> {
    let bytes = arguments.as_bytes();
    let mut cursor = 0;
    let mut tokens = Vec::new();
    while cursor < bytes.len() {
        while cursor < bytes.len() && bytes[cursor] == b' ' {
            cursor += 1;
        }
        if cursor == bytes.len() {
            break;
        }
        let start = cursor;
        let mut in_string = false;
        let mut escaped = false;
        let mut in_brackets = false;
        while cursor < bytes.len() {
            match bytes[cursor] {
                b'"' if !escaped => in_string = !in_string,
                b'\\' if in_string => escaped = !escaped,
                b'[' if !in_string => in_brackets = true,
                b']' if !in_string && in_brackets && bytes.get(cursor + 1) == Some(&b']') => {
                    cursor += 1;
                }
                b']' if !in_string => in_brackets = false,
                b' ' if !in_string && !in_brackets => break,
                _ => escaped = false,
            }
            cursor += 1;
        }
        if in_string {
            return Err("unterminated JSON string");
        }
        tokens.push(RawToken {
            text: &arguments[start..cursor],
            start,
        });
    }
    Ok(tokens)
}

/// Splits a leading target from the arguments that follow it, so a space inside
/// a structured-reference header stays part of the target.
pub(crate) fn split_target_and_rest(arguments: &str) -> Option<(&str, &str)> {
    let target = *split_tokens(arguments).ok()?.first()?;
    let rest = arguments.get(target.end()..)?.trim_start();
    (!rest.is_empty()).then_some((target.text, rest))
}
