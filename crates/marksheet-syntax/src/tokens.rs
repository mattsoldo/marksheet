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
    let mut cursor = 0;
    let mut tokens = Vec::new();
    while let Some(token) = next_token(arguments, cursor)? {
        cursor = token.end();
        tokens.push(token);
    }
    Ok(tokens)
}

/// Splits a leading target from the arguments that follow it, so a space inside
/// a structured-reference header stays part of the target.
///
/// Scanning stops at the end of the target. Everything after it is returned as
/// opaque text, so a caller such as `@fill` — whose remainder is a formula, not
/// a token list — never turns a malformed formula into a tokenizer rejection.
pub(crate) fn split_target_and_rest(arguments: &str) -> Option<(&str, &str)> {
    let target = next_token(arguments, 0).ok()??;
    let rest = arguments.get(target.end()..)?.trim_start();
    (!rest.is_empty()).then_some((target.text, rest))
}

/// Scans the one argument that starts at or after `cursor`, returning `None`
/// once only separating spaces remain.
fn next_token(arguments: &str, cursor: usize) -> Result<Option<RawToken<'_>>, &'static str> {
    let bytes = arguments.as_bytes();
    let mut index = cursor;
    while index < bytes.len() && bytes[index] == b' ' {
        index += 1;
    }
    if index == bytes.len() {
        return Ok(None);
    }
    let start = index;
    let mut in_string = false;
    let mut escaped = false;
    let mut in_brackets = false;
    while index < bytes.len() {
        match bytes[index] {
            b'"' if !escaped => in_string = !in_string,
            b'\\' if in_string => escaped = !escaped,
            b'[' if !in_string => in_brackets = true,
            b']' if !in_string && in_brackets && bytes.get(index + 1) == Some(&b']') => {
                index += 1;
            }
            b']' if !in_string => in_brackets = false,
            b' ' if !in_string && !in_brackets => break,
            _ => escaped = false,
        }
        index += 1;
    }
    if in_string {
        return Err("unterminated JSON string");
    }
    Ok(Some(RawToken {
        text: &arguments[start..index],
        start,
    }))
}
