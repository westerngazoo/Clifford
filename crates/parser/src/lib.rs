//! # clifford-parser
//!
//! Recursive-descent parser for the Clifford language. Implements §2 (Grammar)
//! and §3 (Parser Behavior) of `docs/CLIFFORD_SPEC.md`. Phase 0 of the
//! implementation roadmap (§11).
//!
//! ## Approach
//!
//! Recursive descent with one-token lookahead, augmented by sigil-driven
//! dispatch (§3 of the spec):
//!
//! 1. Sigil dispatch at item position: the leading sigil (`@`, `#`) selects
//!    which item-grammar to enter.
//! 2. Sigil dispatch at statement position: inside a `#`-context body, leading
//!    `#mutate`, `#>`, narrow unsafe primitives, etc. select the statement form.
//! 3. Generic vs. less-than disambiguation: bounded backtracking when `<`
//!    could begin a generic argument list.
//! 4. Inline effect metadata: `#effect`/`#interrupt` declarations consume zero
//!    or more `effect_meta` clauses before the body block.
//! 5. `#states` omission default (Decision #5): missing `#states` ⇒ inserted
//!    synthetic `[Ready]` and the AST is marked as a *monoid automaton*.
//! 6. Register-block automaton dispatch (Decision #6): `#address` clause marks
//!    the AST node as a register block; every field requires `#offset`.
//! 7. Call-site context classification (Refinement #5b generalisation):
//!    `#> name(args)` callees are tagged Transition / Identity / Generic per
//!    callee kind during name resolution (this is `clifford-resolve`'s job,
//!    not the parser's — but the parser preserves the call-site for it).
//! 8. Interface-method dispatch (Decision #16): `#> Name::method(args)` where
//!    `Name` is a generic parameter is recorded as a `Generic` call site at
//!    resolution time; the parser produces the call-form, resolution decides.
//! 9. Sigma-loop parsing (Decision #14): the `sigma` keyword opens a
//!    `sigma_expr`; bound annotations attached to the iteration variable.
//!
//! ## Error recovery
//!
//! Per CLAUDE.md §6 Phase 0, the parser produces a partial AST and reports
//! all errors, not just the first. Resync points are at item boundaries,
//! statement separators, and closing braces. (First slice is fail-fast;
//! resync arrives in a follow-up.)
//!
//! ## Round-trip property
//!
//! `source → AST → pretty-print → AST` is identity modulo whitespace
//! (CLAUDE.md §6 Phase 0 property test requirement; pretty-printer + property
//! test land alongside the AST having more than name+span).
//!
//! ## Implementation status
//!
//! First slice (this PR): item-position sigil dispatch. Parses bare top-level
//! `@fn name() { }` and `#automaton Name { }` forms — no parameters, no
//! return type, no body content, no trait list, no automaton fields. Just
//! enough to validate the dispatch shape and the AST plumbing.

#![forbid(unsafe_code)]
#![warn(missing_docs)]

use clifford_ast::{AutomatonDecl, FnDecl, Item, Program};
use clifford_lexer::{Span, Token, TokenKind};
use thiserror::Error;

/// Errors produced during parsing.
///
/// Per CLAUDE.md §3.4, every error carries a stable error code in the `E02xx`
/// range.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum ParseError {
    /// An unexpected token was encountered at item position.
    #[error("E0203: expected an item (@fn, #automaton, …), found {found:?} at byte {at}")]
    ExpectedItem {
        /// The token kind that was found instead of an item-starter.
        found: TokenKind,
        /// Byte offset where the unexpected token began.
        at: usize,
    },

    /// A token was expected at a known position but not found.
    #[error("E0204: expected {expected}, found {found:?} at byte {at}")]
    Expected {
        /// What the parser was looking for, in human-readable form.
        expected: &'static str,
        /// The token kind that was found instead.
        found: TokenKind,
        /// Byte offset where the unexpected token began.
        at: usize,
    },

    /// Source ended in the middle of an item being parsed.
    #[error("E0205: unexpected end of input while parsing {context}")]
    UnexpectedEof {
        /// What was being parsed when EOF arrived.
        context: &'static str,
    },
}

/// Parse a token stream into a [`Program`] (the root AST node).
///
/// # Examples
///
/// ```
/// use clifford_lexer::tokenize;
/// use clifford_parser::parse;
///
/// // Empty input → empty program.
/// let tokens = tokenize("").unwrap();
/// let program = parse(&tokens).unwrap();
/// assert!(program.items.is_empty());
///
/// // Single @fn declaration.
/// let tokens = tokenize("@fn main() { }").unwrap();
/// let program = parse(&tokens).unwrap();
/// assert_eq!(program.items.len(), 1);
/// ```
///
/// # Errors
///
/// Returns the first [`ParseError`] encountered. Error recovery (collecting
/// all errors, resyncing at item boundaries) lands in a follow-up per
/// CLAUDE.md §6 Phase 0.
pub fn parse(tokens: &[Token]) -> Result<Program, ParseError> {
    let mut p = Parser::new(tokens);
    let mut items = Vec::new();
    while !p.at_eof() {
        items.push(p.parse_item()?);
    }
    let span = match (items.first(), items.last()) {
        (Some(first), Some(last)) => Span::new(first.span().start, last.span().end),
        _ => Span::default(),
    };
    Ok(Program { span, items })
}

// ─── Internal parser ─────────────────────────────────────────────────────────

/// Token-stream cursor for the recursive-descent parser.
struct Parser<'t> {
    tokens: &'t [Token],
    pos: usize,
}

impl<'t> Parser<'t> {
    fn new(tokens: &'t [Token]) -> Self {
        Self { tokens, pos: 0 }
    }

    fn peek(&self) -> &Token {
        // Caller must check at_eof() before driving deeper. The lexer
        // guarantees a trailing TokenKind::Eof so indexing is always valid.
        &self.tokens[self.pos]
    }

    fn at_eof(&self) -> bool {
        matches!(self.peek().kind, TokenKind::Eof)
    }

    fn advance(&mut self) -> &Token {
        let t = &self.tokens[self.pos];
        if !matches!(t.kind, TokenKind::Eof) {
            self.pos += 1;
        }
        t
    }

    /// Consume an identifier or fail with a useful diagnostic.
    fn expect_ident(&mut self, expected: &'static str) -> Result<(String, Span), ParseError> {
        let t = self.peek().clone();
        match t.kind {
            TokenKind::Ident(name) => {
                self.advance();
                Ok((name, t.span))
            }
            TokenKind::Eof => Err(ParseError::UnexpectedEof { context: expected }),
            other => Err(ParseError::Expected {
                expected,
                found: other,
                at: t.span.start,
            }),
        }
    }

    /// Consume a token of an exact kind or fail.
    fn expect(&mut self, kind: TokenKind, expected: &'static str) -> Result<Span, ParseError> {
        let t = self.peek().clone();
        if t.kind == kind {
            self.advance();
            Ok(t.span)
        } else if matches!(t.kind, TokenKind::Eof) {
            Err(ParseError::UnexpectedEof { context: expected })
        } else {
            Err(ParseError::Expected {
                expected,
                found: t.kind,
                at: t.span.start,
            })
        }
    }

    /// Parse one top-level item, dispatching by the leading sigil-token.
    ///
    /// First slice handles `@fn` and `#automaton` only; the full §2.1 item
    /// catalogue (`@type`, `@trait`, `@module`, top-level `#effect`,
    /// `#interrupt`, `#interface`, `#impl`, `#test`, `static`, `const`,
    /// `extern_block`, `use_decl`, `@sequential` attribute) lands in
    /// subsequent slices.
    fn parse_item(&mut self) -> Result<Item, ParseError> {
        let lead = self.peek().clone();
        match lead.kind {
            TokenKind::KwAtFn => self.parse_fn_decl(lead.span.start).map(Item::Fn),
            TokenKind::KwHashAutomaton => {
                self.parse_automaton_decl(lead.span.start).map(Item::Automaton)
            }
            other => Err(ParseError::ExpectedItem {
                found: other,
                at: lead.span.start,
            }),
        }
    }

    /// Parse `@fn name() { }`.
    ///
    /// First-slice form. Generic parameters, value parameters, return type,
    /// trait list, where-clause, extern modifier, and body content all
    /// arrive in subsequent slices.
    fn parse_fn_decl(&mut self, start: usize) -> Result<FnDecl, ParseError> {
        self.advance(); // `@fn`
        let (name, _) = self.expect_ident("function name after `@fn`")?;
        self.expect(TokenKind::LParen, "`(` after function name")?;
        self.expect(TokenKind::RParen, "`)` (empty parameter list in this slice)")?;
        self.expect(TokenKind::LBrace, "`{` to open function body")?;
        let close = self.expect(TokenKind::RBrace, "`}` to close function body")?;
        Ok(FnDecl {
            name,
            span: Span::new(start, close.end),
        })
    }

    /// Parse `#automaton Name { }`.
    ///
    /// First-slice form. `#address` register-block annotation, `#basis`
    /// clause, `#states` list, automaton fields, and named transitions all
    /// arrive in subsequent slices.
    fn parse_automaton_decl(&mut self, start: usize) -> Result<AutomatonDecl, ParseError> {
        self.advance(); // `#automaton`
        let (name, _) = self.expect_ident("automaton name after `#automaton`")?;
        self.expect(TokenKind::LBrace, "`{` to open automaton body")?;
        let close = self.expect(TokenKind::RBrace, "`}` to close automaton body")?;
        Ok(AutomatonDecl {
            name,
            span: Span::new(start, close.end),
        })
    }
}

// ─── Tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use clifford_ast::Layer;
    use clifford_lexer::tokenize;

    fn parse_str(src: &str) -> Result<Program, ParseError> {
        let tokens = tokenize(src).expect("tokenize");
        parse(&tokens)
    }

    #[test]
    fn empty_input_parses_to_empty_program() {
        let p = parse_str("").expect("parse empty");
        assert!(p.items.is_empty());
        assert_eq!(p.span, Span::default());
    }

    #[test]
    fn whitespace_only_is_empty() {
        let p = parse_str("   \n\n  ").expect("parse whitespace");
        assert!(p.items.is_empty());
    }

    #[test]
    fn single_fn_decl() {
        let p = parse_str("@fn main() { }").expect("parse @fn main");
        assert_eq!(p.items.len(), 1);
        match &p.items[0] {
            Item::Fn(decl) => {
                assert_eq!(decl.name, "main");
                assert_eq!(decl.span.start, 0);
                // span should cover through the closing brace
                assert!(decl.span.end >= "@fn main() {".len());
            }
            other => panic!("expected Fn, got {:?}", other),
        }
        assert_eq!(p.items[0].layer(), Layer::Functional);
    }

    #[test]
    fn single_automaton_decl() {
        let p = parse_str("#automaton Counter { }").expect("parse #automaton");
        assert_eq!(p.items.len(), 1);
        match &p.items[0] {
            Item::Automaton(decl) => {
                assert_eq!(decl.name, "Counter");
                assert_eq!(decl.span.start, 0);
            }
            other => panic!("expected Automaton, got {:?}", other),
        }
        assert_eq!(p.items[0].layer(), Layer::Imperative);
    }

    #[test]
    fn multiple_items_in_source_order() {
        let p = parse_str(
            "@fn first() { }\n\
             #automaton Middle { }\n\
             @fn last() { }",
        )
        .expect("parse multi-item");
        assert_eq!(p.items.len(), 3);

        match &p.items[0] {
            Item::Fn(d) => assert_eq!(d.name, "first"),
            other => panic!("expected Fn, got {:?}", other),
        }
        match &p.items[1] {
            Item::Automaton(d) => assert_eq!(d.name, "Middle"),
            other => panic!("expected Automaton, got {:?}", other),
        }
        match &p.items[2] {
            Item::Fn(d) => assert_eq!(d.name, "last"),
            other => panic!("expected Fn, got {:?}", other),
        }
    }

    #[test]
    fn program_span_covers_first_to_last_item() {
        let src = "@fn a() { } @fn b() { }";
        let p = parse_str(src).expect("parse two @fn");
        assert_eq!(p.span.start, 0);
        assert_eq!(p.span.end, src.len());
    }

    #[test]
    fn comments_between_items_are_invisible() {
        let p = parse_str(
            "// preface\n\
             @fn a() { }\n\
             /* block */\n\
             #automaton B { }\n\
             /// doc on next item — currently skipped\n\
             @fn c() { }",
        )
        .expect("parse with comments");
        assert_eq!(p.items.len(), 3);
    }

    #[test]
    fn fn_must_have_name() {
        // `@fn ()` — no name after the sigil.
        let err = parse_str("@fn ()").expect_err("missing fn name should error");
        assert!(matches!(
            err,
            ParseError::Expected {
                expected: "function name after `@fn`",
                ..
            }
        ));
    }

    #[test]
    fn automaton_must_have_name() {
        let err = parse_str("#automaton { }").expect_err("missing automaton name");
        assert!(matches!(
            err,
            ParseError::Expected {
                expected: "automaton name after `#automaton`",
                ..
            }
        ));
    }

    #[test]
    fn fn_must_have_parens() {
        let err = parse_str("@fn main { }").expect_err("missing parens");
        assert!(matches!(
            err,
            ParseError::Expected {
                expected: "`(` after function name",
                ..
            }
        ));
    }

    #[test]
    fn fn_body_must_be_braced() {
        let err = parse_str("@fn main()").expect_err("missing body");
        assert!(matches!(err, ParseError::UnexpectedEof { .. }));
    }

    #[test]
    fn unknown_top_level_token_is_expected_item_error() {
        let err = parse_str("let x = 1;").expect_err("let is not a top-level item in this slice");
        assert!(matches!(err, ParseError::ExpectedItem { .. }));
    }

    #[test]
    fn parser_is_deterministic() {
        let src = "@fn a() { } #automaton B { } @fn c() { }";
        let a = parse_str(src).expect("first parse");
        let b = parse_str(src).expect("second parse");
        assert_eq!(a, b);
    }
}
