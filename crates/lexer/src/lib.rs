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
//! Slices 1 + 2 (this PR): whitespace, comments (line / nested block / doc-skip),
//! identifiers, all bare keywords from §1.3, the full sigil catalogue (`#`-forms,
//! `@`-forms, `$`, composite `#>`), decimal integer literals with `_` separators,
//! the full §1.4 operator set including compound assignment, shifts, and range.
//!
//! Slice 3 (next PR): hex / binary / float literals with type suffixes; string,
//! char, and byte (`b'X'`) literals with escape sequences. Doc-comment token
//! preservation (currently skipped) lands when the AST gains a doc field.

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
/// Covers the full §1.3 keyword + sigil catalogue and the full §1.4 operator
/// set. Literal variants are limited to decimal integers in this slice; the
/// hex / binary / float / string / char / byte family arrives in slice 3.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum TokenKind {
    // ─ Identifiers ─────────────────────────────────────────────────────────
    /// A user-supplied identifier: `[a-zA-Z_][a-zA-Z0-9_]*`.
    Ident(String),

    // ─ Bare keywords (§1.3) ────────────────────────────────────────────────
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
    /// `access` (Decision #19 type constructor: `access<T>` / `access const<T>`)
    KwAccess,
    /// `null` (context-typed null access literal; Decision #19)
    KwNull,
    /// `self`
    KwSelf,
    /// `Self`
    KwSelfType,
    /// `true`
    KwTrue,
    /// `false`
    KwFalse,

    // ─ Imperative sigil-prefixed forms (`#`) — §1.3 ───────────────────────
    /// `#automaton` (Decision #5)
    KwHashAutomaton,
    /// `#effect`
    KwHashEffect,
    /// `#interrupt`
    KwHashInterrupt,
    /// `#interface` (Decision #16)
    KwHashInterface,
    /// `#impl` (Decision #16)
    KwHashImpl,
    /// `#test "name" { … }` (Decision #7)
    KwHashTest,
    /// `#mutate Auto { field = expr, … }`
    KwHashMutate,
    /// `#transition <name>: Source -> Target { … }` (Refinement #5b)
    KwHashTransition,
    /// `#states: [Init, Running, …]` (Decision #5)
    KwHashStates,
    /// `#mutates: [A, B, …]`
    KwHashMutates,
    /// `#cannot_mutate: [A, B, …]`
    KwHashCannotMutate,
    /// `#invariant: expr`
    KwHashInvariant,
    /// `#priority: LOW | MEDIUM | HIGH | <int>`
    KwHashPriority,
    /// `#atomic: interrupt_critical | multicore_critical | <ident>`
    KwHashAtomic,
    /// `#basis: { field: e_i, … }` (Decision #4)
    KwHashBasis,
    /// `#address: 0x…` (Decision #6 register-block annotation)
    KwHashAddress,
    /// `#offset: <int>` (Decision #6 register-field annotation)
    KwHashOffset,
    /// `#access: r | w | rw` (Decision #6 register-field annotation)
    KwHashAccess,
    /// `#bits { … }` (Decision #20 first-class bitfields)
    KwHashBits,
    /// `#at: <int>` (Decision #20 bitfield offset)
    KwHashAt,
    /// `#unchecked_load<T>(p)` (Decision #17 narrow primitive)
    KwHashUncheckedLoad,
    /// `#unchecked_store<T>(p, v)` (Decision #17)
    KwHashUncheckedStore,
    /// `#volatile_load<T>(p)` (Decision #17)
    KwHashVolatileLoad,
    /// `#volatile_store<T>(p, v)` (Decision #17)
    KwHashVolatileStore,
    /// `#unchecked_cast<S, T>("reason", v)` (Decision #17 + Refinement #19a)
    KwHashUncheckedCast,
    /// `#unchecked_offset<T>(p, n)` (Decision #19)
    KwHashUncheckedOffset,
    /// `#asm("…", inputs, outputs)` (Decision #17)
    KwHashAsm,
    /// `#free p` (Decision #13 Rule 5 linear deallocation)
    KwHashFree,
    /// `#flush A;` (Decision #12, reserved for v0.2 — recognised so v0.2 code parses)
    KwHashFlush,
    /// `#staged` modifier (Decision #12, reserved for v0.2)
    KwHashStaged,
    /// `#audit` annotation (Decision #18, reserved for v0.2)
    KwHashAudit,

    // ─ Functional sigil-prefixed forms (`@`) — §1.3 ───────────────────────
    /// `@fn` (Decision #1)
    KwAtFn,
    /// `@type`
    KwAtType,
    /// `@trait`
    KwAtTrait,
    /// `@module`
    KwAtModule,
    /// `@sequential(A, B);` top-level attribute (Decision #11)
    KwAtSequential,
    /// `@initial` state marker (§6.1)
    KwAtInitial,
    /// `@terminal` state marker (§6.1)
    KwAtTerminal,
    /// `@non_atomic` transition opt-out attribute (Refinement #5e)
    KwAtNonAtomic,
    /// `Auto@state` state-read operator (Refinement #5d)
    KwAtState,

    // ─ Composite sigils ─────────────────────────────────────────────────────
    /// `#>` effect-procedure call operator (Decision #3)
    HashGt,
    /// `$` trait-list marker (Decision #2)
    Dollar,

    // ─ Literals ────────────────────────────────────────────────────────────
    /// Decimal integer literal. Stores the raw textual digits for now;
    /// numeric value parsing and type-suffix handling land in slice 3.
    IntLiteral(String),

    // ─ Operators and punctuation (§1.4) ────────────────────────────────────
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
    /// `::` (path separator; `<Auto>::<StateName>` per Refinement #5d)
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
    /// `..` half-open range
    DotDot,
    /// `..=` inclusive range
    DotDotEq,
    /// `?`
    Question,
    /// `->` arrow
    Arrow,
    /// `=>` fat arrow
    FatArrow,
    /// `:=` short binding (Decision #8)
    ColonEq,
    /// `+=`
    PlusEq,
    /// `-=`
    MinusEq,
    /// `*=`
    StarEq,
    /// `/=`
    SlashEq,
    /// `%=`
    PercentEq,
    /// `&=`
    AmpEq,
    /// `|=`
    PipeEq,
    /// `^=`
    CaretEq,
    /// `<<`
    Shl,
    /// `>>`
    Shr,
    /// `<<=`
    ShlEq,
    /// `>>=`
    ShrEq,

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

    /// A `#` sigil was followed by an unrecognised identifier.
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

    /// A block comment (`/* … */`) was opened but never closed before EOF.
    #[error("E0106: unterminated block comment opened at byte {at}")]
    UnterminatedBlockComment {
        /// Byte offset of the opening `/*`.
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

    /// Skip whitespace and comments (§1.5).
    ///
    /// - Whitespace: space, tab, LF, CR.
    /// - Line comments `// …` consumed up to (but not including) the newline.
    /// - Block comments `/* … */` consumed including any nested block comments.
    /// - Doc comments `/// …` are currently consumed as line comments;
    ///   preservation as `DocComment` tokens lands when the AST has a place
    ///   for them.
    fn skip_trivia(&mut self) -> Result<(), LexError> {
        loop {
            match self.peek(0) {
                Some(b' ' | b'\t' | b'\n' | b'\r') => {
                    self.pos += 1;
                }
                Some(b'/') if self.peek(1) == Some(b'/') => {
                    // Line or doc comment. (Doc comments — `///` — currently
                    // skipped just like line comments; will be promoted to
                    // tokens when the AST is ready.)
                    self.pos += 2;
                    while let Some(b) = self.peek(0) {
                        if b == b'\n' {
                            break;
                        }
                        self.pos += 1;
                    }
                }
                Some(b'/') if self.peek(1) == Some(b'*') => {
                    let opened_at = self.pos;
                    self.pos += 2;
                    let mut depth: usize = 1;
                    loop {
                        match (self.peek(0), self.peek(1)) {
                            (None, _) => {
                                return Err(LexError::UnterminatedBlockComment {
                                    at: opened_at,
                                });
                            }
                            (Some(b'/'), Some(b'*')) => {
                                self.pos += 2;
                                depth += 1;
                            }
                            (Some(b'*'), Some(b'/')) => {
                                self.pos += 2;
                                depth -= 1;
                                if depth == 0 {
                                    break;
                                }
                            }
                            _ => {
                                self.pos += 1;
                            }
                        }
                    }
                }
                _ => return Ok(()),
            }
        }
    }

    fn next_token(&mut self) -> Result<Token, LexError> {
        self.skip_trivia()?;

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
            b'$' => {
                self.pos += 1;
                Ok(Token {
                    kind: TokenKind::Dollar,
                    span: Span::new(start, self.pos),
                })
            }
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
        // Decimal digits with optional `_` separators per §1.2
        // `integer := [0-9]+ ('_' [0-9]+)* type_suffix?`. Type suffix and
        // hex / binary / float forms land in slice 3.
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
        // Composite sigil `#>` (Decision #3 effect-procedure call).
        if self.peek(0) == Some(b'>') {
            self.pos += 1;
            return Ok(Token {
                kind: TokenKind::HashGt,
                span: Span::new(start, self.pos),
            });
        }
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
            // Decision #5 + Refinement #5b
            "automaton" => TokenKind::KwHashAutomaton,
            "effect" => TokenKind::KwHashEffect,
            "interrupt" => TokenKind::KwHashInterrupt,
            "transition" => TokenKind::KwHashTransition,
            "states" => TokenKind::KwHashStates,
            "mutate" => TokenKind::KwHashMutate,
            "mutates" => TokenKind::KwHashMutates,
            "cannot_mutate" => TokenKind::KwHashCannotMutate,
            "invariant" => TokenKind::KwHashInvariant,
            "priority" => TokenKind::KwHashPriority,
            "atomic" => TokenKind::KwHashAtomic,
            "basis" => TokenKind::KwHashBasis,
            // Decision #6 register-block annotations
            "address" => TokenKind::KwHashAddress,
            "offset" => TokenKind::KwHashOffset,
            "access" => TokenKind::KwHashAccess,
            // Decision #20 bit-fields
            "bits" => TokenKind::KwHashBits,
            "at" => TokenKind::KwHashAt,
            // Decision #16 plugin mutators
            "interface" => TokenKind::KwHashInterface,
            "impl" => TokenKind::KwHashImpl,
            // Decision #7 testing
            "test" => TokenKind::KwHashTest,
            // Decision #17 + #19 narrow unsafe primitives
            "unchecked_load" => TokenKind::KwHashUncheckedLoad,
            "unchecked_store" => TokenKind::KwHashUncheckedStore,
            "volatile_load" => TokenKind::KwHashVolatileLoad,
            "volatile_store" => TokenKind::KwHashVolatileStore,
            "unchecked_cast" => TokenKind::KwHashUncheckedCast,
            "unchecked_offset" => TokenKind::KwHashUncheckedOffset,
            "asm" => TokenKind::KwHashAsm,
            // Decision #13 Rule 5 linear deallocation
            "free" => TokenKind::KwHashFree,
            // Reserved for v0.2 — Decision #12 / #18
            "staged" => TokenKind::KwHashStaged,
            "flush" => TokenKind::KwHashFlush,
            "audit" => TokenKind::KwHashAudit,
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
            // Decision #1 functional layer constructs
            "fn" => TokenKind::KwAtFn,
            "type" => TokenKind::KwAtType,
            "trait" => TokenKind::KwAtTrait,
            "module" => TokenKind::KwAtModule,
            // Decision #11 sequential attribute
            "sequential" => TokenKind::KwAtSequential,
            // §6.1 state markers
            "initial" => TokenKind::KwAtInitial,
            "terminal" => TokenKind::KwAtTerminal,
            // Refinement #5e transition opt-out
            "non_atomic" => TokenKind::KwAtNonAtomic,
            // Refinement #5d state-read operator (Auto@state)
            "state" => TokenKind::KwAtState,
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
            b'+' => match self.peek(0) {
                Some(b'=') => {
                    self.pos += 1;
                    TokenKind::PlusEq
                }
                _ => TokenKind::Plus,
            },
            b'-' => match self.peek(0) {
                Some(b'>') => {
                    self.pos += 1;
                    TokenKind::Arrow
                }
                Some(b'=') => {
                    self.pos += 1;
                    TokenKind::MinusEq
                }
                _ => TokenKind::Minus,
            },
            b'*' => match self.peek(0) {
                Some(b'=') => {
                    self.pos += 1;
                    TokenKind::StarEq
                }
                _ => TokenKind::Star,
            },
            b'/' => match self.peek(0) {
                Some(b'=') => {
                    self.pos += 1;
                    TokenKind::SlashEq
                }
                _ => TokenKind::Slash,
            },
            b'%' => match self.peek(0) {
                Some(b'=') => {
                    self.pos += 1;
                    TokenKind::PercentEq
                }
                _ => TokenKind::Percent,
            },
            b'&' => match self.peek(0) {
                Some(b'&') => {
                    self.pos += 1;
                    TokenKind::AmpAmp
                }
                Some(b'=') => {
                    self.pos += 1;
                    TokenKind::AmpEq
                }
                _ => TokenKind::Amp,
            },
            b'|' => match self.peek(0) {
                Some(b'|') => {
                    self.pos += 1;
                    TokenKind::PipePipe
                }
                Some(b'=') => {
                    self.pos += 1;
                    TokenKind::PipeEq
                }
                _ => TokenKind::Pipe,
            },
            b'^' => match self.peek(0) {
                Some(b'=') => {
                    self.pos += 1;
                    TokenKind::CaretEq
                }
                _ => TokenKind::Caret,
            },
            b'~' => TokenKind::Tilde,
            b'.' => match (self.peek(0), self.peek(1)) {
                (Some(b'.'), Some(b'=')) => {
                    self.pos += 2;
                    TokenKind::DotDotEq
                }
                (Some(b'.'), _) => {
                    self.pos += 1;
                    TokenKind::DotDot
                }
                _ => TokenKind::Dot,
            },
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
            b'<' => match (self.peek(0), self.peek(1)) {
                (Some(b'<'), Some(b'=')) => {
                    self.pos += 2;
                    TokenKind::ShlEq
                }
                (Some(b'<'), _) => {
                    self.pos += 1;
                    TokenKind::Shl
                }
                (Some(b'='), _) => {
                    self.pos += 1;
                    TokenKind::LtEq
                }
                _ => TokenKind::Lt,
            },
            b'>' => match (self.peek(0), self.peek(1)) {
                (Some(b'>'), Some(b'=')) => {
                    self.pos += 2;
                    TokenKind::ShrEq
                }
                (Some(b'>'), _) => {
                    self.pos += 1;
                    TokenKind::Shr
                }
                (Some(b'='), _) => {
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
        tokenize(src)
            .expect("tokenize")
            .into_iter()
            .map(|t| t.kind)
            .collect()
    }

    // ─── Slice 1 (preserved) ──────────────────────────────────────────────

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
    fn all_bare_keywords() {
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

    // ─── Slice 2: full sigil catalogue ─────────────────────────────────────

    #[test]
    fn all_imperative_sigil_forms() {
        let src = "#automaton #effect #interrupt #interface #impl #test \
                   #mutate #transition #states #mutates #cannot_mutate \
                   #invariant #priority #atomic #basis \
                   #address #offset #access #bits #at \
                   #unchecked_load #unchecked_store #volatile_load #volatile_store \
                   #unchecked_cast #unchecked_offset #asm #free \
                   #staged #flush #audit";
        let expected = vec![
            TokenKind::KwHashAutomaton,
            TokenKind::KwHashEffect,
            TokenKind::KwHashInterrupt,
            TokenKind::KwHashInterface,
            TokenKind::KwHashImpl,
            TokenKind::KwHashTest,
            TokenKind::KwHashMutate,
            TokenKind::KwHashTransition,
            TokenKind::KwHashStates,
            TokenKind::KwHashMutates,
            TokenKind::KwHashCannotMutate,
            TokenKind::KwHashInvariant,
            TokenKind::KwHashPriority,
            TokenKind::KwHashAtomic,
            TokenKind::KwHashBasis,
            TokenKind::KwHashAddress,
            TokenKind::KwHashOffset,
            TokenKind::KwHashAccess,
            TokenKind::KwHashBits,
            TokenKind::KwHashAt,
            TokenKind::KwHashUncheckedLoad,
            TokenKind::KwHashUncheckedStore,
            TokenKind::KwHashVolatileLoad,
            TokenKind::KwHashVolatileStore,
            TokenKind::KwHashUncheckedCast,
            TokenKind::KwHashUncheckedOffset,
            TokenKind::KwHashAsm,
            TokenKind::KwHashFree,
            TokenKind::KwHashStaged,
            TokenKind::KwHashFlush,
            TokenKind::KwHashAudit,
            TokenKind::Eof,
        ];
        assert_eq!(kinds(src), expected);
    }

    #[test]
    fn all_functional_sigil_forms() {
        let src = "@fn @type @trait @module @sequential @initial @terminal @non_atomic @state";
        let expected = vec![
            TokenKind::KwAtFn,
            TokenKind::KwAtType,
            TokenKind::KwAtTrait,
            TokenKind::KwAtModule,
            TokenKind::KwAtSequential,
            TokenKind::KwAtInitial,
            TokenKind::KwAtTerminal,
            TokenKind::KwAtNonAtomic,
            TokenKind::KwAtState,
            TokenKind::Eof,
        ];
        assert_eq!(kinds(src), expected);
    }

    #[test]
    fn composite_sigils() {
        // `#>` as one token, not `#` + `>`. `$` as the trait-list marker.
        assert_eq!(
            kinds("#> $"),
            vec![TokenKind::HashGt, TokenKind::Dollar, TokenKind::Eof],
        );
    }

    #[test]
    fn hash_gt_is_atomic() {
        // Crucial: `#>name` (no space) tokenises as `#>` then `Ident("name")`,
        // not as `#name` then `>` (which would be wrong; `#name` would also
        // need to be a known sigil-form).
        let tokens = kinds("#> tick");
        assert_eq!(
            tokens,
            vec![
                TokenKind::HashGt,
                TokenKind::Ident("tick".into()),
                TokenKind::Eof,
            ],
        );
    }

    #[test]
    fn auto_at_state_pattern() {
        // `Counter@state == Counter::Idle` — the recognition pattern from
        // Refinement #5d. Lexer produces the right tokens; parser composes.
        let tokens = kinds("Counter@state == Counter::Idle");
        assert_eq!(
            tokens,
            vec![
                TokenKind::Ident("Counter".into()),
                TokenKind::KwAtState,
                TokenKind::EqEq,
                TokenKind::Ident("Counter".into()),
                TokenKind::ColonColon,
                TokenKind::Ident("Idle".into()),
                TokenKind::Eof,
            ],
        );
    }

    #[test]
    fn unknown_sigil_form_errors() {
        assert!(matches!(
            tokenize("#unknownThing"),
            Err(LexError::UnknownHashForm { .. }),
        ));
        assert!(matches!(
            tokenize("@badform"),
            Err(LexError::UnknownAtForm { .. }),
        ));
    }

    // ─── Slice 2: comments ────────────────────────────────────────────────

    #[test]
    fn line_comment_skipped() {
        let tokens = kinds("let // a comment\nfoo");
        assert_eq!(
            tokens,
            vec![
                TokenKind::KwLet,
                TokenKind::Ident("foo".into()),
                TokenKind::Eof,
            ],
        );
    }

    #[test]
    fn doc_comment_skipped_for_now() {
        // Doc comments are skipped in this slice; will be promoted to tokens
        // when the AST has a place for them.
        let tokens = kinds("/// doc\nlet x");
        assert_eq!(
            tokens,
            vec![
                TokenKind::KwLet,
                TokenKind::Ident("x".into()),
                TokenKind::Eof,
            ],
        );
    }

    #[test]
    fn block_comment_skipped() {
        let tokens = kinds("let /* comment */ foo");
        assert_eq!(
            tokens,
            vec![
                TokenKind::KwLet,
                TokenKind::Ident("foo".into()),
                TokenKind::Eof,
            ],
        );
    }

    #[test]
    fn block_comments_nest() {
        // §1.5: "Block comments nest." — only the outermost `*/` closes.
        let tokens = kinds("let /* outer /* inner */ still outer */ foo");
        assert_eq!(
            tokens,
            vec![
                TokenKind::KwLet,
                TokenKind::Ident("foo".into()),
                TokenKind::Eof,
            ],
        );
    }

    #[test]
    fn unterminated_block_comment_errors() {
        assert!(matches!(
            tokenize("/* unfinished"),
            Err(LexError::UnterminatedBlockComment { at: 0 }),
        ));
    }

    // ─── Slice 2: full operator set ───────────────────────────────────────

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
    fn compound_assignment_operators() {
        let src = "+= -= *= /= %= &= |= ^= <<= >>=";
        let expected = vec![
            TokenKind::PlusEq,
            TokenKind::MinusEq,
            TokenKind::StarEq,
            TokenKind::SlashEq,
            TokenKind::PercentEq,
            TokenKind::AmpEq,
            TokenKind::PipeEq,
            TokenKind::CaretEq,
            TokenKind::ShlEq,
            TokenKind::ShrEq,
            TokenKind::Eof,
        ];
        assert_eq!(kinds(src), expected);
    }

    #[test]
    fn shift_and_range_operators() {
        let src = "<< >> .. ..=";
        let expected = vec![
            TokenKind::Shl,
            TokenKind::Shr,
            TokenKind::DotDot,
            TokenKind::DotDotEq,
            TokenKind::Eof,
        ];
        assert_eq!(kinds(src), expected);
    }

    #[test]
    fn shl_does_not_consume_assign_when_not_present() {
        // `<<` followed by something other than `=` should yield Shl + that thing.
        let tokens = kinds("a << b");
        assert_eq!(
            tokens,
            vec![
                TokenKind::Ident("a".into()),
                TokenKind::Shl,
                TokenKind::Ident("b".into()),
                TokenKind::Eof,
            ],
        );
    }

    #[test]
    fn dot_dot_eq_takes_precedence_over_dot_dot() {
        let tokens = kinds("a..=b");
        assert_eq!(
            tokens,
            vec![
                TokenKind::Ident("a".into()),
                TokenKind::DotDotEq,
                TokenKind::Ident("b".into()),
                TokenKind::Eof,
            ],
        );
    }

    // ─── Slice 2: integration / ergonomic samples ─────────────────────────

    #[test]
    fn blinky_imperative_line() {
        // `Counter.blinks += 1;` — the Decision #15 sugar form.
        let tokens = kinds("Counter.blinks += 1;");
        assert_eq!(
            tokens,
            vec![
                TokenKind::Ident("Counter".into()),
                TokenKind::Dot,
                TokenKind::Ident("blinks".into()),
                TokenKind::PlusEq,
                TokenKind::IntLiteral("1".into()),
                TokenKind::Semi,
                TokenKind::Eof,
            ],
        );
    }

    #[test]
    fn pure_fn_signature() {
        // `@fn pick_next(snap: &SchedulerSnapshot) -> Decision $ [Pure]`
        let src = "@fn pick_next ( snap : & SchedulerSnapshot ) -> Decision $ [ Pure ]";
        let expected = vec![
            TokenKind::KwAtFn,
            TokenKind::Ident("pick_next".into()),
            TokenKind::LParen,
            TokenKind::Ident("snap".into()),
            TokenKind::Colon,
            TokenKind::Amp,
            TokenKind::Ident("SchedulerSnapshot".into()),
            TokenKind::RParen,
            TokenKind::Arrow,
            TokenKind::Ident("Decision".into()),
            TokenKind::Dollar,
            TokenKind::LBracket,
            TokenKind::Ident("Pure".into()),
            TokenKind::RBracket,
            TokenKind::Eof,
        ];
        assert_eq!(kinds(src), expected);
    }

    #[test]
    fn transition_call() {
        // `#> boot_complete();` — Refinement #5b transition invocation.
        let tokens = kinds("#> boot_complete();");
        assert_eq!(
            tokens,
            vec![
                TokenKind::HashGt,
                TokenKind::Ident("boot_complete".into()),
                TokenKind::LParen,
                TokenKind::RParen,
                TokenKind::Semi,
                TokenKind::Eof,
            ],
        );
    }

    // ─── Error and span sanity ────────────────────────────────────────────

    #[test]
    fn unexpected_character_errors() {
        // `?` is valid but `\\` is not.
        assert!(matches!(
            tokenize("\\"),
            Err(LexError::UnexpectedChar { ch: '\\', at: 0 }),
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
        let src = "let x := 42 + Counter.blinks;";
        let a = tokenize(src).unwrap();
        let b = tokenize(src).unwrap();
        assert_eq!(a, b);
    }
}
