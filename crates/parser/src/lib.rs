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

use clifford_ast::{
    AccessType, ArraySize, ArrayType, AutomatonDecl, EffectDecl, FnDecl, FnType, ImplDecl,
    InterfaceDecl, InterruptDecl, Item, PathType, PriorityLevel, PrimitiveType, Program,
    RefType, SequentialAttr, SliceType, TestDecl, TraitRef, TupleType, TypeExpr, TypeKind,
};
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
    /// Slice 2 handles: `@fn`, `#automaton`, `#effect`, `#interrupt`,
    /// `#interface`, `#impl`, `#test`, and the `@sequential` attribute.
    /// Still deferred per §2.1: `@type`, `@trait`, `@module`, `static`,
    /// `const`, `extern_block`, `use_decl` — these need type and value
    /// expression parsing which is slice-3+ work.
    fn parse_item(&mut self) -> Result<Item, ParseError> {
        let lead = self.peek().clone();
        match lead.kind {
            TokenKind::KwAtFn => self.parse_fn_decl(lead.span.start).map(Item::Fn),
            TokenKind::KwHashAutomaton => {
                self.parse_automaton_decl(lead.span.start).map(Item::Automaton)
            }
            TokenKind::KwHashEffect => {
                self.parse_effect_decl(lead.span.start).map(Item::Effect)
            }
            TokenKind::KwHashInterrupt => {
                self.parse_interrupt_decl(lead.span.start).map(Item::Interrupt)
            }
            TokenKind::KwHashInterface => {
                self.parse_interface_decl(lead.span.start).map(Item::Interface)
            }
            TokenKind::KwHashImpl => self.parse_impl_decl(lead.span.start).map(Item::Impl),
            TokenKind::KwHashTest => self.parse_test_decl(lead.span.start).map(Item::Test),
            TokenKind::KwAtSequential => {
                self.parse_sequential_attr(lead.span.start).map(Item::Sequential)
            }
            other => Err(ParseError::ExpectedItem {
                found: other,
                at: lead.span.start,
            }),
        }
    }

    /// Parse `@fn name() -> T { }`.
    ///
    /// Slice-3 form: optional return type after `->`. Generic parameters,
    /// value parameters, trait list, where-clause, extern modifier, and
    /// body content all arrive in subsequent slices.
    fn parse_fn_decl(&mut self, start: usize) -> Result<FnDecl, ParseError> {
        self.advance(); // `@fn`
        let (name, _) = self.expect_ident("function name after `@fn`")?;
        self.expect(TokenKind::LParen, "`(` after function name")?;
        self.expect(TokenKind::RParen, "`)` (empty parameter list in this slice)")?;
        let return_type = if matches!(self.peek().kind, TokenKind::Arrow) {
            self.advance(); // `->`
            Some(self.parse_type()?)
        } else {
            None
        };
        self.expect(TokenKind::LBrace, "`{` to open function body")?;
        let close = self.expect(TokenKind::RBrace, "`}` to close function body")?;
        Ok(FnDecl {
            name,
            return_type,
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

    /// Parse `#effect name() #mutates: [A, B] { }`.
    ///
    /// Slice-2 form. The empty parameter list `()` is required; parameters
    /// land in slice 3. Body content (statements) lands in slice 4. The
    /// `#mutates` clause is required (per §2.5 notes for `#effect`); it may
    /// be empty (`#mutates: []`) for pure effects. `#cannot_mutate` is
    /// optional. Other effect_meta clauses (`#invariant`, `#atomic`) land
    /// in subsequent slices.
    fn parse_effect_decl(&mut self, start: usize) -> Result<EffectDecl, ParseError> {
        self.advance(); // `#effect`
        let (name, _) = self.expect_ident("effect name after `#effect`")?;
        self.expect(TokenKind::LParen, "`(` after effect name")?;
        self.expect(TokenKind::RParen, "`)` (empty parameter list in this slice)")?;

        let (mutates, cannot_mutate) = self.parse_effect_meta_for_effect()?;

        self.expect(TokenKind::LBrace, "`{` to open effect body")?;
        let close = self.expect(TokenKind::RBrace, "`}` to close effect body")?;
        Ok(EffectDecl {
            name,
            mutates,
            cannot_mutate,
            span: Span::new(start, close.end),
        })
    }

    /// Parse `#interrupt NAME() #mutates: [A] #priority: HIGH { }`.
    ///
    /// Per §2.5 notes, `#interrupt` requires both `#mutates` and `#priority`.
    /// The name becomes the linker symbol per Decision #10.
    fn parse_interrupt_decl(&mut self, start: usize) -> Result<InterruptDecl, ParseError> {
        self.advance(); // `#interrupt`
        let (name, _) = self.expect_ident("interrupt vector name after `#interrupt`")?;
        self.expect(TokenKind::LParen, "`(` after interrupt name")?;
        self.expect(TokenKind::RParen, "`)` (empty parameter list in this slice)")?;

        let (mutates, priority) = self.parse_effect_meta_for_interrupt(start)?;

        self.expect(TokenKind::LBrace, "`{` to open interrupt body")?;
        let close = self.expect(TokenKind::RBrace, "`}` to close interrupt body")?;
        Ok(InterruptDecl {
            name,
            mutates,
            priority,
            span: Span::new(start, close.end),
        })
    }

    /// Parse `#interface Name { }` (Decision #16).
    ///
    /// Slice-2 form. Empty body for now; method signatures land in slice 3.
    fn parse_interface_decl(&mut self, start: usize) -> Result<InterfaceDecl, ParseError> {
        self.advance(); // `#interface`
        let (name, _) = self.expect_ident("interface name after `#interface`")?;
        self.expect(TokenKind::LBrace, "`{` to open interface body")?;
        let close = self.expect(TokenKind::RBrace, "`}` to close interface body")?;
        Ok(InterfaceDecl {
            name,
            span: Span::new(start, close.end),
        })
    }

    /// Parse `#impl Interface for Automaton { }` (Decision #16).
    ///
    /// Slice-2 form. Empty body for now; method bodies land in slice 3
    /// alongside statement/expression parsing. The `for` is the bare
    /// keyword `for` (not a sigil-form), borrowing the §1.3 `for` keyword
    /// for its second use position.
    fn parse_impl_decl(&mut self, start: usize) -> Result<ImplDecl, ParseError> {
        self.advance(); // `#impl`
        let (interface_name, _) =
            self.expect_ident("interface name after `#impl`")?;
        self.expect(TokenKind::KwFor, "`for` between interface and automaton")?;
        let (automaton_name, _) =
            self.expect_ident("automaton name after `for`")?;
        self.expect(TokenKind::LBrace, "`{` to open impl body")?;
        let close = self.expect(TokenKind::RBrace, "`}` to close impl body")?;
        Ok(ImplDecl {
            interface_name,
            automaton_name,
            span: Span::new(start, close.end),
        })
    }

    /// Parse `#test "description" { }` (Decision #7).
    ///
    /// Slice-2 form. Body content lands in slice 4.
    fn parse_test_decl(&mut self, start: usize) -> Result<TestDecl, ParseError> {
        self.advance(); // `#test`
        let t = self.peek().clone();
        let description = match t.kind {
            TokenKind::StringLiteral(s) => {
                self.advance();
                s
            }
            TokenKind::Eof => {
                return Err(ParseError::UnexpectedEof {
                    context: "string-literal description after `#test`",
                });
            }
            other => {
                return Err(ParseError::Expected {
                    expected: "string-literal description after `#test`",
                    found: other,
                    at: t.span.start,
                });
            }
        };
        self.expect(TokenKind::LBrace, "`{` to open test body")?;
        let close = self.expect(TokenKind::RBrace, "`}` to close test body")?;
        Ok(TestDecl {
            description,
            span: Span::new(start, close.end),
        })
    }

    /// Parse `@sequential(A, B);` (Decision #11).
    fn parse_sequential_attr(&mut self, start: usize) -> Result<SequentialAttr, ParseError> {
        self.advance(); // `@sequential`
        self.expect(TokenKind::LParen, "`(` after `@sequential`")?;
        let (a, _) = self.expect_ident("first automaton name in `@sequential(…, …)`")?;
        self.expect(TokenKind::Comma, "`,` between sequential automaton names")?;
        let (b, _) = self.expect_ident("second automaton name in `@sequential(…, …)`")?;
        self.expect(TokenKind::RParen, "`)` to close `@sequential(…)`")?;
        let close = self.expect(TokenKind::Semi, "`;` to terminate `@sequential` attribute")?;
        Ok(SequentialAttr {
            a,
            b,
            span: Span::new(start, close.end),
        })
    }

    // ─── effect_meta parsing helpers ──────────────────────────────────────

    /// Parse the metadata clauses of an `#effect` declaration (slice-2 set).
    ///
    /// Slice-2 supports `#mutates: [...]` (required, possibly empty) and
    /// `#cannot_mutate: [...]` (optional). Order: `#mutates` must come first
    /// for now; richer reordering and `#invariant` / `#atomic` come in
    /// subsequent slices.
    fn parse_effect_meta_for_effect(
        &mut self,
    ) -> Result<(Vec<String>, Vec<String>), ParseError> {
        self.expect(TokenKind::KwHashMutates, "`#mutates: [...]` clause is required for `#effect`")?;
        self.expect(TokenKind::Colon, "`:` after `#mutates`")?;
        let mutates = self.parse_ident_list_in_brackets()?;

        let cannot_mutate = if matches!(self.peek().kind, TokenKind::KwHashCannotMutate) {
            self.advance(); // `#cannot_mutate`
            self.expect(TokenKind::Colon, "`:` after `#cannot_mutate`")?;
            self.parse_ident_list_in_brackets()?
        } else {
            Vec::new()
        };

        Ok((mutates, cannot_mutate))
    }

    /// Parse the metadata clauses of an `#interrupt` declaration.
    ///
    /// `#interrupt` requires both `#mutates` and `#priority` per §2.5.
    fn parse_effect_meta_for_interrupt(
        &mut self,
        decl_start: usize,
    ) -> Result<(Vec<String>, PriorityLevel), ParseError> {
        self.expect(
            TokenKind::KwHashMutates,
            "`#mutates: [...]` clause is required for `#interrupt`",
        )?;
        self.expect(TokenKind::Colon, "`:` after `#mutates`")?;
        let mutates = self.parse_ident_list_in_brackets()?;

        self.expect(
            TokenKind::KwHashPriority,
            "`#priority: …` clause is required for `#interrupt`",
        )?;
        self.expect(TokenKind::Colon, "`:` after `#priority`")?;
        let priority = self.parse_priority_level(decl_start)?;
        Ok((mutates, priority))
    }

    /// Parse `[ ident (, ident)* ]` or `[]`.
    fn parse_ident_list_in_brackets(&mut self) -> Result<Vec<String>, ParseError> {
        self.expect(TokenKind::LBracket, "`[` to open identifier list")?;
        let mut idents = Vec::new();
        loop {
            match self.peek().kind {
                TokenKind::RBracket => {
                    self.advance();
                    return Ok(idents);
                }
                TokenKind::Eof => {
                    return Err(ParseError::UnexpectedEof {
                        context: "identifier list",
                    });
                }
                _ => {}
            }
            let (name, _) = self.expect_ident("identifier in list")?;
            idents.push(name);
            match self.peek().kind {
                TokenKind::Comma => {
                    self.advance();
                }
                TokenKind::RBracket => {
                    self.advance();
                    return Ok(idents);
                }
                _ => {
                    let t = self.peek().clone();
                    return Err(ParseError::Expected {
                        expected: "`,` or `]` in identifier list",
                        found: t.kind,
                        at: t.span.start,
                    });
                }
            }
        }
    }

    /// Parse `LOW | MEDIUM | HIGH | <integer>` per §2.5.
    fn parse_priority_level(&mut self, decl_start: usize) -> Result<PriorityLevel, ParseError> {
        let t = self.peek().clone();
        match t.kind {
            TokenKind::Ident(ref name) => {
                let level = match name.as_str() {
                    "LOW" => PriorityLevel::Low,
                    "MEDIUM" => PriorityLevel::Medium,
                    "HIGH" => PriorityLevel::High,
                    _ => {
                        return Err(ParseError::Expected {
                            expected: "`LOW`, `MEDIUM`, `HIGH`, or integer literal as `#priority` value",
                            found: t.kind,
                            at: t.span.start,
                        });
                    }
                };
                self.advance();
                let _ = decl_start;
                Ok(level)
            }
            TokenKind::IntLiteral(text) => {
                self.advance();
                Ok(PriorityLevel::Numeric(text))
            }
            TokenKind::Eof => Err(ParseError::UnexpectedEof {
                context: "`#priority` value",
            }),
            other => Err(ParseError::Expected {
                expected: "`LOW`, `MEDIUM`, `HIGH`, or integer literal as `#priority` value",
                found: other,
                at: t.span.start,
            }),
        }
    }

    // ─── Type expressions (§2.7) ──────────────────────────────────────────

    /// Parse one type expression. Recursive (types contain types).
    ///
    /// The dispatch is single-token-lookahead — the leading token uniquely
    /// determines which type form follows:
    ///
    /// - `&`         → ref_type
    /// - `access`    → access_type
    /// - `[`         → array_type or slice_type (lookahead for `;`)
    /// - `(`         → tuple_type or unit (`()`)
    /// - `@fn`       → fn_type
    /// - identifier  → primitive_type or path (single-segment paths whose
    ///                 name matches a primitive resolve to `Primitive`)
    /// - `Self`      → path of one segment `Self`
    fn parse_type(&mut self) -> Result<TypeExpr, ParseError> {
        let t = self.peek().clone();
        let start = t.span.start;
        match t.kind {
            TokenKind::Amp => self.parse_ref_type(start),
            TokenKind::KwAccess => self.parse_access_type(start),
            TokenKind::LBracket => self.parse_array_or_slice_type(start),
            TokenKind::LParen => self.parse_tuple_or_unit_type(start),
            TokenKind::KwAtFn => self.parse_fn_type(start),
            TokenKind::Ident(_) | TokenKind::KwSelfType => self.parse_path_or_primitive(start),
            TokenKind::Eof => Err(ParseError::UnexpectedEof {
                context: "type expression",
            }),
            other => Err(ParseError::Expected {
                expected: "type expression",
                found: other,
                at: start,
            }),
        }
    }

    /// Parse `&T` or `&mut T`.
    fn parse_ref_type(&mut self, start: usize) -> Result<TypeExpr, ParseError> {
        self.advance(); // `&`
        let mutable = if matches!(self.peek().kind, TokenKind::KwMut) {
            self.advance();
            true
        } else {
            false
        };
        let inner = self.parse_type()?;
        let end = inner.span.end;
        Ok(TypeExpr {
            kind: TypeKind::Ref(RefType {
                mutable,
                inner: Box::new(inner),
            }),
            span: Span::new(start, end),
        })
    }

    /// Parse `access<T>` or `access const<T>` (Decision #19).
    fn parse_access_type(&mut self, start: usize) -> Result<TypeExpr, ParseError> {
        self.advance(); // `access`
        let is_const = if matches!(self.peek().kind, TokenKind::KwConst) {
            self.advance();
            true
        } else {
            false
        };
        self.expect(TokenKind::Lt, "`<` after `access`")?;
        let inner = self.parse_type()?;
        let close = self.expect(TokenKind::Gt, "`>` to close `access<…>`")?;
        Ok(TypeExpr {
            kind: TypeKind::Access(AccessType {
                is_const,
                inner: Box::new(inner),
            }),
            span: Span::new(start, close.end),
        })
    }

    /// Parse `[T; N]` (array) or `[T]` (slice). Lookahead on the token after
    /// the element type disambiguates.
    fn parse_array_or_slice_type(&mut self, start: usize) -> Result<TypeExpr, ParseError> {
        self.advance(); // `[`
        let element = self.parse_type()?;
        let next = self.peek().clone();
        match next.kind {
            TokenKind::Semi => {
                self.advance(); // `;`
                let size_tok = self.peek().clone();
                let size = match size_tok.kind {
                    TokenKind::IntLiteral(text) => {
                        self.advance();
                        ArraySize::IntLiteral(text)
                    }
                    TokenKind::Eof => {
                        return Err(ParseError::UnexpectedEof {
                            context: "array size literal",
                        });
                    }
                    other => {
                        return Err(ParseError::Expected {
                            expected: "integer literal as array size",
                            found: other,
                            at: size_tok.span.start,
                        });
                    }
                };
                let close = self.expect(TokenKind::RBracket, "`]` to close array type")?;
                Ok(TypeExpr {
                    kind: TypeKind::Array(ArrayType {
                        element: Box::new(element),
                        size,
                    }),
                    span: Span::new(start, close.end),
                })
            }
            TokenKind::RBracket => {
                self.advance(); // `]`
                let end = next.span.end;
                Ok(TypeExpr {
                    kind: TypeKind::Slice(SliceType {
                        element: Box::new(element),
                    }),
                    span: Span::new(start, end),
                })
            }
            TokenKind::Eof => Err(ParseError::UnexpectedEof {
                context: "array or slice type",
            }),
            other => Err(ParseError::Expected {
                expected: "`;` (array) or `]` (slice) after element type",
                found: other,
                at: next.span.start,
            }),
        }
    }

    /// Parse `()` (unit), `(T)` (parenthesised type), or `(T1, T2, …)` (tuple
    /// with ≥ 2 elements per §2.7).
    fn parse_tuple_or_unit_type(&mut self, start: usize) -> Result<TypeExpr, ParseError> {
        self.advance(); // `(`
        // Empty `()` → unit.
        if matches!(self.peek().kind, TokenKind::RParen) {
            let close = self.expect(TokenKind::RParen, "internal: just peeked RParen")?;
            return Ok(TypeExpr {
                kind: TypeKind::Unit,
                span: Span::new(start, close.end),
            });
        }
        let first = self.parse_type()?;
        match self.peek().kind {
            TokenKind::RParen => {
                // `(T)` — parenthesised type, not a 1-tuple.
                let close = self.expect(TokenKind::RParen, "internal: just peeked RParen")?;
                Ok(TypeExpr {
                    kind: first.kind,
                    span: Span::new(start, close.end),
                })
            }
            TokenKind::Comma => {
                self.advance(); // `,`
                let mut elements = vec![first];
                loop {
                    if matches!(self.peek().kind, TokenKind::RParen) {
                        break;
                    }
                    elements.push(self.parse_type()?);
                    match self.peek().kind {
                        TokenKind::Comma => {
                            self.advance();
                        }
                        TokenKind::RParen => break,
                        TokenKind::Eof => {
                            return Err(ParseError::UnexpectedEof {
                                context: "tuple type",
                            });
                        }
                        _ => {
                            let t = self.peek().clone();
                            return Err(ParseError::Expected {
                                expected: "`,` or `)` in tuple type",
                                found: t.kind,
                                at: t.span.start,
                            });
                        }
                    }
                }
                let close = self.expect(TokenKind::RParen, "`)` to close tuple type")?;
                Ok(TypeExpr {
                    kind: TypeKind::Tuple(TupleType { elements }),
                    span: Span::new(start, close.end),
                })
            }
            TokenKind::Eof => Err(ParseError::UnexpectedEof {
                context: "tuple or parenthesised type",
            }),
            _ => {
                let t = self.peek().clone();
                Err(ParseError::Expected {
                    expected: "`,` (tuple) or `)` (paren type) after type in parens",
                    found: t.kind,
                    at: t.span.start,
                })
            }
        }
    }

    /// Parse `@fn(T1, T2) -> T3 $ [TraitList]` — function-pointer type per §2.7.
    /// Both the return type and the trait list are optional.
    fn parse_fn_type(&mut self, start: usize) -> Result<TypeExpr, ParseError> {
        self.advance(); // `@fn`
        self.expect(TokenKind::LParen, "`(` after `@fn` in type position")?;
        let mut params = Vec::new();
        if !matches!(self.peek().kind, TokenKind::RParen) {
            loop {
                params.push(self.parse_type()?);
                match self.peek().kind {
                    TokenKind::Comma => {
                        self.advance();
                    }
                    TokenKind::RParen => break,
                    TokenKind::Eof => {
                        return Err(ParseError::UnexpectedEof {
                            context: "@fn parameter list",
                        });
                    }
                    _ => {
                        let t = self.peek().clone();
                        return Err(ParseError::Expected {
                            expected: "`,` or `)` in @fn parameter types",
                            found: t.kind,
                            at: t.span.start,
                        });
                    }
                }
            }
        }
        let close_paren =
            self.expect(TokenKind::RParen, "`)` to close @fn parameter types")?;
        let mut end = close_paren.end;

        let return_type = if matches!(self.peek().kind, TokenKind::Arrow) {
            self.advance(); // `->`
            let rt = self.parse_type()?;
            end = rt.span.end;
            Some(Box::new(rt))
        } else {
            None
        };

        let trait_list = if matches!(self.peek().kind, TokenKind::Dollar) {
            let (list, list_end) = self.parse_trait_list()?;
            end = list_end;
            list
        } else {
            Vec::new()
        };

        Ok(TypeExpr {
            kind: TypeKind::Fn(FnType {
                params,
                return_type,
                trait_list,
            }),
            span: Span::new(start, end),
        })
    }

    /// Parse a path-or-primitive type. The leading token is an `Ident` or
    /// `KwSelfType`. Single-segment idents matching a primitive name resolve
    /// to `TypeKind::Primitive`; everything else becomes `TypeKind::Path`.
    fn parse_path_or_primitive(&mut self, start: usize) -> Result<TypeExpr, ParseError> {
        let first_tok = self.peek().clone();
        let (first, first_span) = match first_tok.kind {
            TokenKind::Ident(name) => {
                self.advance();
                (name, first_tok.span)
            }
            TokenKind::KwSelfType => {
                self.advance();
                ("Self".to_owned(), first_tok.span)
            }
            _ => unreachable!("dispatch ensured Ident or KwSelfType"),
        };

        // If single ident matches a primitive AND the next token doesn't
        // continue the path or a generic argument list, classify as Primitive.
        if let Some(prim) = primitive_from_str(&first) {
            if !matches!(self.peek().kind, TokenKind::ColonColon | TokenKind::Lt) {
                return Ok(TypeExpr {
                    kind: TypeKind::Primitive(prim),
                    span: Span::new(start, first_span.end),
                });
            }
            // Otherwise fall through and treat as path — `u8::CONST` would
            // be unusual but is syntactically a path.
        }

        let mut segments = vec![first];
        let mut end = first_span.end;
        while matches!(self.peek().kind, TokenKind::ColonColon) {
            self.advance(); // `::`
            let (seg, seg_span) = self.expect_ident("path segment after `::`")?;
            segments.push(seg);
            end = seg_span.end;
        }

        let generic_args = if matches!(self.peek().kind, TokenKind::Lt) {
            let (args, args_end) = self.parse_generic_args()?;
            end = args_end;
            args
        } else {
            Vec::new()
        };

        Ok(TypeExpr {
            kind: TypeKind::Path(PathType {
                segments,
                generic_args,
            }),
            span: Span::new(start, end),
        })
    }

    /// Parse `<T1, T2, …>`. Returns the args and the end byte of the `>`.
    /// At type position there is no generic-vs-comparison ambiguity (we are
    /// not parsing expressions), so a leading `<` always begins generic args.
    fn parse_generic_args(&mut self) -> Result<(Vec<TypeExpr>, usize), ParseError> {
        self.expect(TokenKind::Lt, "`<` to open generic args")?;
        let mut args = Vec::new();
        if !matches!(self.peek().kind, TokenKind::Gt) {
            loop {
                args.push(self.parse_type()?);
                match self.peek().kind {
                    TokenKind::Comma => {
                        self.advance();
                    }
                    TokenKind::Gt => break,
                    TokenKind::Eof => {
                        return Err(ParseError::UnexpectedEof {
                            context: "generic argument list",
                        });
                    }
                    _ => {
                        let t = self.peek().clone();
                        return Err(ParseError::Expected {
                            expected: "`,` or `>` in generic argument list",
                            found: t.kind,
                            at: t.span.start,
                        });
                    }
                }
            }
        }
        let close = self.expect(TokenKind::Gt, "`>` to close generic argument list")?;
        Ok((args, close.end))
    }

    /// Parse `$ [Trait, Trait<T>, …]` — trait list per Decision #2 / §2.7.
    /// Returns the parsed list and the end byte of the closing `]`.
    fn parse_trait_list(&mut self) -> Result<(Vec<TraitRef>, usize), ParseError> {
        self.expect(TokenKind::Dollar, "`$` to start trait list")?;
        self.expect(TokenKind::LBracket, "`[` to open trait list")?;
        let mut traits = Vec::new();
        if !matches!(self.peek().kind, TokenKind::RBracket) {
            loop {
                traits.push(self.parse_trait_ref()?);
                match self.peek().kind {
                    TokenKind::Comma => {
                        self.advance();
                    }
                    TokenKind::RBracket => break,
                    TokenKind::Eof => {
                        return Err(ParseError::UnexpectedEof {
                            context: "trait list",
                        });
                    }
                    _ => {
                        let t = self.peek().clone();
                        return Err(ParseError::Expected {
                            expected: "`,` or `]` in trait list",
                            found: t.kind,
                            at: t.span.start,
                        });
                    }
                }
            }
        }
        let close = self.expect(TokenKind::RBracket, "`]` to close trait list")?;
        Ok((traits, close.end))
    }

    /// Parse a trait reference: `Name` or `Name<T1, T2>`.
    fn parse_trait_ref(&mut self) -> Result<TraitRef, ParseError> {
        let (name, _) = self.expect_ident("trait name in trait list")?;
        let generic_args = if matches!(self.peek().kind, TokenKind::Lt) {
            let (args, _) = self.parse_generic_args()?;
            args
        } else {
            Vec::new()
        };
        Ok(TraitRef { name, generic_args })
    }
}

/// Map an identifier string to a primitive type, if it is one.
///
/// This is the lexer-agnostic version of the §1.3 keyword check — primitive
/// type names are *not* lexer keywords (no `KwU8` token); they're regular
/// identifiers that the parser recognises in type position.
fn primitive_from_str(name: &str) -> Option<PrimitiveType> {
    Some(match name {
        "u8" => PrimitiveType::U8,
        "u16" => PrimitiveType::U16,
        "u32" => PrimitiveType::U32,
        "u64" => PrimitiveType::U64,
        "usize" => PrimitiveType::Usize,
        "i8" => PrimitiveType::I8,
        "i16" => PrimitiveType::I16,
        "i32" => PrimitiveType::I32,
        "i64" => PrimitiveType::I64,
        "isize" => PrimitiveType::Isize,
        "f32" => PrimitiveType::F32,
        "f64" => PrimitiveType::F64,
        "bool" => PrimitiveType::Bool,
        "char" => PrimitiveType::Char,
        _ => return None,
    })
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

    // ─── Slice 2: extended top-level items ────────────────────────────────

    use clifford_ast::{
        EffectDecl, ImplDecl, InterfaceDecl, InterruptDecl, PriorityLevel, SequentialAttr,
        TestDecl,
    };

    #[test]
    fn effect_with_empty_mutates() {
        let p = parse_str("#effect noop() #mutates: [] { }").expect("parse pure-ish #effect");
        assert_eq!(p.items.len(), 1);
        match &p.items[0] {
            Item::Effect(EffectDecl {
                name,
                mutates,
                cannot_mutate,
                ..
            }) => {
                assert_eq!(name, "noop");
                assert!(mutates.is_empty());
                assert!(cannot_mutate.is_empty());
            }
            other => panic!("expected Effect, got {:?}", other),
        }
        assert_eq!(p.items[0].layer(), Layer::Imperative);
    }

    #[test]
    fn effect_with_mutates_list() {
        let p = parse_str("#effect tick() #mutates: [Counter, Logger] { }")
            .expect("parse #effect tick");
        match &p.items[0] {
            Item::Effect(EffectDecl { name, mutates, .. }) => {
                assert_eq!(name, "tick");
                assert_eq!(mutates, &vec!["Counter".to_string(), "Logger".to_string()]);
            }
            other => panic!("expected Effect, got {:?}", other),
        }
    }

    #[test]
    fn effect_with_mutates_and_cannot_mutate() {
        let p = parse_str(
            "#effect tick() \
             #mutates: [Counter, Logger] \
             #cannot_mutate: [Boot] \
             { }",
        )
        .expect("parse #effect with both clauses");
        match &p.items[0] {
            Item::Effect(EffectDecl {
                mutates,
                cannot_mutate,
                ..
            }) => {
                assert_eq!(mutates, &vec!["Counter".to_string(), "Logger".to_string()]);
                assert_eq!(cannot_mutate, &vec!["Boot".to_string()]);
            }
            other => panic!("expected Effect, got {:?}", other),
        }
    }

    #[test]
    fn effect_requires_mutates() {
        // Per §2.5 notes, `#effect` requires `#mutates` (may be empty list).
        let err = parse_str("#effect noop() { }").expect_err("missing #mutates should error");
        assert!(matches!(
            err,
            ParseError::Expected {
                expected: "`#mutates: [...]` clause is required for `#effect`",
                ..
            }
        ));
    }

    #[test]
    fn interrupt_with_priority_high() {
        let p = parse_str("#interrupt USART1_IRQHandler() #mutates: [UartRx] #priority: HIGH { }")
            .expect("parse #interrupt");
        match &p.items[0] {
            Item::Interrupt(InterruptDecl {
                name,
                mutates,
                priority,
                ..
            }) => {
                assert_eq!(name, "USART1_IRQHandler");
                assert_eq!(mutates, &vec!["UartRx".to_string()]);
                assert_eq!(*priority, PriorityLevel::High);
            }
            other => panic!("expected Interrupt, got {:?}", other),
        }
    }

    #[test]
    fn interrupt_with_numeric_priority() {
        let p = parse_str("#interrupt SysTick_Handler() #mutates: [Sched] #priority: 7 { }")
            .expect("parse #interrupt with numeric priority");
        match &p.items[0] {
            Item::Interrupt(InterruptDecl { priority, .. }) => {
                assert_eq!(*priority, PriorityLevel::Numeric("7".into()));
            }
            other => panic!("expected Interrupt, got {:?}", other),
        }
    }

    #[test]
    fn interrupt_requires_priority() {
        let err =
            parse_str("#interrupt UART_RX() #mutates: [UartRx] { }").expect_err("missing #priority");
        assert!(matches!(
            err,
            ParseError::Expected {
                expected: "`#priority: …` clause is required for `#interrupt`",
                ..
            }
        ));
    }

    #[test]
    fn interrupt_priority_rejects_random_ident() {
        let err = parse_str(
            "#interrupt X() #mutates: [A] #priority: SUPER_DUPER_HIGH { }",
        )
        .expect_err("random ident is not a valid priority level");
        assert!(matches!(
            err,
            ParseError::Expected {
                expected: "`LOW`, `MEDIUM`, `HIGH`, or integer literal as `#priority` value",
                ..
            }
        ));
    }

    #[test]
    fn interface_decl() {
        let p = parse_str("#interface Serial { }").expect("parse #interface");
        match &p.items[0] {
            Item::Interface(InterfaceDecl { name, .. }) => assert_eq!(name, "Serial"),
            other => panic!("expected Interface, got {:?}", other),
        }
    }

    #[test]
    fn impl_decl() {
        let p = parse_str("#impl Serial for Usart1 { }").expect("parse #impl");
        match &p.items[0] {
            Item::Impl(ImplDecl {
                interface_name,
                automaton_name,
                ..
            }) => {
                assert_eq!(interface_name, "Serial");
                assert_eq!(automaton_name, "Usart1");
            }
            other => panic!("expected Impl, got {:?}", other),
        }
    }

    #[test]
    fn impl_requires_for() {
        let err = parse_str("#impl Serial Usart1 { }").expect_err("missing `for`");
        assert!(matches!(
            err,
            ParseError::Expected {
                expected: "`for` between interface and automaton",
                ..
            }
        ));
    }

    #[test]
    fn test_decl_with_description() {
        let p = parse_str(r#"#test "scheduler picks lowest vruntime" { }"#)
            .expect("parse #test");
        match &p.items[0] {
            Item::Test(TestDecl { description, .. }) => {
                assert_eq!(description, "scheduler picks lowest vruntime");
            }
            other => panic!("expected Test, got {:?}", other),
        }
    }

    #[test]
    fn test_decl_requires_string_description() {
        let err = parse_str("#test foo { }").expect_err("non-string after #test");
        assert!(matches!(
            err,
            ParseError::Expected {
                expected: "string-literal description after `#test`",
                ..
            }
        ));
    }

    #[test]
    fn sequential_attr() {
        let p = parse_str("@sequential(Boot, Counter);").expect("parse @sequential");
        assert_eq!(p.items.len(), 1);
        match &p.items[0] {
            Item::Sequential(SequentialAttr { a, b, .. }) => {
                assert_eq!(a, "Boot");
                assert_eq!(b, "Counter");
            }
            other => panic!("expected Sequential, got {:?}", other),
        }
        assert_eq!(p.items[0].layer(), Layer::Functional);
    }

    #[test]
    fn sequential_attr_needs_terminating_semi() {
        let err = parse_str("@sequential(A, B)").expect_err("missing semicolon");
        // Either UnexpectedEof or Expected — both acceptable here.
        assert!(matches!(
            err,
            ParseError::UnexpectedEof { .. } | ParseError::Expected { .. }
        ));
    }

    #[test]
    fn realistic_blinky_skeleton() {
        // The shape of `crates/examples/blinky` once it lands. Tests that
        // all six new top-level forms coexist in a single program.
        let src = "\
            #automaton Counter { }\n\
            #automaton Boot { }\n\
            #effect tick() #mutates: [Counter] { }\n\
            #interrupt USART1_IRQHandler() #mutates: [Counter] #priority: HIGH { }\n\
            #interface Serial { }\n\
            #impl Serial for Boot { }\n\
            #test \"sanity\" { }\n\
            @sequential(Boot, Counter);\n\
            @fn main() { }\n\
        ";
        let p = parse_str(src).expect("parse blinky-shape");
        assert_eq!(p.items.len(), 9);

        // Spot-check the layer stamps.
        let layers: Vec<Layer> = p.items.iter().map(|i| i.layer()).collect();
        assert_eq!(
            layers,
            vec![
                Layer::Imperative,  // #automaton Counter
                Layer::Imperative,  // #automaton Boot
                Layer::Imperative,  // #effect
                Layer::Imperative,  // #interrupt
                Layer::Imperative,  // #interface
                Layer::Imperative,  // #impl
                Layer::Imperative,  // #test
                Layer::Functional,  // @sequential
                Layer::Functional,  // @fn main
            ]
        );
    }

    #[test]
    fn empty_ident_list_is_valid_in_brackets() {
        // `#mutates: []` is the canonical "pure effect" form.
        let p = parse_str("#effect noop() #mutates: [] { }").unwrap();
        assert!(matches!(
            &p.items[0],
            Item::Effect(EffectDecl { mutates, .. }) if mutates.is_empty()
        ));
    }

    #[test]
    fn ident_list_with_trailing_no_comma() {
        // Single ident, no trailing comma.
        let p = parse_str("#effect e() #mutates: [Solo] { }").unwrap();
        match &p.items[0] {
            Item::Effect(EffectDecl { mutates, .. }) => {
                assert_eq!(mutates, &vec!["Solo".to_string()]);
            }
            other => panic!("expected Effect, got {:?}", other),
        }
    }

    // ─── Slice 3: type expressions (§2.7) ─────────────────────────────────

    use clifford_ast::{
        AccessType, ArraySize, ArrayType, FnType, PathType, PrimitiveType, RefType, SliceType,
        TraitRef, TupleType, TypeExpr, TypeKind,
    };

    /// Test helper: parse one type expression from source. The parser must
    /// consume every token to EOF (so trailing junk is an error).
    fn parse_type_str(src: &str) -> Result<TypeExpr, ParseError> {
        let tokens = tokenize(src).expect("tokenize");
        let mut p = Parser::new(&tokens);
        let ty = p.parse_type()?;
        if !p.at_eof() {
            let t = p.peek().clone();
            return Err(ParseError::Expected {
                expected: "EOF after type",
                found: t.kind,
                at: t.span.start,
            });
        }
        Ok(ty)
    }

    #[test]
    fn primitive_types() {
        for (src, prim) in &[
            ("u8", PrimitiveType::U8),
            ("u16", PrimitiveType::U16),
            ("u32", PrimitiveType::U32),
            ("u64", PrimitiveType::U64),
            ("usize", PrimitiveType::Usize),
            ("i8", PrimitiveType::I8),
            ("i16", PrimitiveType::I16),
            ("i32", PrimitiveType::I32),
            ("i64", PrimitiveType::I64),
            ("isize", PrimitiveType::Isize),
            ("f32", PrimitiveType::F32),
            ("f64", PrimitiveType::F64),
            ("bool", PrimitiveType::Bool),
            ("char", PrimitiveType::Char),
        ] {
            let ty = parse_type_str(src).unwrap_or_else(|e| panic!("{src}: {e}"));
            assert_eq!(ty.kind, TypeKind::Primitive(*prim), "src: {src}");
        }
    }

    #[test]
    fn unit_type() {
        let ty = parse_type_str("()").expect("parse unit");
        assert_eq!(ty.kind, TypeKind::Unit);
    }

    #[test]
    fn parenthesised_type_unwraps() {
        // `(u32)` is just `u32`; tuples need >= 2 elements per §2.7.
        let ty = parse_type_str("(u32)").expect("parse parenthesised");
        assert_eq!(ty.kind, TypeKind::Primitive(PrimitiveType::U32));
    }

    #[test]
    fn tuple_type_two() {
        let ty = parse_type_str("(u32, bool)").expect("parse 2-tuple");
        match ty.kind {
            TypeKind::Tuple(TupleType { elements }) => {
                assert_eq!(elements.len(), 2);
                assert_eq!(elements[0].kind, TypeKind::Primitive(PrimitiveType::U32));
                assert_eq!(elements[1].kind, TypeKind::Primitive(PrimitiveType::Bool));
            }
            other => panic!("expected Tuple, got {:?}", other),
        }
    }

    #[test]
    fn tuple_type_three() {
        let ty = parse_type_str("(u8, u16, u32)").expect("parse 3-tuple");
        match ty.kind {
            TypeKind::Tuple(TupleType { elements }) => assert_eq!(elements.len(), 3),
            other => panic!("expected Tuple, got {:?}", other),
        }
    }

    #[test]
    fn ref_type_immutable() {
        let ty = parse_type_str("&u32").expect("parse &u32");
        match ty.kind {
            TypeKind::Ref(RefType { mutable, inner }) => {
                assert!(!mutable);
                assert_eq!(inner.kind, TypeKind::Primitive(PrimitiveType::U32));
            }
            other => panic!("expected Ref, got {:?}", other),
        }
    }

    #[test]
    fn ref_type_mutable() {
        let ty = parse_type_str("&mut u32").expect("parse &mut u32");
        match ty.kind {
            TypeKind::Ref(RefType { mutable, inner }) => {
                assert!(mutable);
                assert_eq!(inner.kind, TypeKind::Primitive(PrimitiveType::U32));
            }
            other => panic!("expected Ref, got {:?}", other),
        }
    }

    #[test]
    fn access_type_default() {
        let ty = parse_type_str("access<u8>").expect("parse access<u8>");
        match ty.kind {
            TypeKind::Access(AccessType { is_const, inner }) => {
                assert!(!is_const);
                assert_eq!(inner.kind, TypeKind::Primitive(PrimitiveType::U8));
            }
            other => panic!("expected Access, got {:?}", other),
        }
    }

    #[test]
    fn access_const_type() {
        let ty = parse_type_str("access const<u8>").expect("parse access const<u8>");
        match ty.kind {
            TypeKind::Access(AccessType { is_const, .. }) => assert!(is_const),
            other => panic!("expected Access, got {:?}", other),
        }
    }

    #[test]
    fn array_type() {
        let ty = parse_type_str("[u8; 64]").expect("parse [u8; 64]");
        match ty.kind {
            TypeKind::Array(ArrayType { element, size }) => {
                assert_eq!(element.kind, TypeKind::Primitive(PrimitiveType::U8));
                assert_eq!(size, ArraySize::IntLiteral("64".into()));
            }
            other => panic!("expected Array, got {:?}", other),
        }
    }

    #[test]
    fn array_type_with_underscore_size() {
        let ty = parse_type_str("[u32; 1_024]").expect("parse [u32; 1_024]");
        match ty.kind {
            TypeKind::Array(ArrayType { size, .. }) => {
                assert_eq!(size, ArraySize::IntLiteral("1_024".into()));
            }
            other => panic!("expected Array, got {:?}", other),
        }
    }

    #[test]
    fn slice_type() {
        let ty = parse_type_str("[u8]").expect("parse [u8]");
        match ty.kind {
            TypeKind::Slice(SliceType { element }) => {
                assert_eq!(element.kind, TypeKind::Primitive(PrimitiveType::U8));
            }
            other => panic!("expected Slice, got {:?}", other),
        }
    }

    #[test]
    fn path_type_single_segment() {
        let ty = parse_type_str("Counter").expect("parse Counter");
        match ty.kind {
            TypeKind::Path(PathType {
                segments,
                generic_args,
            }) => {
                assert_eq!(segments, vec!["Counter".to_string()]);
                assert!(generic_args.is_empty());
            }
            other => panic!("expected Path, got {:?}", other),
        }
    }

    #[test]
    fn path_type_multi_segment() {
        let ty = parse_type_str("clifford::core::Option<u32>").expect("parse path");
        match ty.kind {
            TypeKind::Path(PathType {
                segments,
                generic_args,
            }) => {
                assert_eq!(segments, vec!["clifford".to_string(), "core".to_string(), "Option".to_string()]);
                assert_eq!(generic_args.len(), 1);
                assert_eq!(generic_args[0].kind, TypeKind::Primitive(PrimitiveType::U32));
            }
            other => panic!("expected Path, got {:?}", other),
        }
    }

    #[test]
    fn path_type_generic_with_two_args() {
        let ty = parse_type_str("Result<u32, bool>").expect("parse Result");
        match ty.kind {
            TypeKind::Path(PathType { generic_args, .. }) => {
                assert_eq!(generic_args.len(), 2);
            }
            other => panic!("expected Path, got {:?}", other),
        }
    }

    #[test]
    fn self_type() {
        let ty = parse_type_str("Self").expect("parse Self");
        match ty.kind {
            TypeKind::Path(PathType { segments, .. }) => {
                assert_eq!(segments, vec!["Self".to_string()]);
            }
            other => panic!("expected Path containing Self, got {:?}", other),
        }
    }

    #[test]
    fn fn_type_no_params_no_return() {
        let ty = parse_type_str("@fn()").expect("parse @fn()");
        match ty.kind {
            TypeKind::Fn(FnType {
                params,
                return_type,
                trait_list,
            }) => {
                assert!(params.is_empty());
                assert!(return_type.is_none());
                assert!(trait_list.is_empty());
            }
            other => panic!("expected Fn, got {:?}", other),
        }
    }

    #[test]
    fn fn_type_with_params_and_return() {
        let ty = parse_type_str("@fn(u32, bool) -> i64").expect("parse @fn");
        match ty.kind {
            TypeKind::Fn(FnType {
                params,
                return_type,
                trait_list,
            }) => {
                assert_eq!(params.len(), 2);
                assert!(return_type.is_some());
                assert_eq!(
                    return_type.unwrap().kind,
                    TypeKind::Primitive(PrimitiveType::I64)
                );
                assert!(trait_list.is_empty());
            }
            other => panic!("expected Fn, got {:?}", other),
        }
    }

    #[test]
    fn fn_type_with_trait_list() {
        let ty = parse_type_str("@fn(u32) -> u32 $ [Pure, Readable]").expect("parse @fn $");
        match ty.kind {
            TypeKind::Fn(FnType { trait_list, .. }) => {
                let names: Vec<_> = trait_list.iter().map(|t| t.name.as_str()).collect();
                assert_eq!(names, vec!["Pure", "Readable"]);
                // Each trait has no generic args here.
                for t in &trait_list {
                    assert!(t.generic_args.is_empty());
                }
            }
            other => panic!("expected Fn, got {:?}", other),
        }
    }

    #[test]
    fn fn_type_with_generic_trait_in_list() {
        let ty = parse_type_str("@fn(u32) $ [Iterator<u32>]").expect("parse @fn with generic trait");
        match ty.kind {
            TypeKind::Fn(FnType { trait_list, .. }) => {
                assert_eq!(trait_list.len(), 1);
                let TraitRef { name, generic_args } = &trait_list[0];
                assert_eq!(name, "Iterator");
                assert_eq!(generic_args.len(), 1);
                assert_eq!(
                    generic_args[0].kind,
                    TypeKind::Primitive(PrimitiveType::U32)
                );
            }
            other => panic!("expected Fn, got {:?}", other),
        }
    }

    #[test]
    fn nested_types() {
        // `&[u32; 4]` — ref to a 4-element array of u32.
        let ty = parse_type_str("&[u32; 4]").expect("parse &[u32; 4]");
        match ty.kind {
            TypeKind::Ref(RefType { mutable, inner }) => {
                assert!(!mutable);
                match inner.kind {
                    TypeKind::Array(ArrayType { element, size }) => {
                        assert_eq!(element.kind, TypeKind::Primitive(PrimitiveType::U32));
                        assert_eq!(size, ArraySize::IntLiteral("4".into()));
                    }
                    other => panic!("expected Array inside Ref, got {:?}", other),
                }
            }
            other => panic!("expected Ref, got {:?}", other),
        }
    }

    #[test]
    fn deeply_nested_types() {
        // `access<[Result<u32, bool>; 8]>` — a register pointer to an array
        // of Results. Stress-tests recursion depth.
        let ty = parse_type_str("access<[Result<u32, bool>; 8]>")
            .expect("parse deep nested");
        match ty.kind {
            TypeKind::Access(AccessType { is_const, inner }) => {
                assert!(!is_const);
                assert!(matches!(inner.kind, TypeKind::Array(_)));
            }
            other => panic!("expected Access, got {:?}", other),
        }
    }

    #[test]
    fn tuple_of_refs() {
        // `(&u32, &mut u32)` — common shape for "in / out" style helpers.
        let ty = parse_type_str("(&u32, &mut u32)").expect("parse tuple of refs");
        match ty.kind {
            TypeKind::Tuple(TupleType { elements }) => {
                assert_eq!(elements.len(), 2);
                assert!(matches!(elements[0].kind, TypeKind::Ref(_)));
                assert!(matches!(elements[1].kind, TypeKind::Ref(RefType { mutable: true, .. })));
            }
            other => panic!("expected Tuple, got {:?}", other),
        }
    }

    #[test]
    fn type_expr_spans_track_correctly() {
        let src = "&mut [u8; 64]";
        let ty = parse_type_str(src).unwrap();
        assert_eq!(ty.span, Span::new(0, src.len()));
    }

    #[test]
    fn empty_type_input_errors() {
        assert!(matches!(
            parse_type_str(""),
            Err(ParseError::UnexpectedEof { .. })
        ));
    }

    #[test]
    fn array_with_non_int_size_errors() {
        let err = parse_type_str("[u8; foo]").expect_err("non-int size");
        assert!(matches!(
            err,
            ParseError::Expected {
                expected: "integer literal as array size",
                ..
            }
        ));
    }

    #[test]
    fn access_requires_angle_brackets() {
        let err = parse_type_str("access u8").expect_err("missing `<`");
        assert!(matches!(
            err,
            ParseError::Expected {
                expected: "`<` after `access`",
                ..
            }
        ));
    }

    // ─── Slice 3 wiring: @fn return types ─────────────────────────────────

    #[test]
    fn fn_with_return_type() {
        let p = parse_str("@fn next() -> u32 { }").expect("parse @fn -> u32");
        match &p.items[0] {
            Item::Fn(FnDecl {
                name,
                return_type,
                ..
            }) => {
                assert_eq!(name, "next");
                assert!(return_type.is_some());
                assert_eq!(
                    return_type.as_ref().unwrap().kind,
                    TypeKind::Primitive(PrimitiveType::U32)
                );
            }
            other => panic!("expected Fn, got {:?}", other),
        }
    }

    #[test]
    fn fn_without_return_type_keeps_none() {
        let p = parse_str("@fn no_return() { }").expect("parse @fn (no return)");
        match &p.items[0] {
            Item::Fn(FnDecl { return_type, .. }) => assert!(return_type.is_none()),
            other => panic!("expected Fn, got {:?}", other),
        }
    }

    #[test]
    fn fn_with_complex_return_type() {
        // `@fn build() -> Result<access<u8>, [u8; 16]>` — exercises path
        // type with generics, access type, array type, all nested through
        // the return-type position.
        let p = parse_str("@fn build() -> Result<access<u8>, [u8; 16]> { }")
            .expect("parse @fn with complex return");
        match &p.items[0] {
            Item::Fn(FnDecl { return_type, .. }) => {
                let rt = return_type.as_ref().expect("return type present");
                match &rt.kind {
                    TypeKind::Path(PathType {
                        segments,
                        generic_args,
                    }) => {
                        assert_eq!(segments, &vec!["Result".to_string()]);
                        assert_eq!(generic_args.len(), 2);
                        assert!(matches!(generic_args[0].kind, TypeKind::Access(_)));
                        assert!(matches!(generic_args[1].kind, TypeKind::Array(_)));
                    }
                    other => panic!("expected Path return, got {:?}", other),
                }
            }
            other => panic!("expected Fn, got {:?}", other),
        }
    }
}
