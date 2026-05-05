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
    /// `#hidden` per-field encapsulation modifier (Decision #25).
    /// Marks an automaton field as private to the owning automaton's
    /// transitions; outside callables cannot reference it. Algebraic-
    /// trivial-orthogonality: the field's basis vector simply doesn't
    /// appear in non-owning callables' `actual_writes`.
    KwHashHidden,
    /// `#shared` field qualifier (Decision #21, reserved for v0.7+).
    /// Lexes as a token so source compatibility holds across the v0.7
    /// transition; the parser rejects with a "reserved for v0.7" diagnostic.
    KwHashShared,
    /// `#lock NAME #priority: N;` declaration (Decision #21, reserved for v0.7+).
    KwHashLock,
    /// `#with_lock(NAME) { … }` block (Decision #21, reserved for v0.7+).
    KwHashWithLock,
    /// `#reads: [...]` clause on `#effect` / `#interrupt`
    /// (Decision #21, reserved for v0.7+).
    KwHashReads,
    /// `#rotor: SECTION_OFFSET` lock-rotor parameter (Decision #21, reserved for v0.7+).
    KwHashRotor,

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
    /// Decimal integer literal. Stores the raw textual digits *including*
    /// any optional type suffix (e.g. `42`, `1_000_000`, `42u32`). Numeric
    /// value parsing happens at the type-checker layer; this token form
    /// preserves the source text faithfully so diagnostics can quote it.
    IntLiteral(String),
    /// Hexadecimal integer literal: `0x` prefix preserved (e.g. `0xDEAD_BEEF`,
    /// `0xFFu8`).
    HexLiteral(String),
    /// Binary integer literal: `0b` prefix preserved (e.g. `0b1010_0101`,
    /// `0b1111u8`).
    BinLiteral(String),
    /// Float literal: `<digits>.<digits>(e[+-]?<digits>)?<suffix>?`. The
    /// raw source text is preserved, including any `f32` / `f64` suffix.
    /// Per §1.2, the leading and trailing digits around the `.` are
    /// mandatory; `1.` and `.5` are *not* float literals.
    FloatLiteral(String),
    /// Character literal `'X'`. Escape sequences (`\n`, `\\`, `\xHH`, …) are
    /// resolved at lex time; the stored value is the resulting Unicode scalar.
    CharLiteral(char),
    /// Byte literal `b'X'` (Decision #15 / §1.2). Escape sequences are
    /// resolved; the stored value is a single `u8`. Non-ASCII bytes are
    /// rejected at lex time per §1.2's grammar.
    ByteLiteral(u8),
    /// String literal `"…"`. Escape sequences are resolved at lex time; the
    /// stored value is the resulting `String`. Multi-line strings and raw
    /// strings are out of scope for v0.1.
    StringLiteral(String),

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

    /// A string literal was opened but never closed before EOF or LF.
    #[error("E0107: unterminated string literal opened at byte {at}")]
    UnterminatedStringLiteral {
        /// Byte offset of the opening `"`.
        at: usize,
    },

    /// A char or byte literal was opened but never closed.
    #[error("E0108: unterminated {kind} literal opened at byte {at}")]
    UnterminatedCharLiteral {
        /// `"char"` or `"byte"`.
        kind: &'static str,
        /// Byte offset of the opening `'`.
        at: usize,
    },

    /// An invalid escape sequence (`\…`) was encountered inside a string,
    /// char, or byte literal.
    #[error("E0109: invalid escape sequence '\\{ch}' at byte {at}")]
    InvalidEscape {
        /// The character that followed the backslash.
        ch: char,
        /// Byte offset of the backslash.
        at: usize,
    },

    /// A char literal contained zero or more than one character.
    #[error("E0110: char literal must contain exactly one character (at byte {at})")]
    InvalidCharLiteral {
        /// Byte offset of the opening `'`.
        at: usize,
    },

    /// A byte literal contained a non-ASCII character.
    #[error("E0111: byte literal must be ASCII (at byte {at})")]
    NonAsciiByteLiteral {
        /// Byte offset of the opening `'`.
        at: usize,
    },

    /// A numeric literal had a malformed prefix (`0x` / `0b`) with no digits
    /// following, or a malformed exponent (`1.0e` with no digits).
    #[error("E0112: malformed numeric literal at byte {at}: {msg}")]
    MalformedNumber {
        /// Byte offset where the literal begins.
        at: usize,
        /// Human-readable reason.
        msg: &'static str,
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
            // Byte literal `b'X'`. Must dispatch before ident lexing because
            // bare `b` is otherwise an identifier start.
            b'b' if self.peek(1) == Some(b'\'') => self.lex_byte_literal(start),
            b'a'..=b'z' | b'A'..=b'Z' | b'_' => Ok(self.lex_ident_or_keyword(start)),
            b'0'..=b'9' => self.lex_number(start),
            b'\'' => self.lex_char_literal(start),
            b'"' => self.lex_string_literal(start),
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

    /// Lex a numeric literal — dispatches between integer (decimal / hex /
    /// binary) and float per §1.2.
    ///
    /// Caller has verified `peek(0)` is an ASCII digit at `start`.
    fn lex_number(&mut self, start: usize) -> Result<Token, LexError> {
        // Hex / binary prefix dispatch.
        if self.peek(0) == Some(b'0') {
            match self.peek(1) {
                Some(b'x' | b'X') => {
                    self.pos += 2;
                    return self.lex_hex_or_bin_tail(start, /*is_hex=*/ true);
                }
                Some(b'b' | b'B') => {
                    // Disambiguate from a bare `0` followed by an ident `b…` —
                    // but per §1.2 binary literals always have ≥1 binary digit
                    // immediately after `0b`. If what follows is not a 0/1, we
                    // fall back to decimal.
                    if matches!(self.peek(2), Some(b'0' | b'1')) {
                        self.pos += 2;
                        return self.lex_hex_or_bin_tail(start, /*is_hex=*/ false);
                    }
                }
                _ => {}
            }
        }

        // Decimal integer digits with optional `_` separators.
        while let Some(b) = self.peek(0) {
            if b.is_ascii_digit() || b == b'_' {
                self.pos += 1;
            } else {
                break;
            }
        }

        // Float dispatch: `<digits>.<digit>…` (digit immediately after the
        // dot is required per §1.2; this also keeps `1..5` and `tup.0` from
        // being misread as floats).
        if self.peek(0) == Some(b'.') && matches!(self.peek(1), Some(b'0'..=b'9')) {
            self.pos += 1; // the dot
            while let Some(b) = self.peek(0) {
                if b.is_ascii_digit() || b == b'_' {
                    self.pos += 1;
                } else {
                    break;
                }
            }
            // Optional exponent: `e` / `E` followed by optional sign, ≥1 digit.
            if matches!(self.peek(0), Some(b'e' | b'E')) {
                let exp_start = self.pos;
                self.pos += 1;
                if matches!(self.peek(0), Some(b'+' | b'-')) {
                    self.pos += 1;
                }
                let digits_start = self.pos;
                while let Some(b) = self.peek(0) {
                    if b.is_ascii_digit() {
                        self.pos += 1;
                    } else {
                        break;
                    }
                }
                if self.pos == digits_start {
                    return Err(LexError::MalformedNumber {
                        at: exp_start,
                        msg: "exponent has no digits",
                    });
                }
            }
            // Optional float type suffix: f32 / f64.
            self.consume_float_suffix();
            let text = self.text(start);
            return Ok(Token {
                kind: TokenKind::FloatLiteral(text),
                span: Span::new(start, self.pos),
            });
        }

        // Plain integer; consume optional type suffix.
        self.consume_int_suffix();
        let text = self.text(start);
        Ok(Token {
            kind: TokenKind::IntLiteral(text),
            span: Span::new(start, self.pos),
        })
    }

    /// Common tail for hex (`0x…`) and binary (`0b…`) literals. Caller has
    /// already advanced past the prefix.
    fn lex_hex_or_bin_tail(&mut self, start: usize, is_hex: bool) -> Result<Token, LexError> {
        let digits_start = self.pos;
        while let Some(b) = self.peek(0) {
            let ok = if is_hex {
                b.is_ascii_hexdigit() || b == b'_'
            } else {
                matches!(b, b'0' | b'1' | b'_')
            };
            if ok {
                self.pos += 1;
            } else {
                break;
            }
        }
        if self.pos == digits_start {
            return Err(LexError::MalformedNumber {
                at: start,
                msg: if is_hex {
                    "hex literal has no digits after `0x`"
                } else {
                    "binary literal has no digits after `0b`"
                },
            });
        }
        self.consume_int_suffix();
        let text = self.text(start);
        Ok(Token {
            kind: if is_hex {
                TokenKind::HexLiteral(text)
            } else {
                TokenKind::BinLiteral(text)
            },
            span: Span::new(start, self.pos),
        })
    }

    /// Consume an optional integer type suffix (`u8`/`u16`/`u32`/`u64`/`usize`/
    /// `i8`/`i16`/`i32`/`i64`/`isize`) per §1.2 if present. The validity of
    /// the suffix string is verified at the type checker; the lexer accepts
    /// anything that looks like an identifier starting with `u` or `i` —
    /// downstream resolves it.
    fn consume_int_suffix(&mut self) {
        if matches!(self.peek(0), Some(b'u' | b'i')) {
            // Greedily consume identifier-shaped chars.
            while let Some(b) = self.peek(0) {
                if b.is_ascii_alphanumeric() || b == b'_' {
                    self.pos += 1;
                } else {
                    break;
                }
            }
        }
    }

    /// Consume an optional float type suffix (`f32` / `f64`) per §1.2.
    fn consume_float_suffix(&mut self) {
        if self.peek(0) == Some(b'f') {
            while let Some(b) = self.peek(0) {
                if b.is_ascii_alphanumeric() || b == b'_' {
                    self.pos += 1;
                } else {
                    break;
                }
            }
        }
    }

    /// Slice the source between `start` and `self.pos` as an owned String,
    /// asserting the bytes form valid UTF-8.
    fn text(&self, start: usize) -> String {
        std::str::from_utf8(&self.src[start..self.pos])
            .expect("literal bytes are valid UTF-8 by construction")
            .to_owned()
    }

    /// Lex `'X'` char literal. Caller has verified `peek(0) == Some(b'\'')`.
    fn lex_char_literal(&mut self, start: usize) -> Result<Token, LexError> {
        self.pos += 1; // opening `'`
        let ch = match self.peek(0) {
            None | Some(b'\n') => {
                return Err(LexError::UnterminatedCharLiteral { kind: "char", at: start });
            }
            Some(b'\\') => {
                self.pos += 1;
                self.consume_escape_char(start)?
            }
            Some(b) => {
                // Read one Unicode scalar from the source. The byte may be
                // the lead byte of a multi-byte UTF-8 sequence.
                let (ch, width) = self.decode_one_utf8_char(self.pos);
                self.pos += width;
                let _ = b;
                ch
            }
        };
        if self.peek(0) != Some(b'\'') {
            return Err(LexError::InvalidCharLiteral { at: start });
        }
        self.pos += 1; // closing `'`
        Ok(Token {
            kind: TokenKind::CharLiteral(ch),
            span: Span::new(start, self.pos),
        })
    }

    /// Lex `b'X'` byte literal. Caller has verified `peek(0) == Some(b'b')`
    /// and `peek(1) == Some(b'\'')`.
    fn lex_byte_literal(&mut self, start: usize) -> Result<Token, LexError> {
        self.pos += 2; // `b'`
        let value: u8 = match self.peek(0) {
            None | Some(b'\n') => {
                return Err(LexError::UnterminatedCharLiteral { kind: "byte", at: start });
            }
            Some(b'\\') => {
                self.pos += 1;
                let ch = self.consume_escape_char(start)?;
                if !ch.is_ascii() {
                    return Err(LexError::NonAsciiByteLiteral { at: start });
                }
                ch as u8
            }
            Some(b) => {
                if !b.is_ascii() {
                    return Err(LexError::NonAsciiByteLiteral { at: start });
                }
                self.pos += 1;
                b
            }
        };
        if self.peek(0) != Some(b'\'') {
            return Err(LexError::InvalidCharLiteral { at: start });
        }
        self.pos += 1; // closing `'`
        Ok(Token {
            kind: TokenKind::ByteLiteral(value),
            span: Span::new(start, self.pos),
        })
    }

    /// Lex `"…"` string literal. Caller has verified `peek(0) == Some(b'"')`.
    fn lex_string_literal(&mut self, start: usize) -> Result<Token, LexError> {
        self.pos += 1; // opening `"`
        let mut out = String::new();
        loop {
            match self.peek(0) {
                None => {
                    return Err(LexError::UnterminatedStringLiteral { at: start });
                }
                Some(b'"') => {
                    self.pos += 1;
                    return Ok(Token {
                        kind: TokenKind::StringLiteral(out),
                        span: Span::new(start, self.pos),
                    });
                }
                Some(b'\\') => {
                    self.pos += 1;
                    let ch = self.consume_escape_char(start)?;
                    out.push(ch);
                }
                Some(_) => {
                    let (ch, width) = self.decode_one_utf8_char(self.pos);
                    self.pos += width;
                    out.push(ch);
                }
            }
        }
    }

    /// Decode one UTF-8 character starting at `at`, returning `(char, width)`.
    /// Source is required to be valid UTF-8 (we receive a `&str` at the
    /// public boundary, which guarantees this), so this never fails.
    fn decode_one_utf8_char(&self, at: usize) -> (char, usize) {
        let s = std::str::from_utf8(&self.src[at..])
            .expect("source is valid UTF-8 by construction");
        let ch = s.chars().next().expect("at least one char available");
        (ch, ch.len_utf8())
    }

    /// Process an escape sequence starting *after* the leading `\\`.
    /// Recognises `\n \r \t \\ \' \" \0 \xHH` per §1.2 `escape :=`.
    fn consume_escape_char(&mut self, lit_start: usize) -> Result<char, LexError> {
        let (ch, at) = match self.peek(0) {
            None => {
                return Err(LexError::UnterminatedCharLiteral {
                    kind: "char",
                    at: lit_start,
                });
            }
            Some(b) => (b, self.pos),
        };
        self.pos += 1;
        Ok(match ch {
            b'n' => '\n',
            b'r' => '\r',
            b't' => '\t',
            b'\\' => '\\',
            b'\'' => '\'',
            b'"' => '"',
            b'0' => '\0',
            b'x' => {
                // Two hex digits.
                let h1 = self.peek(0).ok_or(LexError::InvalidEscape { ch: 'x', at })?;
                let h2 = self.peek(1).ok_or(LexError::InvalidEscape { ch: 'x', at })?;
                if !h1.is_ascii_hexdigit() || !h2.is_ascii_hexdigit() {
                    return Err(LexError::InvalidEscape { ch: 'x', at });
                }
                self.pos += 2;
                let high = ascii_hex_value(h1);
                let low = ascii_hex_value(h2);
                let value = (high << 4) | low;
                value as char
            }
            other => {
                return Err(LexError::InvalidEscape {
                    ch: other as char,
                    at,
                });
            }
        })
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
            // Decision #25 — `#hidden` field encapsulation modifier.
            "hidden" => TokenKind::KwHashHidden,
            // Reserved for v0.7+ — Decision #21 (shared automata via mutator
            // multivectors). Lexed so source compatibility holds across the
            // v0.7 transition; the parser will reject these with a
            // "reserved for v0.7" diagnostic in v0.1–v0.6.
            "shared" => TokenKind::KwHashShared,
            "lock" => TokenKind::KwHashLock,
            "with_lock" => TokenKind::KwHashWithLock,
            "reads" => TokenKind::KwHashReads,
            "rotor" => TokenKind::KwHashRotor,
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

// ─── Helpers ─────────────────────────────────────────────────────────────────

/// Convert a single ASCII hex digit byte (0–9, a–f, A–F) to its 0–15 value.
/// Caller must ensure the byte is a valid hex digit (`is_ascii_hexdigit`).
#[inline]
fn ascii_hex_value(b: u8) -> u8 {
    match b {
        b'0'..=b'9' => b - b'0',
        b'a'..=b'f' => b - b'a' + 10,
        b'A'..=b'F' => b - b'A' + 10,
        _ => unreachable!("ascii_hex_value called on non-hex byte"),
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
                   #staged #flush #audit \
                   #hidden \
                   #shared #lock #with_lock #reads #rotor";
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
            // Decision #25 — `#hidden` field encapsulation modifier.
            TokenKind::KwHashHidden,
            // Decision #21 — reserved for v0.7+ but lexed today.
            TokenKind::KwHashShared,
            TokenKind::KwHashLock,
            TokenKind::KwHashWithLock,
            TokenKind::KwHashReads,
            TokenKind::KwHashRotor,
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

    // ─── Slice 3: literal family ──────────────────────────────────────────

    #[test]
    fn integer_with_type_suffix() {
        assert_eq!(
            kinds("42u32 7i64 100usize"),
            vec![
                TokenKind::IntLiteral("42u32".into()),
                TokenKind::IntLiteral("7i64".into()),
                TokenKind::IntLiteral("100usize".into()),
                TokenKind::Eof,
            ],
        );
    }

    #[test]
    fn hex_literal() {
        assert_eq!(
            kinds("0xDEAD_BEEF 0xFFu8 0X1234"),
            vec![
                TokenKind::HexLiteral("0xDEAD_BEEF".into()),
                TokenKind::HexLiteral("0xFFu8".into()),
                TokenKind::HexLiteral("0X1234".into()),
                TokenKind::Eof,
            ],
        );
    }

    #[test]
    fn binary_literal() {
        assert_eq!(
            kinds("0b1010_0101 0b11u8"),
            vec![
                TokenKind::BinLiteral("0b1010_0101".into()),
                TokenKind::BinLiteral("0b11u8".into()),
                TokenKind::Eof,
            ],
        );
    }

    #[test]
    fn empty_hex_or_binary_errors() {
        assert!(matches!(
            tokenize("0x"),
            Err(LexError::MalformedNumber { .. }),
        ));
    }

    #[test]
    fn binary_prefix_falls_back_when_no_binary_digit() {
        // `0b3` is NOT a binary literal (3 is not a binary digit). Treat as
        // decimal `0` followed by ident `b3` so existing programs don't
        // accidentally lex strangely.
        assert_eq!(
            kinds("0b3"),
            vec![
                TokenKind::IntLiteral("0".into()),
                TokenKind::Ident("b3".into()),
                TokenKind::Eof,
            ],
        );
    }

    #[test]
    fn float_literal_basic() {
        assert_eq!(
            kinds("3.14 0.5 1_000.000_1"),
            vec![
                TokenKind::FloatLiteral("3.14".into()),
                TokenKind::FloatLiteral("0.5".into()),
                TokenKind::FloatLiteral("1_000.000_1".into()),
                TokenKind::Eof,
            ],
        );
    }

    #[test]
    fn float_with_exponent_and_suffix() {
        assert_eq!(
            kinds("1.0e5 2.5E-3 3.14f32 1.0e+10f64"),
            vec![
                TokenKind::FloatLiteral("1.0e5".into()),
                TokenKind::FloatLiteral("2.5E-3".into()),
                TokenKind::FloatLiteral("3.14f32".into()),
                TokenKind::FloatLiteral("1.0e+10f64".into()),
                TokenKind::Eof,
            ],
        );
    }

    #[test]
    fn dot_after_int_does_not_eat_method_call() {
        // `tup.0` is field-access, NOT a float `tup` followed by `.0`.
        // And `1..5` is a range, NOT a float `1.` followed by `.5`.
        assert_eq!(
            kinds("tup.0 1..5"),
            vec![
                TokenKind::Ident("tup".into()),
                TokenKind::Dot,
                TokenKind::IntLiteral("0".into()),
                TokenKind::IntLiteral("1".into()),
                TokenKind::DotDot,
                TokenKind::IntLiteral("5".into()),
                TokenKind::Eof,
            ],
        );
    }

    #[test]
    fn float_requires_digits_after_dot() {
        // Per §1.2, `1.` alone is not a float; tokenises as int + dot.
        assert_eq!(
            kinds("1."),
            vec![
                TokenKind::IntLiteral("1".into()),
                TokenKind::Dot,
                TokenKind::Eof,
            ],
        );
    }

    #[test]
    fn float_exponent_without_digits_errors() {
        assert!(matches!(
            tokenize("1.0e"),
            Err(LexError::MalformedNumber { .. }),
        ));
    }

    #[test]
    fn char_literal_basic_and_escape() {
        assert_eq!(
            kinds(r"'a' 'Z' '\n' '\\' '\'' '\0' '\x41'"),
            vec![
                TokenKind::CharLiteral('a'),
                TokenKind::CharLiteral('Z'),
                TokenKind::CharLiteral('\n'),
                TokenKind::CharLiteral('\\'),
                TokenKind::CharLiteral('\''),
                TokenKind::CharLiteral('\0'),
                TokenKind::CharLiteral('A'), // 0x41 = 'A'
                TokenKind::Eof,
            ],
        );
    }

    #[test]
    fn byte_literal_basic_and_escape() {
        assert_eq!(
            kinds(r"b'a' b'\n' b'\x41'"),
            vec![
                TokenKind::ByteLiteral(b'a'),
                TokenKind::ByteLiteral(b'\n'),
                TokenKind::ByteLiteral(0x41),
                TokenKind::Eof,
            ],
        );
    }

    #[test]
    fn byte_literal_rejects_non_ascii() {
        // Multi-byte UTF-8 (é = 0xC3 0xA9) is not ASCII and is rejected
        // with E0111.
        assert!(matches!(
            tokenize("b'é'"),
            Err(LexError::NonAsciiByteLiteral { .. }),
        ));
    }

    #[test]
    fn string_literal_basic_and_escape() {
        assert_eq!(
            kinds(r#""hello" "with\nnewline" "quoted\"text""#),
            vec![
                TokenKind::StringLiteral("hello".into()),
                TokenKind::StringLiteral("with\nnewline".into()),
                TokenKind::StringLiteral("quoted\"text".into()),
                TokenKind::Eof,
            ],
        );
    }

    #[test]
    fn string_literal_with_hex_escape() {
        assert_eq!(
            kinds(r#""\x41\x42""#),
            vec![TokenKind::StringLiteral("AB".into()), TokenKind::Eof],
        );
    }

    #[test]
    fn unterminated_string_errors() {
        assert!(matches!(
            tokenize(r#""never closed"#),
            Err(LexError::UnterminatedStringLiteral { .. }),
        ));
    }

    #[test]
    fn invalid_escape_errors() {
        assert!(matches!(
            tokenize(r"'\q'"),
            Err(LexError::InvalidEscape { ch: 'q', .. }),
        ));
    }

    #[test]
    fn empty_char_literal_errors() {
        assert!(matches!(
            tokenize("''"),
            Err(LexError::InvalidCharLiteral { .. }),
        ));
    }

    #[test]
    fn ident_starting_with_b_is_not_byte_literal() {
        // `b` followed by `'` → byte literal. `b` followed by anything
        // else (including alphanumerics) → identifier.
        assert_eq!(
            kinds("b basic blink_count"),
            vec![
                TokenKind::Ident("b".into()),
                TokenKind::Ident("basic".into()),
                TokenKind::Ident("blink_count".into()),
                TokenKind::Eof,
            ],
        );
    }

    #[test]
    fn realistic_decision15_sugar_with_hex() {
        // `Usart1.CR1 = 0xD;` — the kind of register write Decision #15
        // sugar enables, with hex literal RHS.
        assert_eq!(
            kinds("Usart1.CR1 = 0xD;"),
            vec![
                TokenKind::Ident("Usart1".into()),
                TokenKind::Dot,
                TokenKind::Ident("CR1".into()),
                TokenKind::Eq,
                TokenKind::HexLiteral("0xD".into()),
                TokenKind::Semi,
                TokenKind::Eof,
            ],
        );
    }
}
