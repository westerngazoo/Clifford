//! # clifford-lexer
//!
//! Lexer for the Clifford language. Implements §1 (Lexical Structure) of
//! `docs/CLIFFORD_SPEC.md`. Phase 0 of the implementation roadmap (§11).
//!
//! ## Responsibilities
//!
//! - UTF-8 source → stream of [`Token`]s with source spans.
//! - Recognise all bare keywords, sigil-prefixed forms (`#automaton`, `@fn`,
//!   `#unchecked_load`, …), composite sigils (`#>`, `:=`, `<op>=`), literals
//!   (integer / hex / binary / float / string / char / **byte** `b'X'` per
//!   Decision #19), and operators (§1.4).
//! - Track byte spans so every downstream phase can produce diagnostics with
//!   precise source locations (`codespan-reporting` / `ariadne` consumes these).
//! - Normalise line endings: `\r\n` → `\n` on read.
//!
//! ## Non-responsibilities
//!
//! - Grammar: that's `clifford-parser`.
//! - Sigil-layer enforcement: that's `clifford-check` (§5.5).
//! - Anything past producing tokens with spans.
//!
//! ## Determinism
//!
//! Per CLAUDE.md §6 Phase 0: the same input must produce the same token stream
//! byte-for-byte. No hash-map iteration order; no time-dependent behaviour.

#![forbid(unsafe_code)]
#![warn(missing_docs)]

use thiserror::Error;

/// A single byte-span in source, used by every token and AST node.
///
/// Half-open: `start..end`.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash)]
pub struct Span {
    /// File-relative byte offset of the first byte of the span (inclusive).
    pub start: usize,
    /// File-relative byte offset of the byte past the end of the span (exclusive).
    pub end: usize,
}

impl Span {
    /// Construct a new span. `start` must not exceed `end`.
    ///
    /// # Examples
    ///
    /// ```
    /// use clifford_lexer::Span;
    /// let s = Span::new(0, 5);
    /// assert_eq!(s.len(), 5);
    /// ```
    #[must_use]
    pub fn new(start: usize, end: usize) -> Self {
        debug_assert!(start <= end, "span start must not exceed end");
        Self { start, end }
    }

    /// The length of the span in bytes.
    #[must_use]
    pub fn len(self) -> usize {
        self.end - self.start
    }

    /// `true` if the span covers zero bytes.
    #[must_use]
    pub fn is_empty(self) -> bool {
        self.start == self.end
    }
}

/// A lexed token with its source span.
///
/// The `kind` carries the token's category (placeholder; the full enum is filled
/// in during Phase 0 implementation, not at scaffolding time).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Token {
    /// What kind of token this is.
    pub kind: TokenKind,
    /// Where in source this token appeared.
    pub span: Span,
}

/// Token category.
///
/// Phase 0 placeholder — the full set per §1.3 / §1.4 of `CLIFFORD_SPEC.md`
/// lands during lexer implementation, not scaffolding. Currently exposes only
/// `Eof` so the crate compiles and downstream crates can refer to the type.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum TokenKind {
    /// End-of-file marker.
    Eof,
}

/// Errors produced during lexing.
///
/// Per CLAUDE.md §3.4, every error carries a stable error code. The lexer
/// reserves the `E01xx` range (per the spec error-code conventions in §10).
#[derive(Debug, Error)]
pub enum LexError {
    /// Placeholder for Phase 0 scaffolding.
    #[error("E0100: lexer not yet implemented")]
    NotYetImplemented,
}

/// Tokenise a Clifford source string.
///
/// Returns the full token stream (including a trailing [`TokenKind::Eof`]) on
/// success, or the first lexical error encountered. Per §1.1 source files are
/// UTF-8; `\r\n` is normalised to `\n` at this boundary.
///
/// # Examples
///
/// ```
/// use clifford_lexer::tokenize;
/// // Empty input → just an EOF token.
/// let tokens = tokenize("").unwrap();
/// assert_eq!(tokens.len(), 1);
/// ```
///
/// # Errors
///
/// Returns the first [`LexError`] encountered. Phase 0 scaffolding always
/// succeeds with an empty (EOF-only) token stream.
pub fn tokenize(_input: &str) -> Result<Vec<Token>, LexError> {
    // Phase 0 scaffolding: real implementation lands per §1.
    Ok(vec![Token {
        kind: TokenKind::Eof,
        span: Span::new(0, 0),
    }])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_input_yields_eof() {
        let tokens = tokenize("").expect("empty input should not error");
        assert_eq!(tokens.len(), 1);
        assert_eq!(tokens[0].kind, TokenKind::Eof);
    }

    #[test]
    fn span_arithmetic() {
        let s = Span::new(3, 10);
        assert_eq!(s.len(), 7);
        assert!(!s.is_empty());

        let empty = Span::new(5, 5);
        assert!(empty.is_empty());
    }
}
