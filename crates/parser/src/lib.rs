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
    AccessMode, AccessType, AddressClause, ArraySize, ArrayType, AssignOp, AutomatonDecl,
    AutomatonField, BasisClause, BinaryOp, Block, EffectDecl, Expr, ExprKind, Field, FieldAssign,
    FieldKind, FnDecl, FnType, GenericParam, ImplDecl, InterfaceDecl, InterfaceMethod,
    InterruptDecl, Item, Param, PathType, PriorityLevel, PrimitiveType, Program, RefType,
    SequentialAttr, SliceType, StateName, Stmt, StmtKind, TestDecl, TraitDecl, TraitMethod,
    TraitRef, TransitionDecl, TupleType, TypeBody, TypeDecl, TypeExpr, TypeKind, UnaryOp, Variant,
    VariantData,
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

    /// A clause that may appear at most once in a declaration appeared twice.
    /// E.g. two `#address` clauses in the same `#automaton` body.
    #[error("E0210: duplicate `{clause}` clause at byte {at}")]
    DuplicateClause {
        /// The clause that was duplicated, with its leading sigil.
        clause: &'static str,
        /// Byte offset where the second occurrence began.
        at: usize,
    },

    /// `#states: [];` is semantically nonsensical — a multi-state automaton
    /// with zero states cannot exist. Use no `#states` clause to mean
    /// monoid-automaton instead per Decision #5.
    #[error("E0211: `#states: []` is empty at byte {at}; use no `#states` clause for a monoid (single-state) automaton")]
    EmptyStatesList {
        /// Byte offset where the `#states` keyword began.
        at: usize,
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

/// Parse a single expression from a token stream. Useful for REPL-style
/// invocations and for downstream consumers (the type checker, the
/// effect-extractor) that want to parse const-position expressions.
///
/// Returns the parsed [`Expr`] on success. The token stream must contain
/// the expression followed by EOF; trailing junk is `E0204`.
///
/// # Errors
///
/// Returns [`ParseError`] for malformed input or for tokens after the
/// expression.
pub fn parse_expression(tokens: &[Token]) -> Result<Expr, ParseError> {
    let mut p = Parser::new(tokens);
    let expr = p.parse_expr()?;
    if !p.at_eof() {
        let t = p.peek().clone();
        return Err(ParseError::Expected {
            expected: "EOF after expression",
            found: t.kind,
            at: t.span.start,
        });
    }
    Ok(expr)
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
            TokenKind::KwAtType => self.parse_type_decl(lead.span.start).map(Item::Type),
            TokenKind::KwAtTrait => self.parse_trait_decl(lead.span.start).map(Item::Trait),
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

    /// Parse `@fn name(params) -> T $ [TraitList] { body }`.
    ///
    /// Slice-7 form: full body parsing (statements + expressions per §2.6).
    /// Generic parameters, where-clause, and extern modifier still arrive
    /// in subsequent slices.
    fn parse_fn_decl(&mut self, start: usize) -> Result<FnDecl, ParseError> {
        self.advance(); // `@fn`
        let (name, _) = self.expect_ident("function name after `@fn`")?;
        let params = self.parse_param_list()?;
        let return_type = if matches!(self.peek().kind, TokenKind::Arrow) {
            self.advance(); // `->`
            Some(self.parse_type()?)
        } else {
            None
        };
        let trait_list = if matches!(self.peek().kind, TokenKind::Dollar) {
            let (list, _) = self.parse_trait_list()?;
            list
        } else {
            Vec::new()
        };
        let body = self.parse_block()?;
        let end = body.span.end;
        Ok(FnDecl {
            name,
            params,
            return_type,
            trait_list,
            body,
            span: Span::new(start, end),
        })
    }

    /// Parse `#automaton Name { members }`.
    ///
    /// Slice-8 form: full body parsing. Members appear in any order:
    /// `#address: HEX;` (Decision #6), `#basis: name;` (Decision #4),
    /// `#states: [Name1, …];` (Decision #5), field declarations
    /// (`name: TypeExpr (#offset: HEX)? (#access: MODE)?;`), and named
    /// `#transition` blocks (Refinement #5b).
    ///
    /// The parser does not enforce "register-block automata require `#offset`
    /// on every field" — that's `clifford-check`'s job (§5.5). Here we just
    /// preserve what the user wrote and let the later phase reject ill-formed
    /// register blocks.
    ///
    /// Duplicate `#address` / `#basis` / `#states` clauses are rejected at
    /// parse time (E0210) because they're a clear-cut grammatical mistake;
    /// reordering them later doesn't change semantics, but writing two of
    /// the same one is always wrong.
    fn parse_automaton_decl(&mut self, start: usize) -> Result<AutomatonDecl, ParseError> {
        self.advance(); // `#automaton`
        let (name, _) = self.expect_ident("automaton name after `#automaton`")?;
        self.expect(TokenKind::LBrace, "`{` to open automaton body")?;

        let mut address: Option<AddressClause> = None;
        let mut basis: Option<BasisClause> = None;
        let mut states: Option<Vec<StateName>> = None;
        let mut fields: Vec<AutomatonField> = Vec::new();
        let mut transitions: Vec<TransitionDecl> = Vec::new();

        while !matches!(self.peek().kind, TokenKind::RBrace | TokenKind::Eof) {
            match self.peek().kind {
                TokenKind::KwHashAddress => {
                    if address.is_some() {
                        return Err(ParseError::DuplicateClause {
                            clause: "#address",
                            at: self.peek().span.start,
                        });
                    }
                    address = Some(self.parse_address_clause()?);
                }
                TokenKind::KwHashBasis => {
                    if basis.is_some() {
                        return Err(ParseError::DuplicateClause {
                            clause: "#basis",
                            at: self.peek().span.start,
                        });
                    }
                    basis = Some(self.parse_basis_clause()?);
                }
                TokenKind::KwHashStates => {
                    if states.is_some() {
                        return Err(ParseError::DuplicateClause {
                            clause: "#states",
                            at: self.peek().span.start,
                        });
                    }
                    states = Some(self.parse_states_clause()?);
                }
                TokenKind::KwHashTransition => {
                    transitions.push(self.parse_transition_decl()?);
                }
                TokenKind::Ident(_) => {
                    fields.push(self.parse_automaton_field()?);
                }
                TokenKind::Eof => {
                    return Err(ParseError::UnexpectedEof {
                        context: "automaton body",
                    });
                }
                _ => {
                    let t = self.peek().clone();
                    return Err(ParseError::Expected {
                        expected:
                            "`#address`, `#basis`, `#states`, `#transition`, field declaration, \
                             or `}` in automaton body",
                        found: t.kind,
                        at: t.span.start,
                    });
                }
            }
        }

        let close = self.expect(TokenKind::RBrace, "`}` to close automaton body")?;
        Ok(AutomatonDecl {
            name,
            address,
            basis,
            states,
            fields,
            transitions,
            span: Span::new(start, close.end),
        })
    }

    /// Parse `#address: 0xHEX;`. The hex literal text is preserved verbatim.
    fn parse_address_clause(&mut self) -> Result<AddressClause, ParseError> {
        let start = self.peek().span.start;
        self.advance(); // `#address`
        self.expect(TokenKind::Colon, "`:` after `#address`")?;
        let tok = self.peek().clone();
        let value = match tok.kind {
            TokenKind::HexLiteral(s) => {
                self.advance();
                s
            }
            TokenKind::Eof => {
                return Err(ParseError::UnexpectedEof {
                    context: "`#address` value (expected hex literal)",
                });
            }
            other => {
                return Err(ParseError::Expected {
                    expected: "hex literal (e.g. `0x4000_0000`) as `#address` value",
                    found: other,
                    at: tok.span.start,
                });
            }
        };
        let close = self.expect(TokenKind::Semi, "`;` to terminate `#address` clause")?;
        Ok(AddressClause {
            value,
            span: Span::new(start, close.end),
        })
    }

    /// Parse `#basis: name;`.
    fn parse_basis_clause(&mut self) -> Result<BasisClause, ParseError> {
        let start = self.peek().span.start;
        self.advance(); // `#basis`
        self.expect(TokenKind::Colon, "`:` after `#basis`")?;
        let (name, _) = self.expect_ident("basis-vector identifier after `#basis:`")?;
        let close = self.expect(TokenKind::Semi, "`;` to terminate `#basis` clause")?;
        Ok(BasisClause {
            name,
            span: Span::new(start, close.end),
        })
    }

    /// Parse `#states: [Name1, Name2, …];`. The list must be non-empty —
    /// an empty list is semantically nonsensical (a multi-state automaton
    /// with zero states cannot exist). Use *no* `#states` clause to mean
    /// "monoid automaton" instead.
    fn parse_states_clause(&mut self) -> Result<Vec<StateName>, ParseError> {
        let clause_start = self.peek().span.start;
        self.advance(); // `#states`
        self.expect(TokenKind::Colon, "`:` after `#states`")?;
        self.expect(TokenKind::LBracket, "`[` to open `#states` list")?;
        let mut names: Vec<StateName> = Vec::new();
        if matches!(self.peek().kind, TokenKind::RBracket) {
            return Err(ParseError::EmptyStatesList {
                at: clause_start,
            });
        }
        loop {
            let (n, span) = self.expect_ident("state name in `#states` list")?;
            names.push(StateName { name: n, span });
            match self.peek().kind {
                TokenKind::Comma => {
                    self.advance();
                    if matches!(self.peek().kind, TokenKind::RBracket) {
                        break;
                    }
                }
                TokenKind::RBracket => break,
                TokenKind::Eof => {
                    return Err(ParseError::UnexpectedEof {
                        context: "`#states` list",
                    });
                }
                _ => {
                    let t = self.peek().clone();
                    return Err(ParseError::Expected {
                        expected: "`,` or `]` in `#states` list",
                        found: t.kind,
                        at: t.span.start,
                    });
                }
            }
        }
        self.expect(TokenKind::RBracket, "`]` to close `#states` list")?;
        self.expect(TokenKind::Semi, "`;` to terminate `#states` clause")?;
        Ok(names)
    }

    /// Parse one automaton field:
    /// `name: TypeExpr (#hidden | #offset: HEX | #access: MODE)*;`.
    ///
    /// All three modifiers (`#hidden` per Decision #25, `#offset` and
    /// `#access` per Decision #6) may appear in any order; each at most
    /// once. `#hidden` is a flag (no value); the other two take values.
    fn parse_automaton_field(&mut self) -> Result<AutomatonField, ParseError> {
        let start = self.peek().span.start;
        let (name, _) = self.expect_ident("field name in automaton body")?;
        self.expect(TokenKind::Colon, "`:` between field name and type")?;
        let ty = self.parse_type()?;

        let mut offset: Option<String> = None;
        let mut access: Option<AccessMode> = None;
        let mut hidden = false;
        loop {
            match self.peek().kind {
                TokenKind::KwHashOffset => {
                    if offset.is_some() {
                        return Err(ParseError::DuplicateClause {
                            clause: "#offset",
                            at: self.peek().span.start,
                        });
                    }
                    self.advance();
                    self.expect(TokenKind::Colon, "`:` after `#offset`")?;
                    let tok = self.peek().clone();
                    match tok.kind {
                        TokenKind::HexLiteral(s) => {
                            self.advance();
                            offset = Some(s);
                        }
                        TokenKind::Eof => {
                            return Err(ParseError::UnexpectedEof {
                                context: "`#offset` value (expected hex literal)",
                            });
                        }
                        other => {
                            return Err(ParseError::Expected {
                                expected: "hex literal (e.g. `0x04`) as `#offset` value",
                                found: other,
                                at: tok.span.start,
                            });
                        }
                    }
                }
                TokenKind::KwHashAccess => {
                    if access.is_some() {
                        return Err(ParseError::DuplicateClause {
                            clause: "#access",
                            at: self.peek().span.start,
                        });
                    }
                    self.advance();
                    self.expect(TokenKind::Colon, "`:` after `#access`")?;
                    access = Some(self.parse_access_mode()?);
                }
                TokenKind::KwHashHidden => {
                    if hidden {
                        return Err(ParseError::DuplicateClause {
                            clause: "#hidden",
                            at: self.peek().span.start,
                        });
                    }
                    self.advance();
                    hidden = true;
                }
                _ => break,
            }
        }

        let close = self.expect(TokenKind::Semi, "`;` to terminate field declaration")?;
        // Per Decision #21 / spec §7.0: every v0.1–v0.6 field is Private.
        // The `#shared` field qualifier is reserved at the lexer for v0.7+.
        Ok(AutomatonField {
            name,
            ty,
            offset,
            access,
            kind: FieldKind::Private,
            hidden,
            span: Span::new(start, close.end),
        })
    }

    /// Parse `read | write | read_write` as an `#access` mode value.
    /// These are contextual keywords (lexed as `Ident`).
    fn parse_access_mode(&mut self) -> Result<AccessMode, ParseError> {
        let tok = self.peek().clone();
        match tok.kind {
            TokenKind::Ident(ref name) => {
                let mode = match name.as_str() {
                    "read" => AccessMode::Read,
                    "write" => AccessMode::Write,
                    "read_write" => AccessMode::ReadWrite,
                    _ => {
                        return Err(ParseError::Expected {
                            expected: "`read`, `write`, or `read_write` as `#access` value",
                            found: tok.kind,
                            at: tok.span.start,
                        });
                    }
                };
                self.advance();
                Ok(mode)
            }
            TokenKind::Eof => Err(ParseError::UnexpectedEof {
                context: "`#access` value",
            }),
            other => Err(ParseError::Expected {
                expected: "`read`, `write`, or `read_write` as `#access` value",
                found: other,
                at: tok.span.start,
            }),
        }
    }

    /// Parse `#transition name (-> Dest)? { stmts }`.
    ///
    /// Per Decision #5, named transitions are the only place where state
    /// changes happen. The destination state is optional: monoid automata
    /// (no `#states` clause) and same-state transitions both elide it.
    /// `clifford-check` validates that named destinations exist in the
    /// enclosing automaton's `#states` list (§5.5).
    fn parse_transition_decl(&mut self) -> Result<TransitionDecl, ParseError> {
        let start = self.peek().span.start;
        self.advance(); // `#transition`
        let (name, _) = self.expect_ident("transition name after `#transition`")?;
        let destination = if matches!(self.peek().kind, TokenKind::Arrow) {
            self.advance();
            let (dest, _) = self.expect_ident("destination state after `->`")?;
            Some(dest)
        } else {
            None
        };
        // Decision #22: optional `$ [TraitList]` between the destination
        // (if any) and the body block. Mirrors the @fn / fn-pointer
        // placement convention.
        let trait_list = if matches!(self.peek().kind, TokenKind::Dollar) {
            let (list, _) = self.parse_trait_list()?;
            list
        } else {
            Vec::new()
        };
        let body = self.parse_block()?;
        let end = body.span.end;
        Ok(TransitionDecl {
            name,
            destination,
            trait_list,
            body,
            span: Span::new(start, end),
        })
    }

    /// Parse `#effect name(params) -> T #mutates: [A, B] { }`.
    ///
    /// Slice-4 form: parameters and optional return type wired in. Body
    /// content (statements) lands in slice 6. The `#mutates` clause is
    /// required (per §2.5 notes for `#effect`); it may be empty
    /// (`#mutates: []`) for pure effects. `#cannot_mutate` is optional.
    /// Other effect_meta clauses (`#invariant`, `#atomic`) arrive in
    /// subsequent slices.
    fn parse_effect_decl(&mut self, start: usize) -> Result<EffectDecl, ParseError> {
        self.advance(); // `#effect`
        let (name, _) = self.expect_ident("effect name after `#effect`")?;
        let params = self.parse_param_list()?;
        let return_type = if matches!(self.peek().kind, TokenKind::Arrow) {
            self.advance();
            Some(self.parse_type()?)
        } else {
            None
        };

        let (mutates, cannot_mutate) = self.parse_effect_meta_for_effect()?;

        // Decision #22: optional `$ [TraitList]` after the `#mutates` /
        // `#cannot_mutate` metadata, before the body block.
        let trait_list = if matches!(self.peek().kind, TokenKind::Dollar) {
            let (list, _) = self.parse_trait_list()?;
            list
        } else {
            Vec::new()
        };

        let body = self.parse_block()?;
        let end = body.span.end;
        Ok(EffectDecl {
            name,
            params,
            return_type,
            mutates,
            cannot_mutate,
            trait_list,
            body,
            span: Span::new(start, end),
        })
    }

    /// Parse `#interrupt NAME(params) -> T #mutates: [A] #priority: HIGH { }`.
    ///
    /// Per §2.5 notes, `#interrupt` requires both `#mutates` and `#priority`.
    /// The name becomes the linker symbol per Decision #10.
    fn parse_interrupt_decl(&mut self, start: usize) -> Result<InterruptDecl, ParseError> {
        self.advance(); // `#interrupt`
        let (name, _) = self.expect_ident("interrupt vector name after `#interrupt`")?;
        let params = self.parse_param_list()?;
        let return_type = if matches!(self.peek().kind, TokenKind::Arrow) {
            self.advance();
            Some(self.parse_type()?)
        } else {
            None
        };

        let (mutates, priority) = self.parse_effect_meta_for_interrupt(start)?;

        // Decision #22: optional `$ [TraitList]` after `#mutates` / `#priority`
        // metadata, before the body block.
        let trait_list = if matches!(self.peek().kind, TokenKind::Dollar) {
            let (list, _) = self.parse_trait_list()?;
            list
        } else {
            Vec::new()
        };

        let body = self.parse_block()?;
        let end = body.span.end;
        Ok(InterruptDecl {
            name,
            params,
            return_type,
            mutates,
            priority,
            trait_list,
            body,
            span: Span::new(start, end),
        })
    }

    // ─── Parameter list parsing ───────────────────────────────────────────

    /// Parse `( param (, param)* )` or `()`. Each `param` is `mut? ident : type`.
    fn parse_param_list(&mut self) -> Result<Vec<Param>, ParseError> {
        self.expect(TokenKind::LParen, "`(` to open parameter list")?;
        let mut params = Vec::new();
        if matches!(self.peek().kind, TokenKind::RParen) {
            self.advance(); // empty `()`
            return Ok(params);
        }
        loop {
            params.push(self.parse_param()?);
            match self.peek().kind {
                TokenKind::Comma => {
                    self.advance();
                    // Trailing comma allowed: `(a: T,)`.
                    if matches!(self.peek().kind, TokenKind::RParen) {
                        self.advance();
                        return Ok(params);
                    }
                }
                TokenKind::RParen => {
                    self.advance();
                    return Ok(params);
                }
                TokenKind::Eof => {
                    return Err(ParseError::UnexpectedEof {
                        context: "parameter list",
                    });
                }
                _ => {
                    let t = self.peek().clone();
                    return Err(ParseError::Expected {
                        expected: "`,` or `)` in parameter list",
                        found: t.kind,
                        at: t.span.start,
                    });
                }
            }
        }
    }

    /// Parse a single `mut? name : TypeExpr` parameter.
    ///
    /// Special case: a bare `self` (without `: Type`) is accepted as a
    /// receiver-style parameter and gets a synthesised `Self` type. This
    /// matches the §4.5 example `@fn init(self) -> Self;`. Users can also
    /// write the explicit form `self: Self`, `self: &Self`, `self: &mut Self`
    /// when they need precise control.
    fn parse_param(&mut self) -> Result<Param, ParseError> {
        let start = self.peek().span.start;
        let mutable = if matches!(self.peek().kind, TokenKind::KwMut) {
            self.advance();
            true
        } else {
            false
        };

        // Accept `self` (KwSelf) or any `Ident` as parameter name.
        let lead = self.peek().clone();
        let (name, name_span) = match lead.kind {
            TokenKind::Ident(s) => {
                self.advance();
                (s, lead.span)
            }
            TokenKind::KwSelf => {
                self.advance();
                ("self".to_owned(), lead.span)
            }
            TokenKind::Eof => {
                return Err(ParseError::UnexpectedEof {
                    context: "parameter name",
                });
            }
            other => {
                return Err(ParseError::Expected {
                    expected: "parameter name",
                    found: other,
                    at: lead.span.start,
                });
            }
        };

        // Type clause: required in general; optional for `self` (defaults
        // to synthesised `Self` per the §4.5 receiver-shorthand convention).
        let (ty, end) = if name == "self" && !matches!(self.peek().kind, TokenKind::Colon) {
            let self_ty = TypeExpr {
                kind: TypeKind::Path(PathType {
                    segments: vec!["Self".to_owned()],
                    generic_args: Vec::new(),
                }),
                span: name_span,
            };
            (self_ty, name_span.end)
        } else {
            self.expect(TokenKind::Colon, "`:` between parameter name and type")?;
            let ty = self.parse_type()?;
            let end = ty.span.end;
            (ty, end)
        };

        Ok(Param {
            mutable,
            name,
            ty,
            span: Span::new(start, end),
        })
    }

    /// Parse `#interface Name<T> { effect sig; effect sig; }` (Decision #16).
    ///
    /// Body contains zero or more effect signatures. Each signature is
    /// `effect name(params) -> ret;`. The implicit `#mutates: [self]` per
    /// Decision #16 Rule 1 is restored at name-resolution time, not stored
    /// in the AST.
    fn parse_interface_decl(&mut self, start: usize) -> Result<InterfaceDecl, ParseError> {
        self.advance(); // `#interface`
        let (name, _) = self.expect_ident("interface name after `#interface`")?;
        let generic_params = if matches!(self.peek().kind, TokenKind::Lt) {
            self.parse_generic_params()?
        } else {
            Vec::new()
        };
        self.expect(TokenKind::LBrace, "`{` to open interface body")?;
        let mut methods = Vec::new();
        while !matches!(self.peek().kind, TokenKind::RBrace | TokenKind::Eof) {
            methods.push(self.parse_interface_method()?);
        }
        let close = self.expect(TokenKind::RBrace, "`}` to close interface body")?;
        Ok(InterfaceDecl {
            name,
            generic_params,
            methods,
            span: Span::new(start, close.end),
        })
    }

    /// Parse one `effect name(params) -> ret;` entry inside `#interface`.
    ///
    /// `effect` here is a contextual keyword inside an interface body,
    /// not a sigil-prefixed token; the lexer produces an `Ident("effect")`.
    /// The parser recognises it specifically in this context.
    fn parse_interface_method(&mut self) -> Result<InterfaceMethod, ParseError> {
        let start = self.peek().span.start;
        // `effect` keyword (contextual; lexed as Ident).
        let lead = self.peek().clone();
        match lead.kind {
            TokenKind::Ident(ref s) if s == "effect" => {
                self.advance();
            }
            TokenKind::Eof => {
                return Err(ParseError::UnexpectedEof {
                    context: "interface method (expected `effect`)",
                });
            }
            other => {
                return Err(ParseError::Expected {
                    expected: "`effect` keyword to start an interface method signature",
                    found: other,
                    at: lead.span.start,
                });
            }
        }
        let (name, _) = self.expect_ident("method name after `effect`")?;
        let params = self.parse_param_list()?;
        let return_type = if matches!(self.peek().kind, TokenKind::Arrow) {
            self.advance();
            Some(self.parse_type()?)
        } else {
            None
        };
        let close = self.expect(TokenKind::Semi, "`;` to terminate interface method signature")?;
        Ok(InterfaceMethod {
            name,
            params,
            return_type,
            span: Span::new(start, close.end),
        })
    }

    /// Parse `@trait Name<T> { @fn method_sig; @fn method_sig; }` (§4.5).
    fn parse_trait_decl(&mut self, start: usize) -> Result<TraitDecl, ParseError> {
        self.advance(); // `@trait`
        let (name, _) = self.expect_ident("trait name after `@trait`")?;
        let generic_params = if matches!(self.peek().kind, TokenKind::Lt) {
            self.parse_generic_params()?
        } else {
            Vec::new()
        };
        self.expect(TokenKind::LBrace, "`{` to open trait body")?;
        let mut methods = Vec::new();
        while !matches!(self.peek().kind, TokenKind::RBrace | TokenKind::Eof) {
            methods.push(self.parse_trait_method()?);
        }
        let close = self.expect(TokenKind::RBrace, "`}` to close trait body")?;
        Ok(TraitDecl {
            name,
            generic_params,
            methods,
            span: Span::new(start, close.end),
        })
    }

    /// Parse one `@fn name(params) -> ret $ [TraitList];` entry inside `@trait`.
    fn parse_trait_method(&mut self) -> Result<TraitMethod, ParseError> {
        let start = self.peek().span.start;
        self.expect(
            TokenKind::KwAtFn,
            "`@fn` to start a trait method signature",
        )?;
        let (name, _) = self.expect_ident("method name after `@fn`")?;
        let params = self.parse_param_list()?;
        let return_type = if matches!(self.peek().kind, TokenKind::Arrow) {
            self.advance();
            Some(self.parse_type()?)
        } else {
            None
        };
        let trait_list = if matches!(self.peek().kind, TokenKind::Dollar) {
            let (list, _) = self.parse_trait_list()?;
            list
        } else {
            Vec::new()
        };
        let close = self.expect(TokenKind::Semi, "`;` to terminate trait method signature")?;
        Ok(TraitMethod {
            name,
            params,
            return_type,
            trait_list,
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
        let (name, name_span) = self.expect_ident("trait name in trait list")?;
        let (generic_args, end) = if matches!(self.peek().kind, TokenKind::Lt) {
            let (args, args_end) = self.parse_generic_args()?;
            (args, args_end)
        } else {
            (Vec::new(), name_span.end)
        };
        Ok(TraitRef {
            name,
            generic_args,
            span: Span::new(name_span.start, end),
        })
    }

    // ─── @type declarations (§2.3) ────────────────────────────────────────

    /// Parse `@type Name<T, U> = TypeExpr;` (alias) or
    /// `@type Name<T, U> = | A | B(T) | C { f: T };` (ADT).
    fn parse_type_decl(&mut self, start: usize) -> Result<TypeDecl, ParseError> {
        self.advance(); // `@type`
        let (name, _) = self.expect_ident("type name after `@type`")?;
        let generic_params = if matches!(self.peek().kind, TokenKind::Lt) {
            self.parse_generic_params()?
        } else {
            Vec::new()
        };
        self.expect(TokenKind::Eq, "`=` after `@type` name")?;
        let body = self.parse_type_body()?;
        let close = self.expect(TokenKind::Semi, "`;` to terminate `@type` declaration")?;
        Ok(TypeDecl {
            name,
            generic_params,
            body,
            span: Span::new(start, close.end),
        })
    }

    /// Parse the right-hand side of an `@type`: either a single type
    /// expression (alias) or a sequence of variants separated by `|` (ADT).
    ///
    /// Disambiguation strategy: peek the leading token.
    /// - `|` is an unambiguous ADT signal (canonical form per §2.3).
    /// - Otherwise, parse as a `TypeExpr` and inspect what follows. If the
    ///   next token is `;`, it was an alias. If it's `|`, `(`, or `{`, the
    ///   `TypeExpr` we just parsed should have been a variant header — we
    ///   rewind the cursor and re-parse as ADT.
    fn parse_type_body(&mut self) -> Result<TypeBody, ParseError> {
        if matches!(self.peek().kind, TokenKind::Pipe) {
            return self.parse_adt_body();
        }
        let saved = self.pos;
        let ty = match self.parse_type() {
            Ok(t) => t,
            Err(_) => {
                // Couldn't parse as a type — rewind and try ADT (the leading
                // ident might be a unit-only variant name like `Red`).
                self.pos = saved;
                return self.parse_adt_body();
            }
        };
        match self.peek().kind {
            TokenKind::Semi => Ok(TypeBody::Alias(ty)),
            TokenKind::Pipe | TokenKind::LParen | TokenKind::LBrace => {
                // It's an ADT; the parsed TypeExpr was actually the first
                // variant's name (and possibly the start of its data shape).
                self.pos = saved;
                self.parse_adt_body()
            }
            TokenKind::Eof => Err(ParseError::UnexpectedEof {
                context: "`@type` body (expected `;` to end alias)",
            }),
            _ => {
                let t = self.peek().clone();
                Err(ParseError::Expected {
                    expected: "`;` (end alias) or `|` / `(` / `{` (start ADT variant)",
                    found: t.kind,
                    at: t.span.start,
                })
            }
        }
    }

    /// Parse `(|)? Variant (| Variant)*`.
    fn parse_adt_body(&mut self) -> Result<TypeBody, ParseError> {
        if matches!(self.peek().kind, TokenKind::Pipe) {
            self.advance(); // optional leading `|`
        }
        let mut variants = Vec::new();
        loop {
            variants.push(self.parse_variant()?);
            if matches!(self.peek().kind, TokenKind::Pipe) {
                self.advance();
            } else {
                break;
            }
        }
        Ok(TypeBody::Adt(variants))
    }

    /// Parse one ADT variant: `Name` | `Name(T1, T2, …)` | `Name { f1: T1, … }`.
    fn parse_variant(&mut self) -> Result<Variant, ParseError> {
        let start = self.peek().span.start;
        let (name, name_span) = self.expect_ident("variant name")?;
        let (data, end) = match self.peek().kind {
            TokenKind::LParen => {
                self.advance(); // `(`
                let mut elements = Vec::new();
                if !matches!(self.peek().kind, TokenKind::RParen) {
                    loop {
                        elements.push(self.parse_type()?);
                        match self.peek().kind {
                            TokenKind::Comma => {
                                self.advance();
                                if matches!(self.peek().kind, TokenKind::RParen) {
                                    break;
                                }
                            }
                            TokenKind::RParen => break,
                            TokenKind::Eof => {
                                return Err(ParseError::UnexpectedEof {
                                    context: "tuple-style variant payload",
                                });
                            }
                            _ => {
                                let t = self.peek().clone();
                                return Err(ParseError::Expected {
                                    expected: "`,` or `)` in tuple-style variant",
                                    found: t.kind,
                                    at: t.span.start,
                                });
                            }
                        }
                    }
                }
                let close = self.expect(TokenKind::RParen, "`)` to close tuple-style variant")?;
                (VariantData::Tuple(elements), close.end)
            }
            TokenKind::LBrace => {
                self.advance(); // `{`
                let mut fields = Vec::new();
                if !matches!(self.peek().kind, TokenKind::RBrace) {
                    loop {
                        fields.push(self.parse_field()?);
                        match self.peek().kind {
                            TokenKind::Comma => {
                                self.advance();
                                if matches!(self.peek().kind, TokenKind::RBrace) {
                                    break;
                                }
                            }
                            TokenKind::RBrace => break,
                            TokenKind::Eof => {
                                return Err(ParseError::UnexpectedEof {
                                    context: "struct-style variant fields",
                                });
                            }
                            _ => {
                                let t = self.peek().clone();
                                return Err(ParseError::Expected {
                                    expected: "`,` or `}` in struct-style variant",
                                    found: t.kind,
                                    at: t.span.start,
                                });
                            }
                        }
                    }
                }
                let close = self.expect(TokenKind::RBrace, "`}` to close struct-style variant")?;
                (VariantData::Struct(fields), close.end)
            }
            _ => (VariantData::None, name_span.end),
        };
        Ok(Variant {
            name,
            data,
            span: Span::new(start, end),
        })
    }

    /// Parse `name: TypeExpr` — used in struct-style variants and (later)
    /// in `#automaton` field declarations.
    fn parse_field(&mut self) -> Result<Field, ParseError> {
        let start = self.peek().span.start;
        let (name, _) = self.expect_ident("field name")?;
        self.expect(TokenKind::Colon, "`:` between field name and type")?;
        let ty = self.parse_type()?;
        let end = ty.span.end;
        Ok(Field {
            name,
            ty,
            span: Span::new(start, end),
        })
    }

    // ─── Generic parameter declarations (§2.2) ────────────────────────────

    /// Parse `<T, U: Pure + Readable, V>` — generic parameter list.
    fn parse_generic_params(&mut self) -> Result<Vec<GenericParam>, ParseError> {
        self.expect(TokenKind::Lt, "`<` to open generic parameter list")?;
        let mut params = Vec::new();
        if matches!(self.peek().kind, TokenKind::Gt) {
            self.advance();
            return Ok(params);
        }
        loop {
            params.push(self.parse_generic_param()?);
            match self.peek().kind {
                TokenKind::Comma => {
                    self.advance();
                    if matches!(self.peek().kind, TokenKind::Gt) {
                        self.advance();
                        return Ok(params);
                    }
                }
                TokenKind::Gt => {
                    self.advance();
                    return Ok(params);
                }
                TokenKind::Eof => {
                    return Err(ParseError::UnexpectedEof {
                        context: "generic parameter list",
                    });
                }
                _ => {
                    let t = self.peek().clone();
                    return Err(ParseError::Expected {
                        expected: "`,` or `>` in generic parameter list",
                        found: t.kind,
                        at: t.span.start,
                    });
                }
            }
        }
    }

    /// Parse one generic parameter: `T` or `T: Bound + Bound`.
    fn parse_generic_param(&mut self) -> Result<GenericParam, ParseError> {
        let start = self.peek().span.start;
        let (name, name_span) = self.expect_ident("generic parameter name")?;
        let (bounds, end) = if matches!(self.peek().kind, TokenKind::Colon) {
            self.advance();
            let mut bounds = vec![self.parse_trait_ref()?];
            while matches!(self.peek().kind, TokenKind::Plus) {
                self.advance();
                bounds.push(self.parse_trait_ref()?);
            }
            let last_end = bounds.last().expect("just pushed").span.end;
            (bounds, last_end)
        } else {
            (Vec::new(), name_span.end)
        };
        Ok(GenericParam {
            name,
            bounds,
            span: Span::new(start, end),
        })
    }

    // ─── Block / statement parsing (§2.6) ─────────────────────────────────

    /// Parse `{ stmt* }` — a function/effect/transition body.
    fn parse_block(&mut self) -> Result<Block, ParseError> {
        let open = self.expect(TokenKind::LBrace, "`{` to open block")?;
        let mut stmts = Vec::new();
        while !matches!(self.peek().kind, TokenKind::RBrace | TokenKind::Eof) {
            stmts.push(self.parse_stmt()?);
        }
        let close = self.expect(TokenKind::RBrace, "`}` to close block")?;
        Ok(Block {
            stmts,
            span: Span::new(open.start, close.end),
        })
    }

    /// Parse a single statement.
    fn parse_stmt(&mut self) -> Result<Stmt, ParseError> {
        let start = self.peek().span.start;
        match self.peek().kind {
            TokenKind::KwLet => self.parse_let_stmt(start),
            TokenKind::KwReturn => self.parse_return_stmt(start),
            TokenKind::KwHashMutate => self.parse_mutate_stmt(start),
            TokenKind::HashGt => self.parse_proc_call_stmt(start),
            TokenKind::KwHashUncheckedStore => {
                self.parse_unchecked_store_stmt(start, /*volatile=*/ false)
            }
            TokenKind::KwHashVolatileStore => {
                self.parse_unchecked_store_stmt(start, /*volatile=*/ true)
            }
            // Mutation sugar (Decision #15): `Auto.field <op>= expr;` — must
            // be detected via lookahead. The Auto is an `Ident`, followed by
            // `.`, an `Ident` (field), then an assignment op.
            TokenKind::Ident(_) if self.is_mutate_short_stmt() => {
                self.parse_mutate_short_stmt(start)
            }
            _ => self.parse_expr_stmt(start),
        }
    }

    /// Lookahead-only: returns true if the current position starts a
    /// `mutate_short_stmt` per Decision #15 (`Auto.field <op>= expr;`).
    fn is_mutate_short_stmt(&self) -> bool {
        // Pattern: Ident . Ident <assign_op>
        // assign_op: = += -= *= /= %= &= |= ^= <<= >>=
        if !matches!(self.peek().kind, TokenKind::Ident(_)) {
            return false;
        }
        if !matches!(self.peek_offset(1).kind, TokenKind::Dot) {
            return false;
        }
        if !matches!(self.peek_offset(2).kind, TokenKind::Ident(_)) {
            return false;
        }
        matches!(
            self.peek_offset(3).kind,
            TokenKind::Eq
                | TokenKind::PlusEq
                | TokenKind::MinusEq
                | TokenKind::StarEq
                | TokenKind::SlashEq
                | TokenKind::PercentEq
                | TokenKind::AmpEq
                | TokenKind::PipeEq
                | TokenKind::CaretEq
                | TokenKind::ShlEq
                | TokenKind::ShrEq
        )
    }

    /// Look ahead `n` tokens (0 is current). Returns Eof token if past end.
    fn peek_offset(&self, n: usize) -> &Token {
        let idx = self.pos + n;
        if idx < self.tokens.len() {
            &self.tokens[idx]
        } else {
            // Last token is always Eof per the lexer's contract.
            &self.tokens[self.tokens.len() - 1]
        }
    }

    fn parse_let_stmt(&mut self, start: usize) -> Result<Stmt, ParseError> {
        self.advance(); // `let`
        // `let mut?` - if mut, must be plain `let mut x: T = expr;` (no `:=`).
        let mutable = if matches!(self.peek().kind, TokenKind::KwMut) {
            self.advance();
            true
        } else {
            false
        };
        let (name, name_span) = self.expect_ident("binding name after `let`")?;

        // Distinguish `let x := expr;` (LetShort) from `let x = expr;` /
        // `let x: T = expr;` (Let).
        if matches!(self.peek().kind, TokenKind::ColonEq) {
            if mutable {
                let t = self.peek().clone();
                return Err(ParseError::Expected {
                    expected: "explicit `let mut x: T = expr` form (`:=` does not allow `mut`)",
                    found: t.kind,
                    at: t.span.start,
                });
            }
            self.advance(); // `:=`
            let value = self.parse_expr()?;
            let close = self.expect(TokenKind::Semi, "`;` to terminate let statement")?;
            return Ok(Stmt {
                kind: StmtKind::LetShort { name, value },
                span: Span::new(start, close.end),
            });
        }

        let ty = if matches!(self.peek().kind, TokenKind::Colon) {
            self.advance();
            Some(self.parse_type()?)
        } else {
            None
        };
        self.expect(TokenKind::Eq, "`=` after `let` binding name (or type)")?;
        let value = self.parse_expr()?;
        let close = self.expect(TokenKind::Semi, "`;` to terminate let statement")?;
        let _ = name_span;
        Ok(Stmt {
            kind: StmtKind::Let {
                mutable,
                name,
                ty,
                value,
            },
            span: Span::new(start, close.end),
        })
    }

    fn parse_return_stmt(&mut self, start: usize) -> Result<Stmt, ParseError> {
        self.advance(); // `return`
        let value = if matches!(self.peek().kind, TokenKind::Semi) {
            None
        } else {
            Some(self.parse_expr()?)
        };
        let close = self.expect(TokenKind::Semi, "`;` to terminate return statement")?;
        Ok(Stmt {
            kind: StmtKind::Return(value),
            span: Span::new(start, close.end),
        })
    }

    fn parse_mutate_stmt(&mut self, start: usize) -> Result<Stmt, ParseError> {
        self.advance(); // `#mutate`
        let (automaton, _) = self.expect_ident("automaton name after `#mutate`")?;
        self.expect(TokenKind::LBrace, "`{` to open mutate block")?;
        let mut assigns = Vec::new();
        if !matches!(self.peek().kind, TokenKind::RBrace) {
            loop {
                assigns.push(self.parse_field_assign()?);
                match self.peek().kind {
                    TokenKind::Comma => {
                        self.advance();
                        if matches!(self.peek().kind, TokenKind::RBrace) {
                            break;
                        }
                    }
                    TokenKind::RBrace => break,
                    TokenKind::Eof => {
                        return Err(ParseError::UnexpectedEof {
                            context: "mutate block",
                        });
                    }
                    _ => {
                        let t = self.peek().clone();
                        return Err(ParseError::Expected {
                            expected: "`,` or `}` in `#mutate` block",
                            found: t.kind,
                            at: t.span.start,
                        });
                    }
                }
            }
        }
        self.expect(TokenKind::RBrace, "`}` to close mutate block")?;
        let close = self.expect(TokenKind::Semi, "`;` to terminate `#mutate` statement")?;
        Ok(Stmt {
            kind: StmtKind::Mutate { automaton, assigns },
            span: Span::new(start, close.end),
        })
    }

    fn parse_field_assign(&mut self) -> Result<FieldAssign, ParseError> {
        let start = self.peek().span.start;
        let (field, _) = self.expect_ident("field name in mutate block")?;
        let index = if matches!(self.peek().kind, TokenKind::LBracket) {
            self.advance();
            let idx = self.parse_expr()?;
            self.expect(TokenKind::RBracket, "`]` to close field index")?;
            Some(idx)
        } else {
            None
        };
        self.expect(TokenKind::Eq, "`=` after field name in `#mutate` block")?;
        let value = self.parse_expr()?;
        let end = value.span.end;
        Ok(FieldAssign {
            field,
            index,
            value,
            span: Span::new(start, end),
        })
    }

    fn parse_mutate_short_stmt(&mut self, start: usize) -> Result<Stmt, ParseError> {
        let (automaton, _) = self.expect_ident("automaton name")?;
        self.expect(TokenKind::Dot, "`.` between automaton and field")?;
        let (field, _) = self.expect_ident("field name")?;
        let op_tok = self.peek().clone();
        let op = match op_tok.kind {
            TokenKind::Eq => AssignOp::Eq,
            TokenKind::PlusEq => AssignOp::PlusEq,
            TokenKind::MinusEq => AssignOp::MinusEq,
            TokenKind::StarEq => AssignOp::StarEq,
            TokenKind::SlashEq => AssignOp::SlashEq,
            TokenKind::PercentEq => AssignOp::PercentEq,
            TokenKind::AmpEq => AssignOp::AmpEq,
            TokenKind::PipeEq => AssignOp::PipeEq,
            TokenKind::CaretEq => AssignOp::CaretEq,
            TokenKind::ShlEq => AssignOp::ShlEq,
            TokenKind::ShrEq => AssignOp::ShrEq,
            other => {
                return Err(ParseError::Expected {
                    expected: "assignment operator (=, +=, -=, …)",
                    found: other,
                    at: op_tok.span.start,
                });
            }
        };
        self.advance(); // assignment op
        let value = self.parse_expr()?;
        let close = self.expect(TokenKind::Semi, "`;` to terminate mutation-sugar statement")?;
        Ok(Stmt {
            kind: StmtKind::MutateShort {
                automaton,
                field,
                op,
                value,
            },
            span: Span::new(start, close.end),
        })
    }

    fn parse_proc_call_stmt(&mut self, start: usize) -> Result<Stmt, ParseError> {
        self.advance(); // `#>`
        let (name, _) = self.expect_ident("procedure name after `#>`")?;
        self.expect(TokenKind::LParen, "`(` after procedure name")?;
        let args = self.parse_arg_list()?;
        let close = self.expect(TokenKind::Semi, "`;` to terminate `#>` call statement")?;
        Ok(Stmt {
            kind: StmtKind::ProcCall { name, args },
            span: Span::new(start, close.end),
        })
    }

    fn parse_unchecked_store_stmt(
        &mut self,
        start: usize,
        volatile: bool,
    ) -> Result<Stmt, ParseError> {
        self.advance(); // `#unchecked_store` or `#volatile_store`
        self.expect(TokenKind::Lt, "`<` after store primitive")?;
        let ty = self.parse_type()?;
        self.expect(TokenKind::Gt, "`>` to close store type argument")?;
        self.expect(TokenKind::LParen, "`(` to open store arguments")?;
        let ptr = self.parse_expr()?;
        self.expect(TokenKind::Comma, "`,` between store ptr and value")?;
        let value = self.parse_expr()?;
        self.expect(TokenKind::RParen, "`)` to close store arguments")?;
        let close = self.expect(TokenKind::Semi, "`;` to terminate store statement")?;
        let kind = if volatile {
            StmtKind::VolatileStore { ty, ptr, value }
        } else {
            StmtKind::UncheckedStore { ty, ptr, value }
        };
        Ok(Stmt {
            kind,
            span: Span::new(start, close.end),
        })
    }

    fn parse_expr_stmt(&mut self, start: usize) -> Result<Stmt, ParseError> {
        let expr = self.parse_expr()?;
        let close = self.expect(TokenKind::Semi, "`;` to terminate expression statement")?;
        Ok(Stmt {
            kind: StmtKind::Expr(expr),
            span: Span::new(start, close.end),
        })
    }

    fn parse_arg_list(&mut self) -> Result<Vec<Expr>, ParseError> {
        // `(` already consumed.
        let mut args = Vec::new();
        if matches!(self.peek().kind, TokenKind::RParen) {
            self.advance();
            return Ok(args);
        }
        loop {
            args.push(self.parse_expr()?);
            match self.peek().kind {
                TokenKind::Comma => {
                    self.advance();
                    if matches!(self.peek().kind, TokenKind::RParen) {
                        self.advance();
                        return Ok(args);
                    }
                }
                TokenKind::RParen => {
                    self.advance();
                    return Ok(args);
                }
                TokenKind::Eof => {
                    return Err(ParseError::UnexpectedEof {
                        context: "argument list",
                    });
                }
                _ => {
                    let t = self.peek().clone();
                    return Err(ParseError::Expected {
                        expected: "`,` or `)` in argument list",
                        found: t.kind,
                        at: t.span.start,
                    });
                }
            }
        }
    }

    // ─── Expression parser (Pratt; §2.6) ──────────────────────────────────

    /// Parse one expression. Pratt-style with binding-power dispatch.
    fn parse_expr(&mut self) -> Result<Expr, ParseError> {
        self.parse_expr_bp(0)
    }

    /// Pratt main loop: parse atoms / prefix operators, then drive postfix
    /// and infix operators by binding power.
    fn parse_expr_bp(&mut self, min_bp: u8) -> Result<Expr, ParseError> {
        let start = self.peek().span.start;
        let mut lhs = self.parse_prefix(start)?;

        loop {
            // Postfix operators: `.field` / `.method(args)` / `[idx]` /
            // `(args)`. These are higher precedence than any infix, so
            // they're checked first and don't participate in min_bp.
            match self.peek().kind {
                TokenKind::Dot => {
                    self.advance(); // `.`
                    let (name, name_span) = self.expect_ident("field or method name after `.`")?;
                    if matches!(self.peek().kind, TokenKind::LParen) {
                        self.advance();
                        let args = self.parse_arg_list()?;
                        let end = self.tokens[self.pos.saturating_sub(1)].span.end;
                        lhs = Expr {
                            span: Span::new(lhs.span.start, end),
                            kind: ExprKind::MethodCall {
                                obj: Box::new(lhs),
                                method: name,
                                args,
                            },
                        };
                    } else {
                        lhs = Expr {
                            span: Span::new(lhs.span.start, name_span.end),
                            kind: ExprKind::FieldAccess {
                                obj: Box::new(lhs),
                                field: name,
                            },
                        };
                    }
                    continue;
                }
                TokenKind::LBracket => {
                    self.advance(); // `[`
                    let index = self.parse_expr()?;
                    let close = self.expect(TokenKind::RBracket, "`]` to close index")?;
                    lhs = Expr {
                        span: Span::new(lhs.span.start, close.end),
                        kind: ExprKind::Index {
                            obj: Box::new(lhs),
                            index: Box::new(index),
                        },
                    };
                    continue;
                }
                TokenKind::LParen => {
                    self.advance(); // `(`
                    let args = self.parse_arg_list()?;
                    let end = self.tokens[self.pos.saturating_sub(1)].span.end;
                    lhs = Expr {
                        span: Span::new(lhs.span.start, end),
                        kind: ExprKind::Call {
                            callee: Box::new(lhs),
                            args,
                        },
                    };
                    continue;
                }
                TokenKind::KwAs => {
                    // `as` cast — high precedence, between unary and multiplicative.
                    let cast_bp: u8 = 23;
                    if cast_bp < min_bp {
                        break;
                    }
                    self.advance();
                    let ty = self.parse_type()?;
                    let end = ty.span.end;
                    lhs = Expr {
                        span: Span::new(lhs.span.start, end),
                        kind: ExprKind::Cast {
                            value: Box::new(lhs),
                            ty,
                        },
                    };
                    continue;
                }
                _ => {}
            }

            // Infix operators with binding-power dispatch.
            if let Some((l_bp, r_bp, op)) = infix_op(&self.peek().kind) {
                if l_bp < min_bp {
                    break;
                }
                self.advance(); // operator
                let rhs = self.parse_expr_bp(r_bp)?;
                let span = Span::new(lhs.span.start, rhs.span.end);
                lhs = Expr {
                    span,
                    kind: ExprKind::Binary {
                        op,
                        lhs: Box::new(lhs),
                        rhs: Box::new(rhs),
                    },
                };
                continue;
            }

            // Range operator: `a .. b` / `a ..= b`. Lowest precedence.
            // Matched here because it isn't strictly a typical infix
            // (Rust treats it as left-associative with very low priority).
            if matches!(self.peek().kind, TokenKind::DotDot | TokenKind::DotDotEq) {
                let range_bp: u8 = 1;
                if range_bp < min_bp {
                    break;
                }
                let inclusive = matches!(self.peek().kind, TokenKind::DotDotEq);
                self.advance();
                let rhs = self.parse_expr_bp(range_bp + 1)?;
                let span = Span::new(lhs.span.start, rhs.span.end);
                lhs = Expr {
                    span,
                    kind: ExprKind::Range {
                        lo: Box::new(lhs),
                        hi: Box::new(rhs),
                        inclusive,
                    },
                };
                continue;
            }

            break;
        }

        Ok(lhs)
    }

    /// Parse an atom or a prefix-operator expression.
    fn parse_prefix(&mut self, _start: usize) -> Result<Expr, ParseError> {
        let tok = self.peek().clone();
        match tok.kind {
            TokenKind::Minus => self.parse_unary_op(UnaryOp::Neg),
            TokenKind::Bang => self.parse_unary_op(UnaryOp::Not),
            TokenKind::Tilde => self.parse_unary_op(UnaryOp::BitNot),
            TokenKind::Star => self.parse_unary_op(UnaryOp::Deref),
            TokenKind::Amp => self.parse_borrow_expr(),
            _ => self.parse_atom(),
        }
    }

    fn parse_unary_op(&mut self, op: UnaryOp) -> Result<Expr, ParseError> {
        let start = self.peek().span.start;
        self.advance();
        // Unary operators bind tighter than any infix; recurse with high BP.
        let operand = self.parse_expr_bp(25)?;
        let end = operand.span.end;
        Ok(Expr {
            span: Span::new(start, end),
            kind: ExprKind::Unary {
                op,
                operand: Box::new(operand),
            },
        })
    }

    fn parse_borrow_expr(&mut self) -> Result<Expr, ParseError> {
        let start = self.peek().span.start;
        self.advance(); // `&`
        let mutable = if matches!(self.peek().kind, TokenKind::KwMut) {
            self.advance();
            true
        } else {
            false
        };
        let operand = self.parse_expr_bp(25)?;
        let end = operand.span.end;
        Ok(Expr {
            span: Span::new(start, end),
            kind: ExprKind::Ref {
                mutable,
                operand: Box::new(operand),
            },
        })
    }

    fn parse_atom(&mut self) -> Result<Expr, ParseError> {
        let tok = self.peek().clone();
        let start = tok.span.start;
        match tok.kind {
            TokenKind::IntLiteral(s) => {
                self.advance();
                Ok(Expr {
                    kind: ExprKind::IntLit(s),
                    span: tok.span,
                })
            }
            TokenKind::HexLiteral(s) => {
                self.advance();
                Ok(Expr {
                    kind: ExprKind::HexLit(s),
                    span: tok.span,
                })
            }
            TokenKind::BinLiteral(s) => {
                self.advance();
                Ok(Expr {
                    kind: ExprKind::BinLit(s),
                    span: tok.span,
                })
            }
            TokenKind::FloatLiteral(s) => {
                self.advance();
                Ok(Expr {
                    kind: ExprKind::FloatLit(s),
                    span: tok.span,
                })
            }
            TokenKind::CharLiteral(c) => {
                self.advance();
                Ok(Expr {
                    kind: ExprKind::CharLit(c),
                    span: tok.span,
                })
            }
            TokenKind::ByteLiteral(b) => {
                self.advance();
                Ok(Expr {
                    kind: ExprKind::ByteLit(b),
                    span: tok.span,
                })
            }
            TokenKind::StringLiteral(s) => {
                self.advance();
                Ok(Expr {
                    kind: ExprKind::StringLit(s),
                    span: tok.span,
                })
            }
            TokenKind::KwTrue => {
                self.advance();
                Ok(Expr {
                    kind: ExprKind::BoolLit(true),
                    span: tok.span,
                })
            }
            TokenKind::KwFalse => {
                self.advance();
                Ok(Expr {
                    kind: ExprKind::BoolLit(false),
                    span: tok.span,
                })
            }
            TokenKind::KwNull => {
                self.advance();
                Ok(Expr {
                    kind: ExprKind::Null,
                    span: tok.span,
                })
            }
            TokenKind::Ident(_) | TokenKind::KwSelf | TokenKind::KwSelfType => {
                self.parse_path_or_state_expr(start)
            }
            TokenKind::LParen => self.parse_paren_or_tuple_expr(start),
            TokenKind::LBracket => self.parse_array_expr(start),
            TokenKind::KwHashUncheckedLoad => {
                self.parse_unsafe_load_expr(start, /*volatile=*/ false)
            }
            TokenKind::KwHashVolatileLoad => {
                self.parse_unsafe_load_expr(start, /*volatile=*/ true)
            }
            TokenKind::KwHashUncheckedCast => self.parse_unchecked_cast_expr(start),
            TokenKind::KwHashUncheckedOffset => self.parse_unchecked_offset_expr(start),
            TokenKind::Eof => Err(ParseError::UnexpectedEof {
                context: "expression",
            }),
            other => Err(ParseError::Expected {
                expected: "expression",
                found: other,
                at: tok.span.start,
            }),
        }
    }

    fn parse_path_or_state_expr(&mut self, start: usize) -> Result<Expr, ParseError> {
        // Consume the first ident (or KwSelf / KwSelfType).
        let first_tok = self.peek().clone();
        let first = match first_tok.kind {
            TokenKind::Ident(s) => {
                self.advance();
                s
            }
            TokenKind::KwSelf => {
                self.advance();
                "self".to_owned()
            }
            TokenKind::KwSelfType => {
                self.advance();
                "Self".to_owned()
            }
            _ => unreachable!("dispatch ensured Ident / KwSelf / KwSelfType"),
        };
        let mut end = first_tok.span.end;

        // `Auto@state` postfix (Refinement #5d)?
        if matches!(self.peek().kind, TokenKind::KwAtState) {
            let at_tok = self.peek().clone();
            self.advance();
            return Ok(Expr {
                kind: ExprKind::StateRead(first),
                span: Span::new(start, at_tok.span.end),
            });
        }

        let mut segments = vec![first];
        while matches!(self.peek().kind, TokenKind::ColonColon) {
            self.advance();
            let (seg, seg_span) = self.expect_ident("path segment after `::`")?;
            segments.push(seg);
            end = seg_span.end;
        }
        Ok(Expr {
            kind: ExprKind::Path(segments),
            span: Span::new(start, end),
        })
    }

    fn parse_paren_or_tuple_expr(&mut self, start: usize) -> Result<Expr, ParseError> {
        self.advance(); // `(`
        // Empty parens `()` is unit — emitted as an empty Tuple. Real unit
        // expression handling can come later; for now reject explicitly.
        if matches!(self.peek().kind, TokenKind::RParen) {
            let close = self.expect(TokenKind::RParen, "internal RParen")?;
            return Ok(Expr {
                kind: ExprKind::Tuple(Vec::new()),
                span: Span::new(start, close.end),
            });
        }
        let first = self.parse_expr()?;
        match self.peek().kind {
            TokenKind::RParen => {
                let close = self.expect(TokenKind::RParen, "internal RParen")?;
                Ok(Expr {
                    kind: ExprKind::Paren(Box::new(first)),
                    span: Span::new(start, close.end),
                })
            }
            TokenKind::Comma => {
                self.advance();
                let mut elements = vec![first];
                loop {
                    if matches!(self.peek().kind, TokenKind::RParen) {
                        break;
                    }
                    elements.push(self.parse_expr()?);
                    match self.peek().kind {
                        TokenKind::Comma => {
                            self.advance();
                        }
                        TokenKind::RParen => break,
                        TokenKind::Eof => {
                            return Err(ParseError::UnexpectedEof {
                                context: "tuple expression",
                            });
                        }
                        _ => {
                            let t = self.peek().clone();
                            return Err(ParseError::Expected {
                                expected: "`,` or `)` in tuple expression",
                                found: t.kind,
                                at: t.span.start,
                            });
                        }
                    }
                }
                let close = self.expect(TokenKind::RParen, "`)` to close tuple expression")?;
                Ok(Expr {
                    kind: ExprKind::Tuple(elements),
                    span: Span::new(start, close.end),
                })
            }
            _ => {
                let t = self.peek().clone();
                Err(ParseError::Expected {
                    expected: "`,` or `)` in parenthesised expression",
                    found: t.kind,
                    at: t.span.start,
                })
            }
        }
    }

    fn parse_array_expr(&mut self, start: usize) -> Result<Expr, ParseError> {
        self.advance(); // `[`
        if matches!(self.peek().kind, TokenKind::RBracket) {
            let close = self.expect(TokenKind::RBracket, "internal RBracket")?;
            return Ok(Expr {
                kind: ExprKind::Array(Vec::new()),
                span: Span::new(start, close.end),
            });
        }
        let first = self.parse_expr()?;
        match self.peek().kind {
            TokenKind::Semi => {
                self.advance();
                let count = self.parse_expr()?;
                let close = self.expect(TokenKind::RBracket, "`]` to close array-repeat literal")?;
                Ok(Expr {
                    kind: ExprKind::ArrayRepeat {
                        value: Box::new(first),
                        count: Box::new(count),
                    },
                    span: Span::new(start, close.end),
                })
            }
            TokenKind::Comma => {
                self.advance();
                let mut elements = vec![first];
                loop {
                    if matches!(self.peek().kind, TokenKind::RBracket) {
                        break;
                    }
                    elements.push(self.parse_expr()?);
                    match self.peek().kind {
                        TokenKind::Comma => {
                            self.advance();
                        }
                        TokenKind::RBracket => break,
                        TokenKind::Eof => {
                            return Err(ParseError::UnexpectedEof {
                                context: "array literal",
                            });
                        }
                        _ => {
                            let t = self.peek().clone();
                            return Err(ParseError::Expected {
                                expected: "`,` or `]` in array literal",
                                found: t.kind,
                                at: t.span.start,
                            });
                        }
                    }
                }
                let close = self.expect(TokenKind::RBracket, "`]` to close array literal")?;
                Ok(Expr {
                    kind: ExprKind::Array(elements),
                    span: Span::new(start, close.end),
                })
            }
            TokenKind::RBracket => {
                let close = self.expect(TokenKind::RBracket, "internal RBracket")?;
                Ok(Expr {
                    kind: ExprKind::Array(vec![first]),
                    span: Span::new(start, close.end),
                })
            }
            _ => {
                let t = self.peek().clone();
                Err(ParseError::Expected {
                    expected: "`,` (more elements), `;` (repeat), or `]` (close)",
                    found: t.kind,
                    at: t.span.start,
                })
            }
        }
    }

    fn parse_unsafe_load_expr(
        &mut self,
        start: usize,
        volatile: bool,
    ) -> Result<Expr, ParseError> {
        self.advance(); // primitive keyword
        self.expect(TokenKind::Lt, "`<` after load primitive")?;
        let ty = self.parse_type()?;
        self.expect(TokenKind::Gt, "`>` to close load type argument")?;
        self.expect(TokenKind::LParen, "`(` to open load arguments")?;
        let ptr = self.parse_expr()?;
        let close = self.expect(TokenKind::RParen, "`)` to close load arguments")?;
        let kind = if volatile {
            ExprKind::VolatileLoad {
                ty,
                ptr: Box::new(ptr),
            }
        } else {
            ExprKind::UncheckedLoad {
                ty,
                ptr: Box::new(ptr),
            }
        };
        Ok(Expr {
            kind,
            span: Span::new(start, close.end),
        })
    }

    fn parse_unchecked_cast_expr(&mut self, start: usize) -> Result<Expr, ParseError> {
        self.advance(); // `#unchecked_cast`
        self.expect(TokenKind::Lt, "`<` after `#unchecked_cast`")?;
        let from_ty = self.parse_type()?;
        self.expect(TokenKind::Comma, "`,` between cast source and target types")?;
        let to_ty = self.parse_type()?;
        self.expect(TokenKind::Gt, "`>` to close cast type arguments")?;
        self.expect(TokenKind::LParen, "`(` to open cast arguments")?;
        let reason_tok = self.peek().clone();
        let reason = match reason_tok.kind {
            TokenKind::StringLiteral(s) => {
                self.advance();
                if s.trim().is_empty() {
                    return Err(ParseError::Expected {
                        expected: "non-empty reason string for `#unchecked_cast` (Refinement #19a)",
                        found: TokenKind::StringLiteral(s),
                        at: reason_tok.span.start,
                    });
                }
                s
            }
            TokenKind::Eof => {
                return Err(ParseError::UnexpectedEof {
                    context: "`#unchecked_cast` reason string",
                });
            }
            other => {
                return Err(ParseError::Expected {
                    expected: "string-literal reason as first argument to `#unchecked_cast`",
                    found: other,
                    at: reason_tok.span.start,
                });
            }
        };
        self.expect(TokenKind::Comma, "`,` between cast reason and value")?;
        let value = self.parse_expr()?;
        let close = self.expect(TokenKind::RParen, "`)` to close cast arguments")?;
        Ok(Expr {
            kind: ExprKind::UncheckedCast {
                from_ty,
                to_ty,
                reason,
                value: Box::new(value),
            },
            span: Span::new(start, close.end),
        })
    }

    fn parse_unchecked_offset_expr(&mut self, start: usize) -> Result<Expr, ParseError> {
        self.advance(); // `#unchecked_offset`
        self.expect(TokenKind::Lt, "`<` after `#unchecked_offset`")?;
        let ty = self.parse_type()?;
        self.expect(TokenKind::Gt, "`>` to close offset type argument")?;
        self.expect(TokenKind::LParen, "`(` to open offset arguments")?;
        let ptr = self.parse_expr()?;
        self.expect(TokenKind::Comma, "`,` between offset ptr and count")?;
        let n = self.parse_expr()?;
        let close = self.expect(TokenKind::RParen, "`)` to close offset arguments")?;
        Ok(Expr {
            kind: ExprKind::UncheckedOffset {
                ty,
                ptr: Box::new(ptr),
                n: Box::new(n),
            },
            span: Span::new(start, close.end),
        })
    }
}

/// Map an infix-operator token to `(left_bp, right_bp, BinaryOp)`.
///
/// Higher binding power = higher precedence. Left-associative operators
/// have `right_bp = left_bp + 1`; right-associative would have
/// `right_bp = left_bp` (we don't have any in v0.1).
///
/// Comparisons are non-associative in spec but we treat them as
/// left-associative at parse time and reject chains like `a < b < c`
/// in `clifford-types` later.
fn infix_op(kind: &TokenKind) -> Option<(u8, u8, BinaryOp)> {
    Some(match kind {
        // Range is handled separately at bp=1 in parse_expr_bp.
        TokenKind::PipePipe => (3, 4, BinaryOp::Or),
        TokenKind::AmpAmp => (5, 6, BinaryOp::And),
        TokenKind::EqEq => (7, 8, BinaryOp::Eq),
        TokenKind::BangEq => (7, 8, BinaryOp::Ne),
        TokenKind::Lt => (7, 8, BinaryOp::Lt),
        TokenKind::LtEq => (7, 8, BinaryOp::Le),
        TokenKind::Gt => (7, 8, BinaryOp::Gt),
        TokenKind::GtEq => (7, 8, BinaryOp::Ge),
        TokenKind::Pipe => (9, 10, BinaryOp::BitOr),
        TokenKind::Caret => (11, 12, BinaryOp::BitXor),
        TokenKind::Amp => (13, 14, BinaryOp::BitAnd),
        TokenKind::Shl => (15, 16, BinaryOp::Shl),
        TokenKind::Shr => (15, 16, BinaryOp::Shr),
        TokenKind::Plus => (17, 18, BinaryOp::Add),
        TokenKind::Minus => (17, 18, BinaryOp::Sub),
        TokenKind::Star => (19, 20, BinaryOp::Mul),
        TokenKind::Slash => (19, 20, BinaryOp::Div),
        TokenKind::Percent => (19, 20, BinaryOp::Rem),
        // `as` cast is handled separately at bp=23.
        // Unary prefix is handled separately at bp=25.
        // Postfix (./[/() are handled separately, no bp needed.
        _ => return None,
    })
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
        // Now goes through `parse_param_list` which expects `(`.
        assert!(matches!(
            err,
            ParseError::Expected {
                expected: "`(` to open parameter list",
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
    fn interface_decl_empty() {
        let p = parse_str("#interface Serial { }").expect("parse empty #interface");
        match &p.items[0] {
            Item::Interface(InterfaceDecl {
                name,
                methods,
                generic_params,
                ..
            }) => {
                assert_eq!(name, "Serial");
                assert!(methods.is_empty());
                assert!(generic_params.is_empty());
            }
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
                let TraitRef { name, generic_args, .. } = &trait_list[0];
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

    // ─── Slice 4: parameters + trait list on @fn ─────────────────────────

    use clifford_ast::Param;

    #[test]
    fn fn_with_one_param() {
        let p = parse_str("@fn double(x: u32) -> u32 { }").expect("parse @fn one param");
        match &p.items[0] {
            Item::Fn(FnDecl { params, .. }) => {
                assert_eq!(params.len(), 1);
                let Param {
                    mutable, name, ty, ..
                } = &params[0];
                assert!(!mutable);
                assert_eq!(name, "x");
                assert_eq!(ty.kind, TypeKind::Primitive(PrimitiveType::U32));
            }
            other => panic!("expected Fn, got {:?}", other),
        }
    }

    #[test]
    fn fn_with_multiple_params() {
        let p = parse_str("@fn add(a: u32, b: u32) -> u32 { }").expect("parse @fn add");
        match &p.items[0] {
            Item::Fn(FnDecl { params, .. }) => {
                assert_eq!(params.len(), 2);
                assert_eq!(params[0].name, "a");
                assert_eq!(params[1].name, "b");
            }
            other => panic!("expected Fn, got {:?}", other),
        }
    }

    #[test]
    fn fn_with_mut_param() {
        // `mut buf: &mut [u8; 12]` — common out-parameter shape.
        let p =
            parse_str("@fn fill(mut buf: &mut [u8; 12]) { }").expect("parse @fn with mut param");
        match &p.items[0] {
            Item::Fn(FnDecl { params, .. }) => {
                assert_eq!(params.len(), 1);
                assert!(params[0].mutable);
                assert_eq!(params[0].name, "buf");
                assert!(matches!(params[0].ty.kind, TypeKind::Ref(_)));
            }
            other => panic!("expected Fn, got {:?}", other),
        }
    }

    #[test]
    fn fn_with_complex_param_types() {
        // `(snap: &SchedulerSnapshot, deadline: u64)` — the kernel
        // scheduler signature shape from the worked example.
        let p = parse_str(
            "@fn pick_next(snap: &SchedulerSnapshot, deadline: u64) -> Decision { }",
        )
        .expect("parse scheduler-shape signature");
        match &p.items[0] {
            Item::Fn(FnDecl {
                params,
                return_type,
                ..
            }) => {
                assert_eq!(params.len(), 2);
                assert_eq!(params[0].name, "snap");
                assert!(matches!(params[0].ty.kind, TypeKind::Ref(_)));
                assert_eq!(params[1].name, "deadline");
                assert_eq!(params[1].ty.kind, TypeKind::Primitive(PrimitiveType::U64));
                let rt = return_type.as_ref().expect("return type");
                match &rt.kind {
                    TypeKind::Path(PathType { segments, .. }) => {
                        assert_eq!(segments, &vec!["Decision".to_string()]);
                    }
                    other => panic!("expected Path, got {:?}", other),
                }
            }
            other => panic!("expected Fn, got {:?}", other),
        }
    }

    #[test]
    fn fn_with_trailing_comma_in_params() {
        let p = parse_str("@fn f(a: u32, b: u32,) { }").expect("trailing comma is allowed");
        match &p.items[0] {
            Item::Fn(FnDecl { params, .. }) => assert_eq!(params.len(), 2),
            other => panic!("expected Fn, got {:?}", other),
        }
    }

    #[test]
    fn fn_with_trait_list() {
        let p = parse_str("@fn pure_helper(x: u32) -> u32 $ [Pure] { }")
            .expect("parse @fn $ [Pure]");
        match &p.items[0] {
            Item::Fn(FnDecl { trait_list, .. }) => {
                assert_eq!(trait_list.len(), 1);
                assert_eq!(trait_list[0].name, "Pure");
            }
            other => panic!("expected Fn, got {:?}", other),
        }
    }

    #[test]
    fn fn_with_multi_trait_list() {
        let p = parse_str("@fn read_state(s: &Foo) -> u32 $ [Readable, Observable] { }")
            .expect("parse multi-trait list");
        match &p.items[0] {
            Item::Fn(FnDecl { trait_list, .. }) => {
                let names: Vec<_> = trait_list.iter().map(|t| t.name.as_str()).collect();
                assert_eq!(names, vec!["Readable", "Observable"]);
            }
            other => panic!("expected Fn, got {:?}", other),
        }
    }

    #[test]
    fn fn_with_generic_trait_in_list() {
        let p = parse_str("@fn iter() -> u32 $ [PointerAuditor<Sanitizer>] { }")
            .expect("parse generic trait in list");
        match &p.items[0] {
            Item::Fn(FnDecl { trait_list, .. }) => {
                assert_eq!(trait_list.len(), 1);
                assert_eq!(trait_list[0].name, "PointerAuditor");
                assert_eq!(trait_list[0].generic_args.len(), 1);
            }
            other => panic!("expected Fn, got {:?}", other),
        }
    }

    #[test]
    fn fn_no_trait_list_keeps_empty() {
        let p = parse_str("@fn anon() { }").expect("no $ clause");
        match &p.items[0] {
            Item::Fn(FnDecl { trait_list, .. }) => assert!(trait_list.is_empty()),
            other => panic!("expected Fn, got {:?}", other),
        }
    }

    #[test]
    fn fn_full_signature() {
        // Everything together: params, return type, trait list.
        let src =
            "@fn cmd_is_help(buf: &[u8], min_len: usize) -> bool $ [Pure] { }";
        let p = parse_str(src).expect("parse full @fn signature");
        match &p.items[0] {
            Item::Fn(FnDecl {
                params,
                return_type,
                trait_list,
                ..
            }) => {
                assert_eq!(params.len(), 2);
                assert!(return_type.is_some());
                assert_eq!(trait_list.len(), 1);
                assert_eq!(trait_list[0].name, "Pure");
            }
            other => panic!("expected Fn, got {:?}", other),
        }
    }

    #[test]
    fn param_requires_colon_and_type() {
        let err = parse_str("@fn f(x) { }").expect_err("missing `: type`");
        assert!(matches!(
            err,
            ParseError::Expected {
                expected: "`:` between parameter name and type",
                ..
            }
        ));
    }

    #[test]
    fn param_requires_name() {
        let err = parse_str("@fn f(: u32) { }").expect_err("missing param name");
        assert!(matches!(
            err,
            ParseError::Expected {
                expected: "parameter name",
                ..
            }
        ));
    }

    #[test]
    fn effect_with_params() {
        let p = parse_str("#effect log(b: u8) #mutates: [Logger] { }")
            .expect("parse #effect with params");
        match &p.items[0] {
            Item::Effect(EffectDecl {
                params,
                return_type,
                ..
            }) => {
                assert_eq!(params.len(), 1);
                assert_eq!(params[0].name, "b");
                assert!(return_type.is_none());
            }
            other => panic!("expected Effect, got {:?}", other),
        }
    }

    #[test]
    fn effect_with_params_and_return() {
        let p = parse_str(
            "#effect read_byte() -> u8 #mutates: [Usart1] { }",
        )
        .expect("parse #effect -> u8");
        match &p.items[0] {
            Item::Effect(EffectDecl {
                params,
                return_type,
                ..
            }) => {
                assert!(params.is_empty());
                assert_eq!(
                    return_type.as_ref().unwrap().kind,
                    TypeKind::Primitive(PrimitiveType::U8)
                );
            }
            other => panic!("expected Effect, got {:?}", other),
        }
    }

    #[test]
    fn interrupt_with_params_and_return() {
        let p = parse_str(
            "#interrupt CustomISR(arg: u32) -> u32 #mutates: [Sched] #priority: HIGH { }",
        )
        .expect("parse #interrupt with params + return");
        match &p.items[0] {
            Item::Interrupt(InterruptDecl {
                params,
                return_type,
                ..
            }) => {
                assert_eq!(params.len(), 1);
                assert!(return_type.is_some());
            }
            other => panic!("expected Interrupt, got {:?}", other),
        }
    }

    // ─── Slice 5: @type aliases and ADTs (§2.3) ──────────────────────────

    use clifford_ast::{GenericParam, TypeBody, TypeDecl, VariantData};

    #[test]
    fn type_alias_simple() {
        let p = parse_str("@type ByteAlias = u8;").expect("parse alias");
        match &p.items[0] {
            Item::Type(TypeDecl {
                name,
                generic_params,
                body,
                ..
            }) => {
                assert_eq!(name, "ByteAlias");
                assert!(generic_params.is_empty());
                match body {
                    TypeBody::Alias(ty) => {
                        assert_eq!(ty.kind, TypeKind::Primitive(PrimitiveType::U8));
                    }
                    other => panic!("expected Alias, got {:?}", other),
                }
            }
            other => panic!("expected Type, got {:?}", other),
        }
        assert_eq!(p.items[0].layer(), Layer::Functional);
    }

    #[test]
    fn type_alias_complex() {
        let p = parse_str("@type Buf = &[u8; 64];").expect("parse complex alias");
        match &p.items[0] {
            Item::Type(TypeDecl {
                body: TypeBody::Alias(ty),
                ..
            }) => {
                assert!(matches!(ty.kind, TypeKind::Ref(_)));
            }
            other => panic!("expected Type/Alias, got {:?}", other),
        }
    }

    #[test]
    fn type_adt_unit_variants() {
        let p = parse_str("@type Color = Red | Green | Blue;").expect("parse C-style enum");
        match &p.items[0] {
            Item::Type(TypeDecl {
                body: TypeBody::Adt(variants),
                ..
            }) => {
                let names: Vec<_> = variants.iter().map(|v| v.name.as_str()).collect();
                assert_eq!(names, vec!["Red", "Green", "Blue"]);
                for v in variants {
                    assert!(matches!(v.data, VariantData::None));
                }
            }
            other => panic!("expected Type/Adt, got {:?}", other),
        }
    }

    #[test]
    fn type_adt_with_leading_pipe() {
        let p = parse_str("@type Color = | Red | Green | Blue;").expect("parse leading-pipe ADT");
        match &p.items[0] {
            Item::Type(TypeDecl {
                body: TypeBody::Adt(variants),
                ..
            }) => assert_eq!(variants.len(), 3),
            other => panic!("expected Type/Adt, got {:?}", other),
        }
    }

    #[test]
    fn type_adt_tuple_variants() {
        let p = parse_str("@type Result = | Ok(u32) | Err(bool);").expect("parse Result-shape");
        match &p.items[0] {
            Item::Type(TypeDecl {
                body: TypeBody::Adt(variants),
                ..
            }) => {
                assert_eq!(variants.len(), 2);
                match &variants[0].data {
                    VariantData::Tuple(types) => {
                        assert_eq!(types.len(), 1);
                        assert_eq!(types[0].kind, TypeKind::Primitive(PrimitiveType::U32));
                    }
                    other => panic!("expected Tuple variant, got {:?}", other),
                }
                match &variants[1].data {
                    VariantData::Tuple(types) => {
                        assert_eq!(types[0].kind, TypeKind::Primitive(PrimitiveType::Bool));
                    }
                    other => panic!("expected Tuple variant, got {:?}", other),
                }
            }
            other => panic!("expected Type/Adt, got {:?}", other),
        }
    }

    #[test]
    fn type_adt_struct_variants() {
        let p = parse_str("@type Shape = | Circle { r: f32 } | Square { side: f32 };")
            .expect("parse struct-style variants");
        match &p.items[0] {
            Item::Type(TypeDecl {
                body: TypeBody::Adt(variants),
                ..
            }) => {
                assert_eq!(variants.len(), 2);
                match &variants[0].data {
                    VariantData::Struct(fields) => {
                        assert_eq!(fields.len(), 1);
                        assert_eq!(fields[0].name, "r");
                    }
                    other => panic!("expected Struct variant, got {:?}", other),
                }
            }
            other => panic!("expected Type/Adt, got {:?}", other),
        }
    }

    #[test]
    fn type_adt_mixed_variant_kinds() {
        let p = parse_str(
            "@type Event = Tick | Tx(u8) | Reading { ch: u8, value: u16 } | Halt;",
        )
        .expect("parse mixed-kinds ADT");
        match &p.items[0] {
            Item::Type(TypeDecl {
                body: TypeBody::Adt(variants),
                ..
            }) => {
                assert_eq!(variants.len(), 4);
                assert!(matches!(variants[0].data, VariantData::None));
                assert!(matches!(variants[1].data, VariantData::Tuple(_)));
                assert!(matches!(variants[2].data, VariantData::Struct(_)));
                assert!(matches!(variants[3].data, VariantData::None));
            }
            other => panic!("expected Type/Adt, got {:?}", other),
        }
    }

    #[test]
    fn type_with_generic_params() {
        let p = parse_str("@type Result<T, E> = | Ok(T) | Err(E);").expect("parse generic ADT");
        match &p.items[0] {
            Item::Type(TypeDecl {
                generic_params,
                body: TypeBody::Adt(variants),
                ..
            }) => {
                let names: Vec<_> = generic_params.iter().map(|p| p.name.as_str()).collect();
                assert_eq!(names, vec!["T", "E"]);
                for p in generic_params {
                    assert!(p.bounds.is_empty());
                }
                assert_eq!(variants.len(), 2);
            }
            other => panic!("expected Type/Adt with generics, got {:?}", other),
        }
    }

    #[test]
    fn type_with_bounded_generic_params() {
        let p = parse_str("@type Wrapper<T: Pure + Readable> = T;")
            .expect("parse bounded generic alias");
        match &p.items[0] {
            Item::Type(TypeDecl { generic_params, .. }) => {
                assert_eq!(generic_params.len(), 1);
                let GenericParam { name, bounds, .. } = &generic_params[0];
                assert_eq!(name, "T");
                let bound_names: Vec<_> = bounds.iter().map(|b| b.name.as_str()).collect();
                assert_eq!(bound_names, vec!["Pure", "Readable"]);
            }
            other => panic!("expected Type with bounded generic, got {:?}", other),
        }
    }

    #[test]
    fn type_alias_must_end_in_semi() {
        let err = parse_str("@type Foo = u32").expect_err("missing `;`");
        assert!(matches!(err, ParseError::UnexpectedEof { .. }));
    }

    #[test]
    fn type_decl_must_have_eq() {
        let err = parse_str("@type Foo u32;").expect_err("missing `=`");
        assert!(matches!(
            err,
            ParseError::Expected {
                expected: "`=` after `@type` name",
                ..
            }
        ));
    }

    #[test]
    fn struct_variant_field_requires_colon() {
        let err = parse_str("@type Foo = Bar { x };").expect_err("field missing colon");
        assert!(matches!(
            err,
            ParseError::Expected {
                expected: "`:` between field name and type",
                ..
            }
        ));
    }

    #[test]
    fn nested_adt_in_alias() {
        // A type alias whose RHS is itself a path that would look like an
        // ADT variant if not for the generic args (`Result<u32, bool>`).
        let p = parse_str("@type IntOrBool = Result<u32, bool>;")
            .expect("parse alias to generic path");
        match &p.items[0] {
            Item::Type(TypeDecl {
                body: TypeBody::Alias(ty),
                ..
            }) => match &ty.kind {
                TypeKind::Path(PathType {
                    segments,
                    generic_args,
                }) => {
                    assert_eq!(segments, &vec!["Result".to_string()]);
                    assert_eq!(generic_args.len(), 2);
                }
                other => panic!("expected Path alias, got {:?}", other),
            },
            other => panic!("expected Type/Alias, got {:?}", other),
        }
    }

    #[test]
    fn variant_with_trailing_comma() {
        let p = parse_str("@type T = Foo(u8, u16,) | Bar { f: u8, };")
            .expect("trailing commas allowed");
        match &p.items[0] {
            Item::Type(TypeDecl {
                body: TypeBody::Adt(variants),
                ..
            }) => {
                assert_eq!(variants.len(), 2);
                match &variants[0].data {
                    VariantData::Tuple(t) => assert_eq!(t.len(), 2),
                    _ => panic!(),
                }
                match &variants[1].data {
                    VariantData::Struct(f) => assert_eq!(f.len(), 1),
                    _ => panic!(),
                }
            }
            _ => panic!(),
        }
    }

    // ─── Slice 6: @trait + #interface bodies ─────────────────────────────

    use clifford_ast::{InterfaceMethod, TraitDecl, TraitMethod};

    #[test]
    fn interface_with_one_method() {
        let p = parse_str("#interface Serial { effect send_byte(b: u8); }")
            .expect("parse interface with one method");
        match &p.items[0] {
            Item::Interface(InterfaceDecl { methods, .. }) => {
                assert_eq!(methods.len(), 1);
                let InterfaceMethod {
                    name,
                    params,
                    return_type,
                    ..
                } = &methods[0];
                assert_eq!(name, "send_byte");
                assert_eq!(params.len(), 1);
                assert_eq!(params[0].name, "b");
                assert!(return_type.is_none());
            }
            other => panic!("expected Interface, got {:?}", other),
        }
    }

    #[test]
    fn interface_with_multiple_methods() {
        let src =
            "#interface Serial {\n  \
             effect send_byte(b: u8);\n  \
             effect recv_byte() -> u8;\n  \
             effect flush() -> bool;\n\
             }";
        let p = parse_str(src).expect("parse Serial interface");
        match &p.items[0] {
            Item::Interface(InterfaceDecl { methods, .. }) => {
                assert_eq!(methods.len(), 3);
                assert_eq!(methods[0].name, "send_byte");
                assert_eq!(methods[1].name, "recv_byte");
                assert_eq!(methods[2].name, "flush");
                // recv_byte has -> u8
                assert_eq!(
                    methods[1].return_type.as_ref().unwrap().kind,
                    TypeKind::Primitive(PrimitiveType::U8)
                );
            }
            other => panic!("expected Interface, got {:?}", other),
        }
    }

    #[test]
    fn interface_with_generic_params() {
        let p = parse_str("#interface Container<T> { effect put(item: T); }")
            .expect("parse generic interface");
        match &p.items[0] {
            Item::Interface(InterfaceDecl {
                generic_params,
                methods,
                ..
            }) => {
                assert_eq!(generic_params.len(), 1);
                assert_eq!(generic_params[0].name, "T");
                assert_eq!(methods.len(), 1);
            }
            other => panic!("expected Interface, got {:?}", other),
        }
    }

    #[test]
    fn interface_method_must_start_with_effect_keyword() {
        let err = parse_str("#interface X { send_byte(b: u8); }")
            .expect_err("missing `effect` keyword");
        assert!(matches!(
            err,
            ParseError::Expected {
                expected: "`effect` keyword to start an interface method signature",
                ..
            }
        ));
    }

    #[test]
    fn interface_method_requires_terminating_semi() {
        let err = parse_str("#interface X { effect foo() }").expect_err("missing `;`");
        assert!(matches!(
            err,
            ParseError::Expected {
                expected: "`;` to terminate interface method signature",
                ..
            }
        ));
    }

    #[test]
    fn trait_decl_empty() {
        let p = parse_str("@trait Marker { }").expect("parse empty trait");
        match &p.items[0] {
            Item::Trait(TraitDecl {
                name,
                generic_params,
                methods,
                ..
            }) => {
                assert_eq!(name, "Marker");
                assert!(generic_params.is_empty());
                assert!(methods.is_empty());
            }
            other => panic!("expected Trait, got {:?}", other),
        }
        assert_eq!(p.items[0].layer(), Layer::Functional);
    }

    #[test]
    fn trait_with_one_method() {
        let p = parse_str("@trait Initializable { @fn init(self) -> Self; }")
            .expect("parse trait with one method");
        match &p.items[0] {
            Item::Trait(TraitDecl { methods, .. }) => {
                assert_eq!(methods.len(), 1);
                let TraitMethod {
                    name,
                    params,
                    return_type,
                    trait_list,
                    ..
                } = &methods[0];
                assert_eq!(name, "init");
                assert_eq!(params.len(), 1);
                assert_eq!(params[0].name, "self");
                assert!(return_type.is_some());
                assert!(trait_list.is_empty());
            }
            other => panic!("expected Trait, got {:?}", other),
        }
    }

    #[test]
    fn trait_with_method_having_trait_list() {
        let p = parse_str("@trait Sensor { @fn read() -> u32 $ [Readable]; }")
            .expect("parse trait with $-marked method");
        match &p.items[0] {
            Item::Trait(TraitDecl { methods, .. }) => {
                assert_eq!(methods.len(), 1);
                assert_eq!(methods[0].trait_list.len(), 1);
                assert_eq!(methods[0].trait_list[0].name, "Readable");
            }
            other => panic!("expected Trait, got {:?}", other),
        }
    }

    #[test]
    fn trait_with_multiple_methods() {
        let src = "@trait Iterator<T> {\n  \
                   @fn next(self) -> Option;\n  \
                   @fn count(self) -> usize $ [Pure];\n\
                   }";
        let p = parse_str(src).expect("parse multi-method trait");
        match &p.items[0] {
            Item::Trait(TraitDecl {
                generic_params,
                methods,
                ..
            }) => {
                assert_eq!(generic_params.len(), 1);
                assert_eq!(generic_params[0].name, "T");
                assert_eq!(methods.len(), 2);
            }
            other => panic!("expected Trait, got {:?}", other),
        }
    }

    #[test]
    fn trait_method_must_start_with_at_fn() {
        let err = parse_str("@trait X { fn foo(); }").expect_err("missing `@fn`");
        assert!(matches!(
            err,
            ParseError::Expected {
                expected: "`@fn` to start a trait method signature",
                ..
            }
        ));
    }

    #[test]
    fn trait_method_requires_terminating_semi() {
        let err = parse_str("@trait X { @fn foo() }").expect_err("missing `;`");
        assert!(matches!(
            err,
            ParseError::Expected {
                expected: "`;` to terminate trait method signature",
                ..
            }
        ));
    }

    #[test]
    fn realistic_serial_interface_and_impl_pair() {
        // The Decision #16 worked example shape. The #impl body is still
        // empty pending statement parsing in slice 7, but the interface
        // body is fully populated.
        let src = "\
            #interface Serial {\n  \
              effect send_byte(b: u8);\n  \
              effect recv_byte() -> u8;\n\
            }\n\
            #automaton Usart1 { }\n\
            #impl Serial for Usart1 { }\n\
        ";
        let p = parse_str(src).expect("parse Serial + Usart1 + impl");
        assert_eq!(p.items.len(), 3);
        match &p.items[0] {
            Item::Interface(InterfaceDecl { methods, .. }) => {
                assert_eq!(methods.len(), 2);
            }
            other => panic!("expected Interface first, got {:?}", other),
        }
        match &p.items[2] {
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

    // ─── Slice 7: expressions, statements, function bodies (§2.6) ────────

    use clifford_ast::{
        AssignOp, BinaryOp, Block, Expr, ExprKind, FieldAssign, Stmt, StmtKind, UnaryOp,
    };

    /// Parse one expression in isolation; trailing junk is an error.
    fn parse_expr_str(src: &str) -> Result<Expr, ParseError> {
        let tokens = tokenize(src).expect("tokenize");
        crate::parse_expression(&tokens)
    }

    /// Parse a statement by wrapping it in a trivial fn body and pulling it
    /// out. Keeps test fixtures readable while exercising the full pipeline.
    fn parse_stmt_str(src: &str) -> Stmt {
        let wrapped = format!("@fn _t() {{ {src} }}");
        let p = parse_str(&wrapped).expect("parse stmt-wrapping fn");
        match &p.items[0] {
            Item::Fn(decl) => {
                assert_eq!(decl.body.stmts.len(), 1, "expected exactly one statement");
                decl.body.stmts[0].clone()
            }
            other => panic!("expected Fn, got {:?}", other),
        }
    }

    fn parse_body_str(src: &str) -> Block {
        let wrapped = format!("@fn _t() {{ {src} }}");
        let p = parse_str(&wrapped).expect("parse body-wrapping fn");
        match &p.items[0] {
            Item::Fn(decl) => decl.body.clone(),
            other => panic!("expected Fn, got {:?}", other),
        }
    }

    // ── Atoms ────────────────────────────────────────────────────────────

    #[test]
    fn expr_int_literal() {
        let e = parse_expr_str("42").unwrap();
        assert_eq!(e.kind, ExprKind::IntLit("42".into()));
    }

    #[test]
    fn expr_hex_literal() {
        let e = parse_expr_str("0xDEAD_BEEF").unwrap();
        assert_eq!(e.kind, ExprKind::HexLit("0xDEAD_BEEF".into()));
    }

    #[test]
    fn expr_bin_literal() {
        let e = parse_expr_str("0b1010_0101").unwrap();
        assert_eq!(e.kind, ExprKind::BinLit("0b1010_0101".into()));
    }

    #[test]
    fn expr_float_literal() {
        let e = parse_expr_str("3.14").unwrap();
        assert_eq!(e.kind, ExprKind::FloatLit("3.14".into()));
    }

    #[test]
    fn expr_char_literal() {
        let e = parse_expr_str("'A'").unwrap();
        assert_eq!(e.kind, ExprKind::CharLit('A'));
    }

    #[test]
    fn expr_byte_literal() {
        let e = parse_expr_str("b'A'").unwrap();
        assert_eq!(e.kind, ExprKind::ByteLit(b'A'));
    }

    #[test]
    fn expr_string_literal() {
        let e = parse_expr_str(r#""hello""#).unwrap();
        assert_eq!(e.kind, ExprKind::StringLit("hello".into()));
    }

    #[test]
    fn expr_bool_literals() {
        assert_eq!(parse_expr_str("true").unwrap().kind, ExprKind::BoolLit(true));
        assert_eq!(parse_expr_str("false").unwrap().kind, ExprKind::BoolLit(false));
    }

    #[test]
    fn expr_null_literal() {
        let e = parse_expr_str("null").unwrap();
        assert_eq!(e.kind, ExprKind::Null);
    }

    #[test]
    fn expr_path_single_ident() {
        let e = parse_expr_str("counter").unwrap();
        assert_eq!(e.kind, ExprKind::Path(vec!["counter".into()]));
    }

    #[test]
    fn expr_path_multi_segment() {
        let e = parse_expr_str("clifford::core::option").unwrap();
        assert_eq!(
            e.kind,
            ExprKind::Path(vec!["clifford".into(), "core".into(), "option".into()])
        );
    }

    #[test]
    fn expr_state_read() {
        let e = parse_expr_str("Counter@state").unwrap();
        assert_eq!(e.kind, ExprKind::StateRead("Counter".into()));
    }

    #[test]
    fn expr_paren_unwraps_single() {
        let e = parse_expr_str("(42)").unwrap();
        match e.kind {
            ExprKind::Paren(inner) => assert_eq!(inner.kind, ExprKind::IntLit("42".into())),
            other => panic!("expected Paren, got {:?}", other),
        }
    }

    #[test]
    fn expr_tuple_two() {
        let e = parse_expr_str("(1, 2)").unwrap();
        match e.kind {
            ExprKind::Tuple(elems) => assert_eq!(elems.len(), 2),
            other => panic!("expected Tuple, got {:?}", other),
        }
    }

    #[test]
    fn expr_array_literal() {
        let e = parse_expr_str("[1, 2, 3]").unwrap();
        match e.kind {
            ExprKind::Array(elems) => assert_eq!(elems.len(), 3),
            other => panic!("expected Array, got {:?}", other),
        }
    }

    #[test]
    fn expr_array_repeat() {
        let e = parse_expr_str("[0; 64]").unwrap();
        match e.kind {
            ExprKind::ArrayRepeat { value, count } => {
                assert_eq!(value.kind, ExprKind::IntLit("0".into()));
                assert_eq!(count.kind, ExprKind::IntLit("64".into()));
            }
            other => panic!("expected ArrayRepeat, got {:?}", other),
        }
    }

    // ── Postfix ──────────────────────────────────────────────────────────

    #[test]
    fn expr_field_access() {
        let e = parse_expr_str("foo.bar").unwrap();
        match e.kind {
            ExprKind::FieldAccess { obj, field } => {
                assert!(matches!(obj.kind, ExprKind::Path(_)));
                assert_eq!(field, "bar");
            }
            other => panic!("expected FieldAccess, got {:?}", other),
        }
    }

    #[test]
    fn expr_chained_field_access() {
        let e = parse_expr_str("a.b.c").unwrap();
        match e.kind {
            ExprKind::FieldAccess { obj, field } => {
                assert_eq!(field, "c");
                assert!(matches!(obj.kind, ExprKind::FieldAccess { .. }));
            }
            other => panic!("expected FieldAccess, got {:?}", other),
        }
    }

    #[test]
    fn expr_index() {
        let e = parse_expr_str("buf[i]").unwrap();
        match e.kind {
            ExprKind::Index { obj, index } => {
                assert!(matches!(obj.kind, ExprKind::Path(_)));
                assert!(matches!(index.kind, ExprKind::Path(_)));
            }
            other => panic!("expected Index, got {:?}", other),
        }
    }

    #[test]
    fn expr_call_no_args() {
        let e = parse_expr_str("foo()").unwrap();
        match e.kind {
            ExprKind::Call { callee, args } => {
                assert!(matches!(callee.kind, ExprKind::Path(_)));
                assert!(args.is_empty());
            }
            other => panic!("expected Call, got {:?}", other),
        }
    }

    #[test]
    fn expr_call_with_args() {
        let e = parse_expr_str("add(1, 2)").unwrap();
        match e.kind {
            ExprKind::Call { args, .. } => assert_eq!(args.len(), 2),
            other => panic!("expected Call, got {:?}", other),
        }
    }

    #[test]
    fn expr_method_call() {
        let e = parse_expr_str("obj.method(42)").unwrap();
        match e.kind {
            ExprKind::MethodCall { obj, method, args } => {
                assert!(matches!(obj.kind, ExprKind::Path(_)));
                assert_eq!(method, "method");
                assert_eq!(args.len(), 1);
            }
            other => panic!("expected MethodCall, got {:?}", other),
        }
    }

    // ── Unary / borrow ────────────────────────────────────────────────────

    #[test]
    fn expr_unary_neg() {
        let e = parse_expr_str("-5").unwrap();
        match e.kind {
            ExprKind::Unary { op, operand } => {
                assert_eq!(op, UnaryOp::Neg);
                assert_eq!(operand.kind, ExprKind::IntLit("5".into()));
            }
            other => panic!("expected Unary, got {:?}", other),
        }
    }

    #[test]
    fn expr_unary_not() {
        let e = parse_expr_str("!flag").unwrap();
        assert!(matches!(
            e.kind,
            ExprKind::Unary { op: UnaryOp::Not, .. }
        ));
    }

    #[test]
    fn expr_unary_bitnot() {
        let e = parse_expr_str("~mask").unwrap();
        assert!(matches!(
            e.kind,
            ExprKind::Unary { op: UnaryOp::BitNot, .. }
        ));
    }

    #[test]
    fn expr_unary_deref() {
        let e = parse_expr_str("*p").unwrap();
        assert!(matches!(
            e.kind,
            ExprKind::Unary { op: UnaryOp::Deref, .. }
        ));
    }

    #[test]
    fn expr_borrow_immutable() {
        let e = parse_expr_str("&x").unwrap();
        match e.kind {
            ExprKind::Ref { mutable, .. } => assert!(!mutable),
            other => panic!("expected Ref, got {:?}", other),
        }
    }

    #[test]
    fn expr_borrow_mutable() {
        let e = parse_expr_str("&mut x").unwrap();
        match e.kind {
            ExprKind::Ref { mutable, .. } => assert!(mutable),
            other => panic!("expected Ref, got {:?}", other),
        }
    }

    // ── Binary / precedence ──────────────────────────────────────────────

    #[test]
    fn expr_add() {
        let e = parse_expr_str("1 + 2").unwrap();
        match e.kind {
            ExprKind::Binary { op, .. } => assert_eq!(op, BinaryOp::Add),
            other => panic!("expected Binary, got {:?}", other),
        }
    }

    #[test]
    fn expr_precedence_mul_over_add() {
        // `1 + 2 * 3` should parse as `1 + (2 * 3)`.
        let e = parse_expr_str("1 + 2 * 3").unwrap();
        match e.kind {
            ExprKind::Binary { op: BinaryOp::Add, lhs, rhs } => {
                assert_eq!(lhs.kind, ExprKind::IntLit("1".into()));
                match rhs.kind {
                    ExprKind::Binary { op: BinaryOp::Mul, .. } => {}
                    other => panic!("expected Mul on rhs, got {:?}", other),
                }
            }
            other => panic!("expected top-level Add, got {:?}", other),
        }
    }

    #[test]
    fn expr_precedence_left_associative_add() {
        // `1 + 2 + 3` → `(1 + 2) + 3`.
        let e = parse_expr_str("1 + 2 + 3").unwrap();
        match e.kind {
            ExprKind::Binary { op: BinaryOp::Add, lhs, rhs } => {
                assert!(matches!(lhs.kind, ExprKind::Binary { op: BinaryOp::Add, .. }));
                assert_eq!(rhs.kind, ExprKind::IntLit("3".into()));
            }
            other => panic!("expected Add, got {:?}", other),
        }
    }

    #[test]
    fn expr_precedence_or_lower_than_and() {
        // `a || b && c` → `a || (b && c)`.
        let e = parse_expr_str("a || b && c").unwrap();
        match e.kind {
            ExprKind::Binary { op: BinaryOp::Or, rhs, .. } => {
                assert!(matches!(rhs.kind, ExprKind::Binary { op: BinaryOp::And, .. }));
            }
            other => panic!("expected Or, got {:?}", other),
        }
    }

    #[test]
    fn expr_precedence_compare_below_arith() {
        // `a + 1 < b * 2` → `(a + 1) < (b * 2)`.
        let e = parse_expr_str("a + 1 < b * 2").unwrap();
        match e.kind {
            ExprKind::Binary { op: BinaryOp::Lt, lhs, rhs } => {
                assert!(matches!(lhs.kind, ExprKind::Binary { op: BinaryOp::Add, .. }));
                assert!(matches!(rhs.kind, ExprKind::Binary { op: BinaryOp::Mul, .. }));
            }
            other => panic!("expected Lt, got {:?}", other),
        }
    }

    #[test]
    fn expr_bitwise_precedence() {
        // `&` binds tighter than `^`, which binds tighter than `|`.
        // `a | b ^ c & d` → `a | (b ^ (c & d))`.
        let e = parse_expr_str("a | b ^ c & d").unwrap();
        match e.kind {
            ExprKind::Binary { op: BinaryOp::BitOr, rhs, .. } => match rhs.kind {
                ExprKind::Binary { op: BinaryOp::BitXor, rhs: rhs2, .. } => {
                    assert!(matches!(
                        rhs2.kind,
                        ExprKind::Binary { op: BinaryOp::BitAnd, .. }
                    ));
                }
                other => panic!("expected BitXor under BitOr, got {:?}", other),
            },
            other => panic!("expected BitOr at top, got {:?}", other),
        }
    }

    #[test]
    fn expr_shift_precedence() {
        // `a + b << 2` → `(a + b) << 2`? No — `<<` is bp 15/16, `+` is 17/18,
        // so `+` binds tighter: `(a + b) << 2`.
        let e = parse_expr_str("a + b << 2").unwrap();
        match e.kind {
            ExprKind::Binary { op: BinaryOp::Shl, lhs, .. } => {
                assert!(matches!(lhs.kind, ExprKind::Binary { op: BinaryOp::Add, .. }));
            }
            other => panic!("expected Shl, got {:?}", other),
        }
    }

    #[test]
    fn expr_paren_overrides_precedence() {
        // `(1 + 2) * 3` — explicit grouping.
        let e = parse_expr_str("(1 + 2) * 3").unwrap();
        match e.kind {
            ExprKind::Binary { op: BinaryOp::Mul, lhs, .. } => {
                assert!(matches!(lhs.kind, ExprKind::Paren(_)));
            }
            other => panic!("expected Mul, got {:?}", other),
        }
    }

    #[test]
    fn expr_unary_then_binary() {
        // `-a + b` → `(-a) + b`. Unary binds tighter than binary.
        let e = parse_expr_str("-a + b").unwrap();
        match e.kind {
            ExprKind::Binary { op: BinaryOp::Add, lhs, .. } => {
                assert!(matches!(lhs.kind, ExprKind::Unary { op: UnaryOp::Neg, .. }));
            }
            other => panic!("expected Add, got {:?}", other),
        }
    }

    // ── Cast and range ───────────────────────────────────────────────────

    #[test]
    fn expr_cast() {
        let e = parse_expr_str("x as u32").unwrap();
        match e.kind {
            ExprKind::Cast { value, ty } => {
                assert!(matches!(value.kind, ExprKind::Path(_)));
                assert_eq!(ty.kind, TypeKind::Primitive(PrimitiveType::U32));
            }
            other => panic!("expected Cast, got {:?}", other),
        }
    }

    #[test]
    fn expr_range_half_open() {
        let e = parse_expr_str("0..n").unwrap();
        match e.kind {
            ExprKind::Range { inclusive, .. } => assert!(!inclusive),
            other => panic!("expected Range, got {:?}", other),
        }
    }

    #[test]
    fn expr_range_inclusive() {
        let e = parse_expr_str("0..=10").unwrap();
        match e.kind {
            ExprKind::Range { inclusive, .. } => assert!(inclusive),
            other => panic!("expected Range, got {:?}", other),
        }
    }

    // ── Narrow unsafe expressions (Decision #17) ─────────────────────────

    #[test]
    fn expr_unchecked_load() {
        let e = parse_expr_str("#unchecked_load<u32>(p)").unwrap();
        match e.kind {
            ExprKind::UncheckedLoad { ty, ptr } => {
                assert_eq!(ty.kind, TypeKind::Primitive(PrimitiveType::U32));
                assert!(matches!(ptr.kind, ExprKind::Path(_)));
            }
            other => panic!("expected UncheckedLoad, got {:?}", other),
        }
    }

    #[test]
    fn expr_volatile_load() {
        let e = parse_expr_str("#volatile_load<u8>(reg)").unwrap();
        assert!(matches!(e.kind, ExprKind::VolatileLoad { .. }));
    }

    #[test]
    fn expr_unchecked_cast_with_reason() {
        let e =
            parse_expr_str(r#"#unchecked_cast<u32, i32>("safe per ABI", x)"#).unwrap();
        match e.kind {
            ExprKind::UncheckedCast { reason, .. } => {
                assert_eq!(reason, "safe per ABI");
            }
            other => panic!("expected UncheckedCast, got {:?}", other),
        }
    }

    #[test]
    fn expr_unchecked_cast_rejects_empty_reason() {
        // Refinement #19a: reason string must be non-empty.
        let err = parse_expr_str(r#"#unchecked_cast<u32, i32>("", x)"#).expect_err("empty reason");
        assert!(matches!(
            err,
            ParseError::Expected {
                expected: "non-empty reason string for `#unchecked_cast` (Refinement #19a)",
                ..
            }
        ));
    }

    #[test]
    fn expr_unchecked_cast_rejects_whitespace_reason() {
        let err = parse_expr_str(r#"#unchecked_cast<u32, i32>("   ", x)"#)
            .expect_err("whitespace-only reason");
        assert!(matches!(err, ParseError::Expected { .. }));
    }

    #[test]
    fn expr_unchecked_offset() {
        let e = parse_expr_str("#unchecked_offset<u8>(p, 4)").unwrap();
        match e.kind {
            ExprKind::UncheckedOffset { ty, n, .. } => {
                assert_eq!(ty.kind, TypeKind::Primitive(PrimitiveType::U8));
                assert_eq!(n.kind, ExprKind::IntLit("4".into()));
            }
            other => panic!("expected UncheckedOffset, got {:?}", other),
        }
    }

    // ── Statements ───────────────────────────────────────────────────────

    #[test]
    fn stmt_let_explicit() {
        let s = parse_stmt_str("let x: u32 = 1;");
        match s.kind {
            StmtKind::Let { mutable, name, ty, value } => {
                assert!(!mutable);
                assert_eq!(name, "x");
                assert!(ty.is_some());
                assert_eq!(value.kind, ExprKind::IntLit("1".into()));
            }
            other => panic!("expected Let, got {:?}", other),
        }
    }

    #[test]
    fn stmt_let_mut_explicit() {
        let s = parse_stmt_str("let mut count: u32 = 0;");
        match s.kind {
            StmtKind::Let { mutable, .. } => assert!(mutable),
            other => panic!("expected Let, got {:?}", other),
        }
    }

    #[test]
    fn stmt_let_no_type_annotation() {
        let s = parse_stmt_str("let x = 1;");
        match s.kind {
            StmtKind::Let { ty, .. } => assert!(ty.is_none()),
            other => panic!("expected Let, got {:?}", other),
        }
    }

    #[test]
    fn stmt_let_short() {
        // Decision #8: `let x := expr;` short form.
        let s = parse_stmt_str("let n := 42;");
        match s.kind {
            StmtKind::LetShort { name, value } => {
                assert_eq!(name, "n");
                assert_eq!(value.kind, ExprKind::IntLit("42".into()));
            }
            other => panic!("expected LetShort, got {:?}", other),
        }
    }

    #[test]
    fn stmt_let_short_rejects_mut() {
        let wrapped = "@fn t() { let mut x := 1; }";
        let err = parse_str(wrapped).expect_err("`let mut x :=` should be rejected");
        assert!(matches!(err, ParseError::Expected { .. }));
    }

    #[test]
    fn stmt_return_with_value() {
        let s = parse_stmt_str("return 42;");
        match s.kind {
            StmtKind::Return(Some(e)) => {
                assert_eq!(e.kind, ExprKind::IntLit("42".into()));
            }
            other => panic!("expected Return(Some), got {:?}", other),
        }
    }

    #[test]
    fn stmt_return_no_value() {
        let s = parse_stmt_str("return;");
        assert!(matches!(s.kind, StmtKind::Return(None)));
    }

    #[test]
    fn stmt_expr() {
        let s = parse_stmt_str("foo();");
        match s.kind {
            StmtKind::Expr(e) => {
                assert!(matches!(e.kind, ExprKind::Call { .. }));
            }
            other => panic!("expected Expr, got {:?}", other),
        }
    }

    #[test]
    fn stmt_mutate_block() {
        let s = parse_stmt_str("#mutate Counter { value = 1, last = 0 };");
        match s.kind {
            StmtKind::Mutate { automaton, assigns } => {
                assert_eq!(automaton, "Counter");
                assert_eq!(assigns.len(), 2);
                let FieldAssign { field, index, value, .. } = &assigns[0];
                assert_eq!(field, "value");
                assert!(index.is_none());
                assert_eq!(value.kind, ExprKind::IntLit("1".into()));
            }
            other => panic!("expected Mutate, got {:?}", other),
        }
    }

    #[test]
    fn stmt_mutate_with_indexed_field() {
        let s = parse_stmt_str("#mutate Buf { buf[3] = 0xFF };");
        match s.kind {
            StmtKind::Mutate { assigns, .. } => {
                assert!(assigns[0].index.is_some());
            }
            other => panic!("expected Mutate, got {:?}", other),
        }
    }

    #[test]
    fn stmt_mutate_short_eq() {
        // Decision #15: `Auto.field = expr;`
        let s = parse_stmt_str("Counter.value = 5;");
        match s.kind {
            StmtKind::MutateShort { automaton, field, op, value } => {
                assert_eq!(automaton, "Counter");
                assert_eq!(field, "value");
                assert_eq!(op, AssignOp::Eq);
                assert_eq!(value.kind, ExprKind::IntLit("5".into()));
            }
            other => panic!("expected MutateShort, got {:?}", other),
        }
    }

    #[test]
    fn stmt_mutate_short_plus_eq() {
        let s = parse_stmt_str("Counter.value += 1;");
        match s.kind {
            StmtKind::MutateShort { op, .. } => assert_eq!(op, AssignOp::PlusEq),
            other => panic!("expected MutateShort, got {:?}", other),
        }
    }

    #[test]
    fn stmt_mutate_short_all_compound_ops() {
        let cases = [
            ("Counter.x -= 1;", AssignOp::MinusEq),
            ("Counter.x *= 2;", AssignOp::StarEq),
            ("Counter.x /= 2;", AssignOp::SlashEq),
            ("Counter.x %= 2;", AssignOp::PercentEq),
            ("Counter.x &= 0xFF;", AssignOp::AmpEq),
            ("Counter.x |= 1;", AssignOp::PipeEq),
            ("Counter.x ^= 1;", AssignOp::CaretEq),
            ("Counter.x <<= 1;", AssignOp::ShlEq),
            ("Counter.x >>= 1;", AssignOp::ShrEq),
        ];
        for (src, expected) in cases {
            let s = parse_stmt_str(src);
            match s.kind {
                StmtKind::MutateShort { op, .. } => {
                    assert_eq!(op, expected, "src: {src}");
                }
                other => panic!("expected MutateShort for {src:?}, got {:?}", other),
            }
        }
    }

    #[test]
    fn stmt_proc_call_no_args() {
        let s = parse_stmt_str("#> tick();");
        match s.kind {
            StmtKind::ProcCall { name, args } => {
                assert_eq!(name, "tick");
                assert!(args.is_empty());
            }
            other => panic!("expected ProcCall, got {:?}", other),
        }
    }

    #[test]
    fn stmt_proc_call_with_args() {
        let s = parse_stmt_str("#> log(1, 2);");
        match s.kind {
            StmtKind::ProcCall { name, args } => {
                assert_eq!(name, "log");
                assert_eq!(args.len(), 2);
            }
            other => panic!("expected ProcCall, got {:?}", other),
        }
    }

    #[test]
    fn stmt_unchecked_store() {
        let s = parse_stmt_str("#unchecked_store<u32>(p, 0xCAFE);");
        match s.kind {
            StmtKind::UncheckedStore { ty, .. } => {
                assert_eq!(ty.kind, TypeKind::Primitive(PrimitiveType::U32));
            }
            other => panic!("expected UncheckedStore, got {:?}", other),
        }
    }

    #[test]
    fn stmt_volatile_store() {
        let s = parse_stmt_str("#volatile_store<u8>(reg, 1);");
        assert!(matches!(s.kind, StmtKind::VolatileStore { .. }));
    }

    // ── Body / Block wiring ──────────────────────────────────────────────

    #[test]
    fn empty_body_block_has_no_stmts() {
        let body = parse_body_str("");
        assert!(body.stmts.is_empty());
    }

    #[test]
    fn body_with_multiple_stmts() {
        let body = parse_body_str(
            "let x: u32 = 1;\
             let y := 2;\
             #> tick();\
             return x;",
        );
        assert_eq!(body.stmts.len(), 4);
        assert!(matches!(body.stmts[0].kind, StmtKind::Let { .. }));
        assert!(matches!(body.stmts[1].kind, StmtKind::LetShort { .. }));
        assert!(matches!(body.stmts[2].kind, StmtKind::ProcCall { .. }));
        assert!(matches!(body.stmts[3].kind, StmtKind::Return(_)));
    }

    #[test]
    fn fn_body_round_trips_through_decl() {
        let p = parse_str("@fn add() -> u32 { let x := 1; return x; }").unwrap();
        match &p.items[0] {
            Item::Fn(decl) => {
                assert_eq!(decl.body.stmts.len(), 2);
            }
            other => panic!("expected Fn, got {:?}", other),
        }
    }

    #[test]
    fn effect_body_round_trips_through_decl() {
        let p = parse_str(
            "#effect tick() #mutates: [Counter] { Counter.value += 1; }",
        )
        .unwrap();
        match &p.items[0] {
            Item::Effect(decl) => {
                assert_eq!(decl.body.stmts.len(), 1);
                assert!(matches!(decl.body.stmts[0].kind, StmtKind::MutateShort { .. }));
            }
            other => panic!("expected Effect, got {:?}", other),
        }
    }

    #[test]
    fn interrupt_body_round_trips_through_decl() {
        let p = parse_str(
            "#interrupt UART_RX() #mutates: [Logger] #priority: HIGH { #> log(1); }",
        )
        .unwrap();
        match &p.items[0] {
            Item::Interrupt(decl) => {
                assert_eq!(decl.body.stmts.len(), 1);
                assert!(matches!(decl.body.stmts[0].kind, StmtKind::ProcCall { .. }));
            }
            other => panic!("expected Interrupt, got {:?}", other),
        }
    }

    // ── Realistic combined exercise ──────────────────────────────────────

    #[test]
    fn realistic_effect_body() {
        // A realistic `tick` effect: read state, compute, write back.
        let src = "\
            #effect tick() #mutates: [Counter] {\n  \
              let next: u32 = Counter.value + 1;\n  \
              Counter.value = next;\n  \
              return;\n\
            }";
        let p = parse_str(src).expect("parse realistic tick effect");
        match &p.items[0] {
            Item::Effect(decl) => {
                assert_eq!(decl.body.stmts.len(), 3);
            }
            other => panic!("expected Effect, got {:?}", other),
        }
    }

    #[test]
    fn body_span_covers_braces() {
        let src = "@fn t() { let x := 1; }";
        let p = parse_str(src).unwrap();
        match &p.items[0] {
            Item::Fn(decl) => {
                let open = src.find('{').unwrap();
                let close = src.rfind('}').unwrap() + 1;
                assert_eq!(decl.body.span.start, open);
                assert_eq!(decl.body.span.end, close);
            }
            other => panic!("expected Fn, got {:?}", other),
        }
    }

    #[test]
    fn parse_expression_rejects_trailing_junk() {
        // The public `parse_expression` entry point should error if tokens
        // remain after the expression.
        let err = parse_expr_str("1 + 2 garbage").expect_err("trailing junk");
        assert!(matches!(
            err,
            ParseError::Expected {
                expected: "EOF after expression",
                ..
            }
        ));
    }

    // ─── Slice 8: automaton members (Decisions #4, #5, #6 + Refinement #5b) ─

    use clifford_ast::{
        AccessMode, AddressClause, AutomatonField, BasisClause, StateName,
    };

    fn auto(p: &Program, idx: usize) -> &clifford_ast::AutomatonDecl {
        match &p.items[idx] {
            Item::Automaton(a) => a,
            other => panic!("expected Automaton at {idx}, got {:?}", other),
        }
    }

    #[test]
    fn empty_automaton_has_no_members() {
        let p = parse_str("#automaton Counter { }").unwrap();
        let a = auto(&p, 0);
        assert_eq!(a.name, "Counter");
        assert!(a.address.is_none());
        assert!(a.basis.is_none());
        assert!(a.states.is_none());
        assert!(a.fields.is_empty());
        assert!(a.transitions.is_empty());
    }

    #[test]
    fn automaton_with_single_field() {
        let p = parse_str("#automaton Counter { value: u32; }").unwrap();
        let a = auto(&p, 0);
        assert_eq!(a.fields.len(), 1);
        let AutomatonField { name, ty, offset, access, .. } = &a.fields[0];
        assert_eq!(name, "value");
        assert_eq!(ty.kind, TypeKind::Primitive(PrimitiveType::U32));
        assert!(offset.is_none());
        assert!(access.is_none());
    }

    #[test]
    fn automaton_with_multiple_fields() {
        let p = parse_str(
            "#automaton Counter { value: u32; last: u32; flags: u8; }",
        )
        .unwrap();
        let a = auto(&p, 0);
        let names: Vec<_> = a.fields.iter().map(|f| f.name.as_str()).collect();
        assert_eq!(names, vec!["value", "last", "flags"]);
    }

    // ── #address (Decision #6) ───────────────────────────────────────────

    #[test]
    fn automaton_with_address() {
        let p = parse_str("#automaton Usart1 { #address: 0x4000_0000; }").unwrap();
        let a = auto(&p, 0);
        assert!(a.address.is_some());
        let AddressClause { value, .. } = a.address.as_ref().unwrap();
        assert_eq!(value, "0x4000_0000");
    }

    #[test]
    fn address_requires_hex_literal() {
        let err = parse_str("#automaton X { #address: 1024; }")
            .expect_err("decimal literal should fail");
        assert!(matches!(
            err,
            ParseError::Expected {
                expected: "hex literal (e.g. `0x4000_0000`) as `#address` value",
                ..
            }
        ));
    }

    #[test]
    fn duplicate_address_rejected() {
        let err = parse_str(
            "#automaton X { #address: 0x4000_0000; #address: 0x5000_0000; }",
        )
        .expect_err("duplicate #address");
        assert!(matches!(
            err,
            ParseError::DuplicateClause { clause: "#address", .. }
        ));
    }

    // ── #basis (Decision #4) ─────────────────────────────────────────────

    #[test]
    fn automaton_with_basis() {
        let p = parse_str("#automaton Counter { #basis: counter_basis; }").unwrap();
        let a = auto(&p, 0);
        let BasisClause { name, .. } = a.basis.as_ref().unwrap();
        assert_eq!(name, "counter_basis");
    }

    #[test]
    fn duplicate_basis_rejected() {
        let err = parse_str("#automaton X { #basis: a; #basis: b; }")
            .expect_err("duplicate #basis");
        assert!(matches!(
            err,
            ParseError::DuplicateClause { clause: "#basis", .. }
        ));
    }

    // ── #states (Decision #5) ────────────────────────────────────────────

    #[test]
    fn automaton_with_states() {
        let p = parse_str(
            "#automaton Counter { #states: [Idle, Counting, Halted]; }",
        )
        .unwrap();
        let a = auto(&p, 0);
        let states = a.states.as_ref().unwrap();
        let names: Vec<_> = states.iter().map(|s| s.name.as_str()).collect();
        assert_eq!(names, vec!["Idle", "Counting", "Halted"]);
    }

    #[test]
    fn no_states_clause_means_monoid() {
        let p = parse_str("#automaton Counter { value: u32; }").unwrap();
        let a = auto(&p, 0);
        assert!(a.states.is_none(), "no #states clause ⇒ monoid (None)");
    }

    #[test]
    fn empty_states_list_rejected() {
        let err =
            parse_str("#automaton X { #states: []; }").expect_err("empty #states list");
        assert!(matches!(err, ParseError::EmptyStatesList { .. }));
    }

    #[test]
    fn duplicate_states_rejected() {
        let err = parse_str(
            "#automaton X { #states: [A]; #states: [B]; }",
        )
        .expect_err("duplicate #states");
        assert!(matches!(
            err,
            ParseError::DuplicateClause { clause: "#states", .. }
        ));
    }

    #[test]
    fn states_preserves_each_span() {
        let src = "#automaton X { #states: [A, B]; }";
        let p = parse_str(src).unwrap();
        let states = auto(&p, 0).states.as_ref().unwrap();
        let StateName { name: a_name, span: a_span } = &states[0];
        let StateName { name: b_name, span: b_span } = &states[1];
        assert_eq!(a_name, "A");
        assert_eq!(b_name, "B");
        // Each span must point at distinct, non-overlapping ranges.
        assert!(a_span.end <= b_span.start);
        assert_eq!(&src[a_span.start..a_span.end], "A");
        assert_eq!(&src[b_span.start..b_span.end], "B");
    }

    // ── Field metadata (Decision #6 register-block fields) ──────────────

    #[test]
    fn field_with_offset() {
        let p = parse_str("#automaton X { status: u32 #offset: 0x00; }").unwrap();
        let a = auto(&p, 0);
        assert_eq!(a.fields[0].offset.as_deref(), Some("0x00"));
        assert!(a.fields[0].access.is_none());
    }

    #[test]
    fn field_with_offset_and_access_read() {
        let p = parse_str("#automaton X { status: u32 #offset: 0x04 #access: read; }")
            .unwrap();
        let f = &auto(&p, 0).fields[0];
        assert_eq!(f.offset.as_deref(), Some("0x04"));
        assert_eq!(f.access, Some(AccessMode::Read));
    }

    #[test]
    fn field_access_modes_all_three() {
        for (src_mode, expected) in [
            ("read", AccessMode::Read),
            ("write", AccessMode::Write),
            ("read_write", AccessMode::ReadWrite),
        ] {
            let src = format!(
                "#automaton X {{ f: u32 #offset: 0x00 #access: {src_mode}; }}"
            );
            let p = parse_str(&src).unwrap_or_else(|e| panic!("{src}: {e}"));
            let f = &auto(&p, 0).fields[0];
            assert_eq!(f.access, Some(expected), "src: {src}");
        }
    }

    #[test]
    fn field_access_rejects_invalid_mode() {
        let err = parse_str("#automaton X { f: u32 #offset: 0x00 #access: maybe; }")
            .expect_err("invalid access mode");
        assert!(matches!(
            err,
            ParseError::Expected {
                expected: "`read`, `write`, or `read_write` as `#access` value",
                ..
            }
        ));
    }

    #[test]
    fn field_offset_requires_hex_literal() {
        let err = parse_str("#automaton X { f: u32 #offset: 4; }")
            .expect_err("decimal offset");
        assert!(matches!(
            err,
            ParseError::Expected {
                expected: "hex literal (e.g. `0x04`) as `#offset` value",
                ..
            }
        ));
    }

    #[test]
    fn field_meta_in_either_order() {
        // `#access` before `#offset` should also work.
        let p = parse_str(
            "#automaton X { f: u32 #access: read_write #offset: 0x10; }",
        )
        .unwrap();
        let f = &auto(&p, 0).fields[0];
        assert_eq!(f.offset.as_deref(), Some("0x10"));
        assert_eq!(f.access, Some(AccessMode::ReadWrite));
    }

    #[test]
    fn duplicate_field_offset_rejected() {
        let err = parse_str(
            "#automaton X { f: u32 #offset: 0x00 #offset: 0x04; }",
        )
        .expect_err("duplicate #offset");
        assert!(matches!(
            err,
            ParseError::DuplicateClause { clause: "#offset", .. }
        ));
    }

    #[test]
    fn duplicate_field_access_rejected() {
        let err = parse_str(
            "#automaton X { f: u32 #access: read #access: write; }",
        )
        .expect_err("duplicate #access");
        assert!(matches!(
            err,
            ParseError::DuplicateClause { clause: "#access", .. }
        ));
    }

    // ── #hidden (Decision #25) ───────────────────────────────────────────

    #[test]
    fn field_with_hidden_modifier() {
        // `#hidden` is a flag (no value). The simplest form: ordinary
        // field marked hidden.
        let p = parse_str("#automaton X { scratch: u32 #hidden; }").unwrap();
        let f = &auto(&p, 0).fields[0];
        assert!(f.hidden, "expected hidden = true");
        assert!(f.offset.is_none());
        assert!(f.access.is_none());
    }

    #[test]
    fn field_without_hidden_defaults_false() {
        // Sanity: ordinary fields default to `hidden = false`.
        let p = parse_str("#automaton X { value: u32; }").unwrap();
        assert!(!auto(&p, 0).fields[0].hidden);
    }

    #[test]
    fn field_hidden_with_offset_and_access_in_any_order() {
        // Decision #25 spec example: hidden + register-block field meta
        // intermixed in arbitrary order. Each variant parses the same way.
        let variants = [
            "#automaton X { status: u32 #hidden #offset: 0x04 #access: read; }",
            "#automaton X { status: u32 #offset: 0x04 #hidden #access: read; }",
            "#automaton X { status: u32 #offset: 0x04 #access: read #hidden; }",
            "#automaton X { status: u32 #access: read #hidden #offset: 0x04; }",
        ];
        for src in variants {
            let p = parse_str(src).unwrap_or_else(|e| panic!("{src}: {e}"));
            let f = &auto(&p, 0).fields[0];
            assert!(f.hidden, "src: {src}");
            assert_eq!(f.offset.as_deref(), Some("0x04"), "src: {src}");
            assert_eq!(f.access, Some(AccessMode::Read), "src: {src}");
        }
    }

    #[test]
    fn duplicate_field_hidden_rejected() {
        // `#hidden` is a flag — repeating it is a duplicate clause error,
        // matching the policy for `#offset` / `#access`.
        let err = parse_str("#automaton X { f: u32 #hidden #hidden; }")
            .expect_err("duplicate #hidden");
        assert!(matches!(
            err,
            ParseError::DuplicateClause { clause: "#hidden", .. }
        ));
    }

    #[test]
    fn multiple_fields_with_mixed_hidden() {
        // Multiple fields in one automaton; `#hidden` on a subset.
        let p = parse_str(
            "#automaton Counter { value: u32; scratch: u32 #hidden; cache: [u8; 4] #hidden; }",
        )
        .unwrap();
        let fields = &auto(&p, 0).fields;
        assert_eq!(fields.len(), 3);
        assert!(!fields[0].hidden, "value should not be hidden");
        assert!(fields[1].hidden, "scratch should be hidden");
        assert!(fields[2].hidden, "cache should be hidden");
    }

    // ── #transition (Refinement #5b) ─────────────────────────────────────

    #[test]
    fn transition_with_no_destination() {
        let p = parse_str(
            "#automaton Counter { #transition tick { Counter.value += 1; } }",
        )
        .unwrap();
        let t = &auto(&p, 0).transitions[0];
        assert_eq!(t.name, "tick");
        assert!(t.destination.is_none());
        assert_eq!(t.body.stmts.len(), 1);
    }

    #[test]
    fn transition_with_destination_state() {
        let p = parse_str(
            "#automaton Sm { \
             #states: [Idle, Running]; \
             #transition start -> Running { } \
             }",
        )
        .unwrap();
        let t = &auto(&p, 0).transitions[0];
        assert_eq!(t.name, "start");
        assert_eq!(t.destination.as_deref(), Some("Running"));
    }

    #[test]
    fn multiple_transitions_in_source_order() {
        let p = parse_str(
            "#automaton Sm { \
             #states: [A, B, C]; \
             #transition go_b -> B { } \
             #transition go_c -> C { } \
             #transition stay { } \
             }",
        )
        .unwrap();
        let names: Vec<_> = auto(&p, 0)
            .transitions
            .iter()
            .map(|t| t.name.as_str())
            .collect();
        assert_eq!(names, vec!["go_b", "go_c", "stay"]);
    }

    #[test]
    fn transition_body_can_have_complex_statements() {
        let p = parse_str(
            "#automaton Counter { \
             #transition advance { \
               let next: u32 = Counter.value + 1; \
               Counter.value = next; \
               #> log(next); \
             } \
             }",
        )
        .unwrap();
        let body = &auto(&p, 0).transitions[0].body;
        assert_eq!(body.stmts.len(), 3);
        assert!(matches!(body.stmts[0].kind, StmtKind::Let { .. }));
        assert!(matches!(body.stmts[1].kind, StmtKind::MutateShort { .. }));
        assert!(matches!(body.stmts[2].kind, StmtKind::ProcCall { .. }));
    }

    // ── Mixed-member ordering ────────────────────────────────────────────

    #[test]
    fn members_can_appear_in_any_order() {
        let p = parse_str(
            "#automaton Mixed { \
             value: u32; \
             #basis: mixed_basis; \
             #transition tick { } \
             #states: [A, B]; \
             flags: u8; \
             #address: 0x4000_0000; \
             #transition reset -> A { } \
             }",
        )
        .unwrap();
        let a = auto(&p, 0);
        assert!(a.address.is_some());
        assert!(a.basis.is_some());
        assert_eq!(a.states.as_ref().unwrap().len(), 2);
        assert_eq!(a.fields.len(), 2);
        assert_eq!(a.transitions.len(), 2);
    }

    #[test]
    fn unknown_member_is_clear_error() {
        // `@trait` lexes fine but is not a valid `#automaton` member.
        let err = parse_str("#automaton X { @trait Foo {} }")
            .expect_err("invalid member sigil");
        // The dispatcher's catch-all fires; check we got a useful diagnostic
        // pointing at the right thing.
        match err {
            ParseError::Expected { expected, .. } => {
                assert!(
                    expected.contains("#address")
                        && expected.contains("#transition")
                        && expected.contains("field declaration"),
                    "diagnostic should enumerate valid member kinds, got: {expected}"
                );
            }
            other => panic!("expected Expected error, got {:?}", other),
        }
    }

    // ── Realistic register-block automaton ───────────────────────────────

    #[test]
    fn realistic_register_block_automaton() {
        // The shape of a UART peripheral register block per Decision #6.
        let src = "\
            #automaton Usart1 {\n  \
              #address: 0x4000_0000;\n  \
              #basis: usart1_basis;\n  \
              status: u32 #offset: 0x00 #access: read;\n  \
              data:   u32 #offset: 0x04 #access: read_write;\n  \
              ctrl:   u32 #offset: 0x08 #access: write;\n\
            }";
        let p = parse_str(src).expect("parse register-block automaton");
        let a = auto(&p, 0);
        assert_eq!(a.name, "Usart1");
        assert!(a.address.is_some());
        assert!(a.basis.is_some());
        assert!(a.states.is_none(), "register block with no #states is monoid");
        assert_eq!(a.fields.len(), 3);
        assert!(a.transitions.is_empty(), "no transitions on a register block");

        let f1 = &a.fields[0];
        assert_eq!(f1.name, "status");
        assert_eq!(f1.offset.as_deref(), Some("0x00"));
        assert_eq!(f1.access, Some(AccessMode::Read));

        let f2 = &a.fields[1];
        assert_eq!(f2.access, Some(AccessMode::ReadWrite));

        let f3 = &a.fields[2];
        assert_eq!(f3.access, Some(AccessMode::Write));
    }

    // ── Realistic multi-state state machine ──────────────────────────────

    #[test]
    fn realistic_multistate_automaton() {
        let src = "\
            #automaton Counter {\n  \
              #basis: counter_basis;\n  \
              #states: [Idle, Counting, Halted];\n  \
              value: u32;\n  \
              last_irq: u32;\n  \
              #transition start -> Counting {\n    \
                Counter.value = 0;\n  \
              }\n  \
              #transition tick {\n    \
                Counter.value += 1;\n  \
              }\n  \
              #transition halt -> Halted { }\n\
            }";
        let p = parse_str(src).expect("parse multi-state automaton");
        let a = auto(&p, 0);
        assert!(a.address.is_none());
        assert!(a.basis.is_some());
        let state_names: Vec<_> = a.states.as_ref().unwrap()
            .iter().map(|s| s.name.as_str()).collect();
        assert_eq!(state_names, vec!["Idle", "Counting", "Halted"]);
        assert_eq!(a.fields.len(), 2);
        assert_eq!(a.transitions.len(), 3);

        let trans_names: Vec<_> =
            a.transitions.iter().map(|t| t.name.as_str()).collect();
        assert_eq!(trans_names, vec!["start", "tick", "halt"]);
        assert_eq!(a.transitions[0].destination.as_deref(), Some("Counting"));
        assert!(a.transitions[1].destination.is_none(), "tick stays in Counting");
        assert_eq!(a.transitions[2].destination.as_deref(), Some("Halted"));

        // The `start` transition body has one statement: Counter.value = 0;
        assert_eq!(a.transitions[0].body.stmts.len(), 1);
        assert!(matches!(
            a.transitions[0].body.stmts[0].kind,
            StmtKind::MutateShort { .. }
        ));
    }

    #[test]
    fn slice_7_real_program_blinky_with_bodies() {
        // A realistic — and now FULLY parseable — Clifford program.
        // Exercises every Phase-0 surface: automata, effects with bodies,
        // interrupts with bodies, traits, types, ADTs, generic params,
        // trait bounds, mutate sugar, narrow unsafe primitives, state reads,
        // proc-calls, returns, arithmetic, comparisons, casts.
        let src = "\
            @type LedState = | Off | On;\n\
            \n\
            @trait Tick {\n  \
              @fn tick(self) -> u32 $ [Pure];\n\
            }\n\
            \n\
            #automaton Counter { }\n\
            \n\
            #effect bump() #mutates: [Counter] {\n  \
              let next: u32 = Counter.value + 1;\n  \
              Counter.value = next;\n  \
              return;\n\
            }\n\
            \n\
            #effect read_status() -> u8 #mutates: [] {\n  \
              let raw := #volatile_load<u8>(status_reg);\n  \
              return raw & 0xFF;\n\
            }\n\
            \n\
            #interrupt USART1_IRQHandler() #mutates: [Counter] #priority: HIGH {\n  \
              #> bump();\n  \
              Counter.last_irq = Counter.value;\n\
            }\n\
            \n\
            #interface Serial {\n  \
              effect send_byte(b: u8);\n  \
              effect recv_byte() -> u8;\n\
            }\n\
            \n\
            #impl Serial for Counter { }\n\
            \n\
            @sequential(Counter, Counter);\n\
            \n\
            @fn cmd_is_help(buf: &[u8], min_len: usize) -> bool $ [Pure] {\n  \
              let len := min_len + 4;\n  \
              return buf[0] == b'h' && len > 0;\n\
            }\n\
            \n\
            #effect main() #mutates: [Counter] {\n  \
              let mut x: u32 = 0;\n  \
              #> bump();\n  \
              return;\n\
            }\n\
        ";
        let p = parse_str(src).expect("parse realistic blinky program");

        // 11 top-level items.
        assert_eq!(p.items.len(), 11);

        // `main` is `#effect` — an imperative entry per the layer rules.
        // (Per Decision #1 / Emergent Rule 4, `@fn main()` could not contain
        //  `let mut` or `#> proc()` calls — those are #-layer constructs.)
        match &p.items[10] {
            Item::Effect(decl) => {
                assert_eq!(decl.name, "main");
                assert_eq!(decl.mutates, vec!["Counter".to_string()]);
            }
            other => panic!("expected #effect main at index 10, got {:?}", other),
        }

        // Spot-check the effect with a real body.
        match &p.items[3] {
            Item::Effect(decl) => {
                assert_eq!(decl.name, "bump");
                assert_eq!(decl.body.stmts.len(), 3);
                assert!(matches!(decl.body.stmts[0].kind, StmtKind::Let { .. }));
                assert!(matches!(decl.body.stmts[1].kind, StmtKind::MutateShort { .. }));
                assert!(matches!(decl.body.stmts[2].kind, StmtKind::Return(None)));
            }
            other => panic!("expected #effect bump at index 3, got {:?}", other),
        }

        // The interrupt has a real body too.
        match &p.items[5] {
            Item::Interrupt(decl) => {
                assert_eq!(decl.name, "USART1_IRQHandler");
                assert_eq!(decl.body.stmts.len(), 2);
                assert!(matches!(decl.body.stmts[0].kind, StmtKind::ProcCall { .. }));
                assert!(matches!(decl.body.stmts[1].kind, StmtKind::MutateShort { .. }));
            }
            other => panic!("expected #interrupt at index 5, got {:?}", other),
        }

        // The pure helper got its trait list AND its body.
        match &p.items[9] {
            Item::Fn(decl) => {
                assert_eq!(decl.name, "cmd_is_help");
                assert_eq!(decl.trait_list.len(), 1);
                assert_eq!(decl.trait_list[0].name, "Pure");
                assert_eq!(decl.body.stmts.len(), 2);
            }
            other => panic!("expected @fn cmd_is_help at index 9, got {:?}", other),
        }
    }

    #[test]
    fn parser_slice_4_blinky_skeleton_with_typed_signatures() {
        // Updated blinky skeleton with realistic typed signatures.
        let src = "\
            #automaton Counter { }\n\
            #effect tick(b: u8) #mutates: [Counter] { }\n\
            #interrupt USART1_IRQHandler() #mutates: [Counter] #priority: HIGH { }\n\
            @fn cmd_is_help(buf: &[u8]) -> bool $ [Pure] { }\n\
            @fn main() { }\n\
        ";
        let p = parse_str(src).expect("parse typed-signature blinky skeleton");
        assert_eq!(p.items.len(), 5);

        // Spot-check: the @fn cmd_is_help has 1 param, return type, and trait list.
        match &p.items[3] {
            Item::Fn(FnDecl {
                name,
                params,
                return_type,
                trait_list,
                ..
            }) => {
                assert_eq!(name, "cmd_is_help");
                assert_eq!(params.len(), 1);
                assert_eq!(params[0].name, "buf");
                assert!(matches!(params[0].ty.kind, TypeKind::Ref(_)));
                assert_eq!(
                    return_type.as_ref().unwrap().kind,
                    TypeKind::Primitive(PrimitiveType::Bool)
                );
                assert_eq!(trait_list.len(), 1);
                assert_eq!(trait_list[0].name, "Pure");
            }
            other => panic!("expected cmd_is_help, got {:?}", other),
        }
    }

    // ─── Decision #22: $ [TraitList] on #effect / #interrupt / #transition ──

    #[test]
    fn effect_with_trait_list_single() {
        let p = parse_str(
            "#automaton Counter { value: u32; } \
             #effect tick() #mutates: [Counter] $ [Realtime] { }",
        )
        .expect("parse #effect $ [Realtime]");
        match &p.items[1] {
            Item::Effect(EffectDecl { trait_list, .. }) => {
                assert_eq!(trait_list.len(), 1);
                assert_eq!(trait_list[0].name, "Realtime");
            }
            other => panic!("expected Effect, got {:?}", other),
        }
    }

    #[test]
    fn effect_with_trait_list_multi() {
        let p = parse_str(
            "#automaton Mmio { ctl: u32; } \
             #effect setup() #mutates: [Mmio] $ [Hardware, Realtime, SeqCst] { }",
        )
        .expect("parse multi-trait effect");
        match &p.items[1] {
            Item::Effect(EffectDecl { trait_list, .. }) => {
                let names: Vec<_> = trait_list.iter().map(|t| t.name.as_str()).collect();
                assert_eq!(names, vec!["Hardware", "Realtime", "SeqCst"]);
            }
            other => panic!("expected Effect, got {:?}", other),
        }
    }

    #[test]
    fn effect_no_trait_list_keeps_empty() {
        let p = parse_str(
            "#automaton C { x: u32; } #effect bump() #mutates: [C] { }",
        )
        .expect("no $ clause");
        match &p.items[1] {
            Item::Effect(EffectDecl { trait_list, .. }) => assert!(trait_list.is_empty()),
            other => panic!("expected Effect, got {:?}", other),
        }
    }

    #[test]
    fn effect_with_cannot_mutate_then_trait_list() {
        // Trait list comes after #cannot_mutate (which is itself optional).
        let p = parse_str(
            "#automaton A { x: u32; } \
             #automaton B { y: u32; } \
             #effect e() #mutates: [A] #cannot_mutate: [B] $ [PureState] { }",
        )
        .expect("parse #cannot_mutate then $");
        match &p.items[2] {
            Item::Effect(EffectDecl { cannot_mutate, trait_list, .. }) => {
                assert_eq!(cannot_mutate, &vec!["B".to_owned()]);
                assert_eq!(trait_list.len(), 1);
                assert_eq!(trait_list[0].name, "PureState");
            }
            other => panic!("expected Effect, got {:?}", other),
        }
    }

    #[test]
    fn interrupt_with_trait_list() {
        let p = parse_str(
            "#automaton Tim { count: u32; } \
             #interrupt SysTick() #mutates: [Tim] #priority: HIGH $ [Hardware, Realtime] { }",
        )
        .expect("parse #interrupt with $");
        match &p.items[1] {
            Item::Interrupt(InterruptDecl { trait_list, .. }) => {
                let names: Vec<_> = trait_list.iter().map(|t| t.name.as_str()).collect();
                assert_eq!(names, vec!["Hardware", "Realtime"]);
            }
            other => panic!("expected Interrupt, got {:?}", other),
        }
    }

    #[test]
    fn interrupt_no_trait_list_keeps_empty() {
        let p = parse_str(
            "#automaton T { x: u32; } \
             #interrupt SysTick() #mutates: [T] #priority: HIGH { }",
        )
        .expect("interrupt without $");
        match &p.items[1] {
            Item::Interrupt(InterruptDecl { trait_list, .. }) => {
                assert!(trait_list.is_empty());
            }
            other => panic!("expected Interrupt, got {:?}", other),
        }
    }

    #[test]
    fn transition_with_trait_list() {
        let p = parse_str(
            "#automaton Counter { value: u32; \
                #transition tick $ [PureState] { Counter.value = 1u32; } \
              }",
        )
        .expect("parse transition with $");
        match &p.items[0] {
            Item::Automaton(AutomatonDecl { transitions, .. }) => {
                assert_eq!(transitions.len(), 1);
                assert_eq!(transitions[0].trait_list.len(), 1);
                assert_eq!(transitions[0].trait_list[0].name, "PureState");
            }
            other => panic!("expected Automaton, got {:?}", other),
        }
    }

    #[test]
    fn transition_with_destination_then_trait_list() {
        // Destination clause `-> Next` comes before `$ [...]`.
        let p = parse_str(
            "#automaton M { #states: [A, B]; \
                #transition step -> B $ [Realtime] { } \
              }",
        )
        .expect("parse `-> Dest` then $");
        match &p.items[0] {
            Item::Automaton(AutomatonDecl { transitions, .. }) => {
                assert_eq!(transitions[0].destination.as_deref(), Some("B"));
                assert_eq!(transitions[0].trait_list.len(), 1);
                assert_eq!(transitions[0].trait_list[0].name, "Realtime");
            }
            other => panic!("expected Automaton, got {:?}", other),
        }
    }

    #[test]
    fn transition_no_trait_list_keeps_empty() {
        let p = parse_str(
            "#automaton C { x: u32; #transition tick { } }",
        )
        .expect("transition without $");
        match &p.items[0] {
            Item::Automaton(AutomatonDecl { transitions, .. }) => {
                assert!(transitions[0].trait_list.is_empty());
            }
            other => panic!("expected Automaton, got {:?}", other),
        }
    }

    #[test]
    fn effect_with_generic_trait_in_list() {
        // Generic trait references work the same as on @fn.
        let p = parse_str(
            "#automaton Bus { f: u32; } \
             #effect tx() #mutates: [Bus] $ [LockingDiscipline<RwLock>] { }",
        )
        .expect("parse generic trait on effect");
        match &p.items[1] {
            Item::Effect(EffectDecl { trait_list, .. }) => {
                assert_eq!(trait_list.len(), 1);
                assert_eq!(trait_list[0].name, "LockingDiscipline");
                assert_eq!(trait_list[0].generic_args.len(), 1);
            }
            other => panic!("expected Effect, got {:?}", other),
        }
    }

    #[test]
    fn imperative_trait_names_pass_through_verbatim() {
        // The parser doesn't validate predeclared trait names — it's
        // syntactic only. Non-predeclared identifiers also parse cleanly;
        // semantic validation (which lives in clifford-types) reports
        // unknown traits there. This matches @fn's behaviour.
        let p = parse_str(
            "#automaton C { x: u32; } \
             #effect e() #mutates: [C] $ [MadeUpTrait, AnotherUserTrait] { }",
        )
        .expect("user-defined trait names parse");
        match &p.items[1] {
            Item::Effect(EffectDecl { trait_list, .. }) => {
                let names: Vec<_> = trait_list.iter().map(|t| t.name.as_str()).collect();
                assert_eq!(names, vec!["MadeUpTrait", "AnotherUserTrait"]);
            }
            other => panic!("expected Effect, got {:?}", other),
        }
    }
}
