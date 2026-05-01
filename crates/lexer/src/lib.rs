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
//!
//! ## Implementation status
//!
//! First slice (this PR): whitespace, identifiers, bare keywords, integer
//! literals, single- and multi-char ASCII operators, one sigil-prefixed form
//! (`#automaton`) to validate the dispatch. Subsequent PRs add: full sigil
//! catalogue, hex/binary/float literals, string/char/byte literals with
//! escapes, comments (line + nested block + doc), and operator forms beyond
//! the basic set.

#![forbid(unsafe_code)]
#![warn(missing_docs)]

use thiserror::Error;

// ─── Spans ───────────────────────────────────────────────────────────────────

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

// ─── Tokens ──────────────────────────────────────────────────────────────────

/// A lexed token with its source span.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Token {
    /// What kind of token this is.
    pub kind: TokenKind,
    /// Where in source this token appeared.
    pub span: Span,
}

/// Token category.
///
/// First-slice subset of §1.3 / §1.4 of `CLIFFORD_SPEC.md`. Subsequent lexer
/// work extends this enum with the full sigil catalogue (e.g.,
/// `KwHashEffect`, `KwAtFn`, `KwHashUncheckedLoad`, …), the full literal
/// family (hex/binary/float/string/char/byte), and the full operator set.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum TokenKind {
    // ─ Identifiers and keywords ────────────────────────────────────────────
    /// A user-supplied identifier: `[a-zA-Z_][a-zA-Z0-9_]*`.
    Ident(String),

    // Bare keywords from §1.3 (subset for the first slice).
    /// `let`
    KwLet,
    /// `mut`
    KwMut,
    /// `const`
    KwConst,
    /// `static`
    KwStatic,
    /// `if`
    KwIf,
    /// `else`
    KwElse,
    /// `while`
    KwWhile,
    /// `loop`
    KwLoop,
    /// `for`
    KwFor,
    /// `in`
    KwIn,
    /// `match`
    KwMatch,
    /// `break`
    KwBreak,
    /// `continue`
    KwContinue,
    /// `return`
    KwReturn,
    /// `extern`
    KwExtern,
    /// `unsafe`
    KwUnsafe,
    /// `as`
    KwAs,
    /// `access`
    KwAccess,
    /// `null`
    KwNull,
    /// `self`
    KwSelf,
    /// `Self`
    KwSelfType,
    /// `true`
    KwTrue,
    /// `false`
    KwFalse,

    // ─ Sigil-prefixed forms (first slice: one each per layer for dispatch) ─
    /// `#automaton`
    KwHashAutomaton,
    /// `@fn`
    KwAtFn,

    // ─ Literals (first slice: integer only; full literal set in next PR) ──
    /// Decimal integer literal. Stores the raw textual digits for now;
    /// numeric value parsing and type-suffix handling land in the next PR.
    IntLiteral(String),

    // ─ Operators and punctuation (first slice: §1.4 ASCII subset) ──────────
    /// `(`
    LParen,
    /// `)`
    RParen,
    /// `{`
    LBrace,
    /// `}`
    RBrace,
    /// `[`
    LBracket,
    /// `]`
    RBracket,
    /// `,`
    Comma,
    /// `;`
    Semi,
    /// `:`
    Colon,
    /// `::` (path separator)
    ColonColon,
    /// `=`
    Eq,
    /// `==`
    EqEq,
    /// `!=`
    BangEq,
    /// `<`
    Lt,
    /// `<=`
    LtEq,
    /// `>`
    Gt,
    /// `>=`
    GtEq,
    /// `+`
    Plus,
    /// `-`
    Minus,
    /// `*`
    Star,
    /// `/`
    Slash,
    /// `%`
    Percent,
    /// `&`
    Amp,
    /// `&&`
    AmpAmp,
    /// `|`
    Pipe,
    /// `||`
    PipePipe,
    /// `^`
    Caret,
    /// `!`
    Bang,
    /// `~`
    Tilde,
    /// `.`
    Dot,
    /// `?`
    Question,
    /// `->` (arrow)
    Arrow,
    /// `=>` (fat arrow)
    FatArrow,
    /// `:=` short binding (Decision #8)
    ColonEq,

    /// End-of-file marker (always the final token).
    Eof,
}

// ─── Errors ──────────────────────────────────────────────────────────────────

/// Errors produced during lexing.
///
/// Per CLAUDE.md §3.4, every error carries a stable error code in the `E01xx`
/// range (per the spec error-code conventions in §10).
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum LexError {
    /// An unexpected byte was encountered that does not begin any token form.
    #[error("E0103: unexpected character '{ch}' at byte {at}")]
    UnexpectedChar {
        /// The offending character.
        ch: char,
        /// Byte offset where the character began.
        at: usize,
    },

    /// A `#` sigil was followed by an unrecognised identifier (first-slice
    /// scope only; the full sigil-form table arrives in subsequent PRs).
    #[error("E0104: unrecognised sigil-prefixed form '#{name}' at byte {at}")]
    UnknownHashForm {
        /// The identifier that followed the `#`.
        name: String,
        /// Byte offset of the leading `#`.
        at: usize,
    },

    /// A `@` sigil was followed by an unrecognised identifier.
    #[error("E0105: unrecognised sigil-prefixed form '@{name}' at byte {at}")]
    UnknownAtForm {
        /// The identifier that followed the `@`.
        name: String,
        /// Byte offset of the leading `@`.
        at: usize,
    },
}

// ─── Public API ──────────────────────────────────────────────────────────────

/// Tokenise a Clifford source string.
///
/// Returns the full token stream (including a trailing [`TokenKind::Eof`]) on
/// success, or the first lexical error encountered. Per §1.1 source files are
/// UTF-8; `\r\n` is normalised to `\n` at the read boundary (currently
/// performed by callers — the lexer itself accepts already-normalised input).
///
/// # Examples
///
/// ```
/// use clifford_lexer::{tokenize, TokenKind};
///
/// // Empty input → just an EOF token.
/// let tokens = tokenize("").unwrap();
/// assert_eq!(tokens.len(), 1);
/// assert_eq!(tokens[0].kind, TokenKind::Eof);
///
/// // A single keyword.
/// let tokens = tokenize("let").unwrap();
/// assert_eq!(tokens[0].kind, TokenKind::KwLet);
/// assert_eq!(tokens[1].kind, TokenKind::Eof);
/// ```
///
/// # Errors
///
/// Returns the first [`LexError`] encountered. The lexer is fail-fast in this
/// PR; error-recovery (resync at item / statement boundaries to collect all
/// errors) lands per CLAUDE.md §6 Phase 0 in a follow-up.
pub fn tokenize(input: &str) -> Result<Vec<Token>, LexError> {
    let mut lx = Lexer::new(input);
    let mut tokens = Vec::new();
    loop {
        let tok = lx.next_token()?;
        let is_eof = matches!(tok.kind, TokenKind::Eof);
        tokens.push(tok);
        if is_eof {
            return Ok(tokens);
        }
    }
}

// ─── Internal lexer ──────────────────────────────────────────────────────────

/// Cursor-based lexer over the source bytes.
///
/// Internal type; consumers use the [`tokenize`] function.
struct Lexer<'src> {
    src: &'src [u8],
    pos: usize,
}

impl<'src> Lexer<'src> {
    fn new(src: &'src str) -> Self {
        Self {
            src: src.as_bytes(),
            pos: 0,
        }
    }

    fn peek(&self, offset: usize) -> Option<u8> {
        self.src.get(self.pos + offset).copied()
    }

    fn advance(&mut self) -> Option<u8> {
        let b = self.src.get(self.pos).copied()?;
        self.pos += 1;
        Some(b)
    }

    fn skip_whitespace(&mut self) {
        while let Some(b) = self.peek(0) {
            // Spec §1.5 comments not in this slice; treat only ASCII
            // whitespace for now.
            if matches!(b, b' ' | b'\t' | b'\n' | b'\r') {
                self.pos += 1;
            } else {
                break;
            }
        }
    }

    fn next_token(&mut self) -> Result<Token, LexError> {
        self.skip_whitespace();

        let start = self.pos;
        let Some(b) = self.peek(0) else {
            return Ok(Token {
                kind: TokenKind::Eof,
                span: Span::new(start, start),
            });
        };

        match b {
            b'a'..=b'z' | b'A'..=b'Z' | b'_' => Ok(self.lex_ident_or_keyword(start)),
            b'0'..=b'9' => Ok(self.lex_integer(start)),
            b'#' => self.lex_hash_form(start),
            b'@' => self.lex_at_form(start),
            _ => self.lex_punct_or_op(start),
        }
    }

    fn lex_ident_or_keyword(&mut self, start: usize) -> Token {
        while let Some(b) = self.peek(0) {
            if b.is_ascii_alphanumeric() || b == b'_' {
                self.pos += 1;
            } else {
                break;
            }
        }
        let text = std::str::from_utf8(&self.src[start..self.pos])
            .expect("ident bytes are ASCII subset; UTF-8 valid by construction");
        let kind = match text {
            "let" => TokenKind::KwLet,
            "mut" => TokenKind::KwMut,
            "const" => TokenKind::KwConst,
            "static" => TokenKind::KwStatic,
            "if" => TokenKind::KwIf,
            "else" => TokenKind::KwElse,
            "while" => TokenKind::KwWhile,
            "loop" => TokenKind::KwLoop,
            "for" => TokenKind::KwFor,
            "in" => TokenKind::KwIn,
            "match" => TokenKind::KwMatch,
            "break" => TokenKind::KwBreak,
            "continue" => TokenKind::KwContinue,
            "return" => TokenKind::KwReturn,
            "extern" => TokenKind::KwExtern,
            "unsafe" => TokenKind::KwUnsafe,
            "as" => TokenKind::KwAs,
            "access" => TokenKind::KwAccess,
            "null" => TokenKind::KwNull,
            "self" => TokenKind::KwSelf,
            "Self" => TokenKind::KwSelfType,
            "true" => TokenKind::KwTrue,
            "false" => TokenKind::KwFalse,
            other => TokenKind::Ident(other.to_owned()),
        };
        Token {
            kind,
            span: Span::new(start, self.pos),
        }
    }

    fn lex_integer(&mut self, start: usize) -> Token {
        // First-slice integer literals: decimal digits with optional `_`
        // separators per §1.2 `integer := [0-9]+ ('_' [0-9]+)* type_suffix?`.
        // Type suffix and hex/binary/float forms land in the next PR.
        while let Some(b) = self.peek(0) {
            if b.is_ascii_digit() || b == b'_' {
                self.pos += 1;
            } else {
                break;
            }
        }
        let text = std::str::from_utf8(&self.src[start..self.pos])
            .expect("integer literal bytes are ASCII; UTF-8 valid by construction")
            .to_owned();
        Token {
            kind: TokenKind::IntLiteral(text),
            span: Span::new(start, self.pos),
        }
    }

    fn lex_hash_form(&mut self, start: usize) -> Result<Token, LexError> {
        // Consume the `#`.
        self.pos += 1;
        // First slice supports `#automaton` only as a representative
        // sigil-form. The full catalogue (Decisions #5–#20) lands in the
        // next PR.
        let name_start = self.pos;
        while let Some(b) = self.peek(0) {
            if b.is_ascii_alphanumeric() || b == b'_' {
                self.pos += 1;
            } else {
                break;
            }
        }
        let name = std::str::from_utf8(&self.src[name_start..self.pos])
            .expect("sigil-form name bytes are ASCII; UTF-8 valid by construction");
        let kind = match name {
            "automaton" => TokenKind::KwHashAutomaton,
            other => {
                return Err(LexError::UnknownHashForm {
                    name: other.to_owned(),
                    at: start,
                });
            }
        };
        Ok(Token {
            kind,
            span: Span::new(start, self.pos),
        })
    }

    fn lex_at_form(&mut self, start: usize) -> Result<Token, LexError> {
        // Consume the `@`.
        self.pos += 1;
        let name_start = self.pos;
        while let Some(b) = self.peek(0) {
            if b.is_ascii_alphanumeric() || b == b'_' {
                self.pos += 1;
            } else {
                break;
            }
        }
        let name = std::str::from_utf8(&self.src[name_start..self.pos])
            .expect("sigil-form name bytes are ASCII; UTF-8 valid by construction");
        let kind = match name {
            "fn" => TokenKind::KwAtFn,
            other => {
                return Err(LexError::UnknownAtForm {
                    name: other.to_owned(),
                    at: start,
                });
            }
        };
        Ok(Token {
            kind,
            span: Span::new(start, self.pos),
        })
    }

    fn lex_punct_or_op(&mut self, start: usize) -> Result<Token, LexError> {
        let b = self.advance().expect("caller verified at least one byte");
        let kind = match b {
            b'(' => TokenKind::LParen,
            b')' => TokenKind::RParen,
            b'{' => TokenKind::LBrace,
            b'}' => TokenKind::RBrace,
            b'[' => TokenKind::LBracket,
            b']' => TokenKind::RBracket,
            b',' => TokenKind::Comma,
            b';' => TokenKind::Semi,
            b'+' => TokenKind::Plus,
            b'-' => match self.peek(0) {
                Some(b'>') => {
                    self.pos += 1;
                    TokenKind::Arrow
                }
                _ => TokenKind::Minus,
            },
            b'*' => TokenKind::Star,
            b'/' => TokenKind::Slash,
            b'%' => TokenKind::Percent,
            b'&' => match self.peek(0) {
                Some(b'&') => {
                    self.pos += 1;
                    TokenKind::AmpAmp
                }
                _ => TokenKind::Amp,
            },
            b'|' => match self.peek(0) {
                Some(b'|') => {
                    self.pos += 1;
                    TokenKind::PipePipe
                }
                _ => TokenKind::Pipe,
            },
            b'^' => TokenKind::Caret,
            b'~' => TokenKind::Tilde,
            b'.' => TokenKind::Dot,
            b'?' => TokenKind::Question,
            b':' => match self.peek(0) {
                Some(b':') => {
                    self.pos += 1;
                    TokenKind::ColonColon
                }
                Some(b'=') => {
                    self.pos += 1;
                    TokenKind::ColonEq
                }
                _ => TokenKind::Colon,
            },
            b'=' => match self.peek(0) {
                Some(b'=') => {
                    self.pos += 1;
                    TokenKind::EqEq
                }
                Some(b'>') => {
                    self.pos += 1;
                    TokenKind::FatArrow
                }
                _ => TokenKind::Eq,
            },
            b'!' => match self.peek(0) {
                Some(b'=') => {
                    self.pos += 1;
                    TokenKind::BangEq
                }
                _ => TokenKind::Bang,
            },
            b'<' => match self.peek(0) {
                Some(b'=') => {
                    self.pos += 1;
                    TokenKind::LtEq
                }
                _ => TokenKind::Lt,
            },
            b'>' => match self.peek(0) {
                Some(b'=') => {
                    self.pos += 1;
                    TokenKind::GtEq
                }
                _ => TokenKind::Gt,
            },
            other => {
                return Err(LexError::UnexpectedChar {
                    ch: other as char,
                    at: start,
                });
            }
        };
        Ok(Token {
            kind,
            span: Span::new(start, self.pos),
        })
    }
}

// ─── Tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    fn kinds(src: &str) -> Vec<TokenKind> {
        tokenize(src).expect("tokenize").into_iter().map(|t| t.kind).collect()
    }

    #[test]
    fn empty_input_yields_eof() {
        assert_eq!(kinds(""), vec![TokenKind::Eof]);
    }

    #[test]
    fn whitespace_is_skipped() {
        assert_eq!(kinds("   \t\n  "), vec![TokenKind::Eof]);
    }

    #[test]
    fn ident_then_keyword() {
        assert_eq!(
            kinds("foo let bar"),
            vec![
                TokenKind::Ident("foo".into()),
                TokenKind::KwLet,
                TokenKind::Ident("bar".into()),
                TokenKind::Eof,
            ],
        );
    }

    #[test]
    fn all_first_slice_keywords() {
        let src = "let mut const static if else while loop for in match break continue return extern unsafe as access null self Self true false";
        let expected = vec![
            TokenKind::KwLet,
            TokenKind::KwMut,
            TokenKind::KwConst,
            TokenKind::KwStatic,
            TokenKind::KwIf,
            TokenKind::KwElse,
            TokenKind::KwWhile,
            TokenKind::KwLoop,
            TokenKind::KwFor,
            TokenKind::KwIn,
            TokenKind::KwMatch,
            TokenKind::KwBreak,
            TokenKind::KwContinue,
            TokenKind::KwReturn,
            TokenKind::KwExtern,
            TokenKind::KwUnsafe,
            TokenKind::KwAs,
            TokenKind::KwAccess,
            TokenKind::KwNull,
            TokenKind::KwSelf,
            TokenKind::KwSelfType,
            TokenKind::KwTrue,
            TokenKind::KwFalse,
            TokenKind::Eof,
        ];
        assert_eq!(kinds(src), expected);
    }

    #[test]
    fn integer_literals_with_underscores() {
        assert_eq!(
            kinds("0 42 1_000_000"),
            vec![
                TokenKind::IntLiteral("0".into()),
                TokenKind::IntLiteral("42".into()),
                TokenKind::IntLiteral("1_000_000".into()),
                TokenKind::Eof,
            ],
        );
    }

    #[test]
    fn single_and_double_char_operators() {
        let src = "( ) { } [ ] , ; : :: = == != < <= > >= + - * / % & && | || ^ ! ~ . ? -> => :=";
        let expected = vec![
            TokenKind::LParen,
            TokenKind::RParen,
            TokenKind::LBrace,
            TokenKind::RBrace,
            TokenKind::LBracket,
            TokenKind::RBracket,
            TokenKind::Comma,
            TokenKind::Semi,
            TokenKind::Colon,
            TokenKind::ColonColon,
            TokenKind::Eq,
            TokenKind::EqEq,
            TokenKind::BangEq,
            TokenKind::Lt,
            TokenKind::LtEq,
            TokenKind::Gt,
            TokenKind::GtEq,
            TokenKind::Plus,
            TokenKind::Minus,
            TokenKind::Star,
            TokenKind::Slash,
            TokenKind::Percent,
            TokenKind::Amp,
            TokenKind::AmpAmp,
            TokenKind::Pipe,
            TokenKind::PipePipe,
            TokenKind::Caret,
            TokenKind::Bang,
            TokenKind::Tilde,
            TokenKind::Dot,
            TokenKind::Question,
            TokenKind::Arrow,
            TokenKind::FatArrow,
            TokenKind::ColonEq,
            TokenKind::Eof,
        ];
        assert_eq!(kinds(src), expected);
    }

    #[test]
    fn sigil_dispatch_first_slice() {
        // Decision #1: `#automaton` and `@fn` are first-slice sigil-prefixed
        // forms. Each is lexed as a single atomic token, not as `#` + ident.
        assert_eq!(
            kinds("#automaton @fn"),
            vec![
                TokenKind::KwHashAutomaton,
                TokenKind::KwAtFn,
                TokenKind::Eof,
            ],
        );
    }

    #[test]
    fn unknown_sigil_form_errors() {
        // Anything beyond the first-slice catalogue errors with E0104/E0105.
        // Subsequent PRs expand the dispatch tables.
        assert!(matches!(
            tokenize("#unknownThing"),
            Err(LexError::UnknownHashForm { .. }),
        ));
        assert!(matches!(
            tokenize("@badform"),
            Err(LexError::UnknownAtForm { .. }),
        ));
    }

    #[test]
    fn unexpected_character_errors() {
        // `$` is the trait-list marker (next PR); for now it errors.
        assert!(matches!(
            tokenize("$"),
            Err(LexError::UnexpectedChar { ch: '$', at: 0 }),
        ));
    }

    #[test]
    fn spans_track_correctly() {
        let tokens = tokenize("let foo").unwrap();
        assert_eq!(tokens[0].kind, TokenKind::KwLet);
        assert_eq!(tokens[0].span, Span::new(0, 3));
        assert_eq!(tokens[1].kind, TokenKind::Ident("foo".into()));
        assert_eq!(tokens[1].span, Span::new(4, 7));
        assert_eq!(tokens[2].kind, TokenKind::Eof);
        assert_eq!(tokens[2].span, Span::new(7, 7));
    }

    #[test]
    fn span_arithmetic() {
        let s = Span::new(3, 10);
        assert_eq!(s.len(), 7);
        assert!(!s.is_empty());

        let empty = Span::new(5, 5);
        assert!(empty.is_empty());
    }

    #[test]
    fn deterministic() {
        // Per CLAUDE.md §6 Phase 0: same input → same output, byte for byte.
        let src = "let x = 42";
        let a = tokenize(src).unwrap();
        let b = tokenize(src).unwrap();
        assert_eq!(a, b);
    }
}
