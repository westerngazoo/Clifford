//! # clifford-check
//!
//! Post-type-check semantic verification for the Clifford compiler. Implements:
//!
//! - §5.4 Mutability checking: every assignment occurs inside a mutation
//!   context; every `mut` binding reachable only inside one; every `#mutate`
//!   targets an automaton in the surrounding `#mutates` set.
//! - §5.5 Sigil-layer boundary checking: `@fn` body contains no `#`-construct;
//!   `#`-context bodies may call `@fn` freely; cross-boundary upward inlining
//!   is forbidden (Emergent Rule 4); cross-boundary downward inlining is
//!   permitted (the standard optimisation path).
//! - §5.6 Trait-list verification: each `@fn` body honours every obligation
//!   in its declared `$ [TraitList]` (or the default `$ [Pure]`).
//! - §5.7 Reference provenance and body-scoped borrowing (Decision #13):
//!   the six-rule discipline (Rules 0–5) on references, including the
//!   field-provenance invalidation walk.
//! - §5.8 Sigma bounds tracking (Decision #14): per-loop refinement-typed
//!   bound on the iteration variable; bounds-check elision for direct
//!   slice/array accesses provable from the bound.
//!
//! ## Phase boundary
//!
//! Runs after `clifford-types`. Output is the verified typed AST consumed by
//! `clifford-effect` and downstream phases.
//!
//! ## Implementation status
//!
//! **Slice 1:** §5.5 sigil-layer boundary checking. Public entry
//! point [`check`] walks every `@fn` body and rejects any `#`-construct it
//! finds — `#mutate` / mutation sugar / `#> proc()` / narrow unsafe
//! primitives / automaton-field reads — with `E0101 ImperativeInFunctional`.
//! Cross-boundary calls (an `@fn` body calling a `#effect` or `#interrupt`
//! via regular call syntax) emit `E0102 CrossBoundaryCall`.
//!
//! **Slice 2 (this PR):** §5.4 mutation-authorisation checking. Walks
//! `#effect`, `#interrupt`, and `#transition` bodies (which Slice 1
//! deliberately skipped) and checks every `#mutate A { ... }` / `Auto.field
//! <op>= ...` mutation against the enclosing context's permitted-mutation
//! set. Two diagnostics:
//!
//! - `E0302 WriteToUndeclaredAutomaton` — the target automaton is not
//!   in the enclosing `#effect`'s / `#interrupt`'s `#mutates: [...]` list,
//!   nor is it the enclosing `#transition`'s owning automaton.
//! - `E0306 WriteToCannotMutate` — the target automaton appears in
//!   the enclosing `#effect`'s `#cannot_mutate: [...]` exclusion list.
//!
//! Slice 2 deliberately defers `E0301` (cross-boundary mutation through
//! a reference into shared state) to slice S3 — that check requires the
//! type checker to track which references are rooted in shared state,
//! which is post-T4b territory. Field-existence (`E0303`) is already
//! covered by the resolver's `E0405 UnknownField`.
//!
//! Trait-list verification (§5.6), reference provenance (§5.7), and
//! sigma bounds (§5.8) arrive in subsequent slices.

#![forbid(unsafe_code)]
#![warn(missing_docs)]

use clifford_ast::{
    AutomatonDecl, Block, EffectDecl, Expr, ExprKind, FnDecl, InterruptDecl, Item, Program, Stmt,
    StmtKind, TransitionDecl,
};
use clifford_resolve::{BindingRef, Resolution, Symbol, SymbolKind};
use thiserror::Error;

/// Errors produced during semantic checking.
///
/// Per the spec's error-code allocation:
/// - `E01xx`: sigil-boundary violations (§5.5) — owned by this slice.
/// - `E02xx`: trait-list obligations (§5.6).
/// - `E03xx`: mutability and mutation-context violations (§5.4).
/// - `E07xx`: reference provenance / body-scoped borrowing (§5.7).
/// - `E08xx`: sigma bounds tracking (§5.8).
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum CheckError {
    /// An `#`-layer construct appears inside an `@fn` body.
    ///
    /// Per Decision #1 / Emergent Rule 4, the functional layer cannot
    /// contain imperative constructs. The diagnostic carries a short name
    /// for the construct that violated the rule (e.g. `"#mutate"`,
    /// `"#> proc()"`, `"#unchecked_load"`, `"automaton-field read"`) and
    /// the byte offset of the offending construct.
    #[error("E0101: imperative construct `{construct}` in functional `@fn` body (at byte {at})")]
    ImperativeInFunctional {
        /// Short name of the construct that violated the rule.
        construct: &'static str,
        /// Byte offset of the offending construct.
        at: usize,
    },

    /// An `@fn` body contains a regular call expression whose callee
    /// resolves to a `#effect` or `#interrupt`.
    ///
    /// This is a special case of E0101 that gets its own code because the
    /// diagnostic shape is different: we name the callee and clarify that
    /// the cross-layer call direction is the violation, not the call form.
    #[error("E0102: cross-boundary call: `@fn` body cannot call `{callee_kind} {callee_name}` (at byte {at})")]
    CrossBoundaryCall {
        /// The callee's name.
        callee_name: String,
        /// The callee's kind, displayed for the user (e.g. `"#effect"`).
        callee_kind: &'static str,
        /// Byte offset of the call expression.
        at: usize,
    },

    /// A `#mutate A { ... }` statement (canonical form) or `Auto.field
    /// <op>= …` mutation-sugar statement (Decision #15) targets an
    /// automaton `A` that is not authorised in the enclosing mutation
    /// context.
    ///
    /// Authorisation rule (§5.4): `A` must be in the enclosing context's
    /// permitted-mutation set, which is:
    ///
    /// - For an `#effect` body: the names in its `#mutates: [...]` clause.
    /// - For an `#interrupt` body: the names in its `#mutates: [...]` clause.
    /// - For a `#transition` body of automaton `Owner`: the singleton
    ///   `[Owner]` (transitions implicitly mutate their own automaton only,
    ///   per Decision #5).
    /// - For an `#impl` method body of automaton `Impl`: the singleton
    ///   `[Impl]` (Decision #16's implicit `#mutates: [self]`). Not yet
    ///   implemented because `#impl` method bodies are post-Slice-7
    ///   parser work.
    ///
    /// The diagnostic carries the offending automaton, the enclosing
    /// callable's display name (e.g. `"#effect bump"`, `"#transition tick
    /// in #automaton Counter"`), and the byte offset of the statement.
    #[error("E0302: write to undeclared automaton `{automaton}`: not in `{enclosing}`'s `#mutates` set (at byte {at})")]
    WriteToUndeclaredAutomaton {
        /// Target automaton of the disallowed `#mutate` / sugar statement.
        automaton: String,
        /// Display name of the enclosing callable, e.g.
        /// `"#effect bump"` or `"#transition tick in #automaton Counter"`.
        enclosing: String,
        /// Byte offset of the offending mutation statement.
        at: usize,
    },

    /// A `#mutate A { ... }` statement (canonical form) or `Auto.field
    /// <op>= …` mutation-sugar statement (Decision #15) targets an
    /// automaton `A` that explicitly appears in the enclosing
    /// `#effect`'s `#cannot_mutate: [...]` exclusion list.
    ///
    /// `#cannot_mutate` is a *prohibition* on top of any implicit or
    /// explicit `#mutates`. If `A` is in both lists, `#cannot_mutate`
    /// wins and writes to `A` are rejected with this diagnostic. Per
    /// §2.5 the clause lists automaton names (not field names — the
    /// spec's earlier draft text wrongly said "field"; the grammar has
    /// always taken automaton names per Decision #3).
    #[error("E0306: write to forbidden automaton `{automaton}`: explicitly listed in `{enclosing}`'s `#cannot_mutate` clause (at byte {at})")]
    WriteToCannotMutate {
        /// Target automaton of the prohibited `#mutate` / sugar statement.
        automaton: String,
        /// Display name of the enclosing callable.
        enclosing: String,
        /// Byte offset of the offending mutation statement.
        at: usize,
    },
}

/// Run §5.5 sigil-layer boundary checking on a [`Program`] given its
/// [`Resolution`].
///
/// Walks every `@fn` body, rejecting any `#`-construct (statements:
/// `#mutate`, mutation sugar, `#> proc()`, narrow unsafe stores;
/// expressions: narrow unsafe loads/casts/offsets, `Auto@state` reads,
/// automaton-field reads). Walks `#`-layer body call expressions for the
/// cross-boundary-call check (`@fn` body calling a `#effect` resolves to
/// E0102). Errors accumulate; a single pass surfaces every violation.
///
/// # Errors
///
/// Returns `Err(Vec<CheckError>)` when any layer-boundary violation is
/// found; the vector is non-empty and ordered by source position. On
/// success returns `Ok(())`.
///
/// # Examples
///
/// ```
/// use clifford_lexer::tokenize;
/// use clifford_parser::parse;
/// use clifford_resolve::resolve;
/// use clifford_check::check;
///
/// // A clean program — `@fn` body has no `#`-construct.
/// let src = "@fn add(a: u32, b: u32) -> u32 { return a; }";
/// let tokens = tokenize(src).unwrap();
/// let program = parse(&tokens).unwrap();
/// let resolution = resolve(&program).unwrap();
/// assert!(check(&program, &resolution).is_ok());
/// ```
pub fn check(program: &Program, resolution: &Resolution) -> Result<(), Vec<CheckError>> {
    let mut walker = Walker {
        resolution,
        errors: Vec::new(),
    };
    for item in &program.items {
        match item {
            // §5.5 sigil-layer boundary check — Slice 1.
            Item::Fn(decl) => walker.walk_fn_decl(decl),
            // §5.4 mutation-authorisation check — Slice 2.
            Item::Effect(decl) => walker.walk_effect_decl(decl),
            Item::Interrupt(decl) => walker.walk_interrupt_decl(decl),
            Item::Automaton(decl) => walker.walk_automaton_decl(decl),
            // `#interface` and `#impl` method bodies arrive in parser
            // Slice 7+; check-slice S2 will pick them up when they exist.
            // `#test` bodies are mixed-layer per Decision #7 and need
            // their own check pass (not yet implemented).
            _ => {}
        }
    }
    if walker.errors.is_empty() {
        Ok(())
    } else {
        Err(walker.errors)
    }
}

// ─── Internal walker ────────────────────────────────────────────────────────

struct Walker<'a> {
    resolution: &'a Resolution,
    errors: Vec<CheckError>,
}

impl<'a> Walker<'a> {
    fn walk_fn_decl(&mut self, decl: &FnDecl) {
        self.walk_block(&decl.body);
    }

    fn walk_block(&mut self, block: &Block) {
        for stmt in &block.stmts {
            self.walk_stmt(stmt);
        }
    }

    // ─── S2: §5.4 mutation-authorisation walking ──────────────────────────

    /// Walk an `#effect` body checking every `#mutate` / sugar against
    /// the declared `#mutates` / `#cannot_mutate` clauses.
    fn walk_effect_decl(&mut self, decl: &EffectDecl) {
        let ctx = MutationContext {
            display_name: format!("#effect {}", decl.name),
            mutates: &decl.mutates,
            cannot_mutate: &decl.cannot_mutate,
        };
        self.walk_imperative_block(&decl.body, &ctx);
    }

    /// Walk an `#interrupt` body checking every `#mutate` / sugar against
    /// the declared `#mutates` clause. `InterruptDecl` does not yet carry
    /// a `cannot_mutate` field on the AST (the grammar permits it per
    /// §2.5 but the AST shape is current-shipping; treat as empty).
    fn walk_interrupt_decl(&mut self, decl: &InterruptDecl) {
        let no_cannot_mutate: Vec<String> = Vec::new();
        let ctx = MutationContext {
            display_name: format!("#interrupt {}", decl.name),
            mutates: &decl.mutates,
            cannot_mutate: &no_cannot_mutate,
        };
        self.walk_imperative_block(&decl.body, &ctx);
    }

    /// Walk every `#transition` body inside an `#automaton` declaration.
    /// Transitions implicitly mutate only their owning automaton (per
    /// Decision #5: state changes happen exclusively inside transition
    /// blocks of the owning automaton).
    fn walk_automaton_decl(&mut self, decl: &AutomatonDecl) {
        for tr in &decl.transitions {
            self.walk_transition_decl(&decl.name, tr);
        }
    }

    fn walk_transition_decl(&mut self, owner: &str, decl: &TransitionDecl) {
        // Transitions of automaton `Owner` get the singleton `[Owner]` as
        // their permitted-mutation set per §5.4 + Decision #5. They have
        // no `#cannot_mutate` analogue today.
        let mutates_self = vec![owner.to_owned()];
        let no_cannot_mutate: Vec<String> = Vec::new();
        let ctx = MutationContext {
            display_name: format!("#transition {} in #automaton {}", decl.name, owner),
            mutates: &mutates_self,
            cannot_mutate: &no_cannot_mutate,
        };
        self.walk_imperative_block(&decl.body, &ctx);
    }

    /// Walk a `#`-layer body in mutation-authorisation mode. Statement-form
    /// mutations (`#mutate A { ... }`, `Auto.field <op>= ...`) get
    /// authorisation-checked against the supplied [`MutationContext`].
    /// Non-mutation statements are recursed into for nested mutations
    /// (e.g. mutations inside an `if`/`while` body — once those land at
    /// the parser level).
    fn walk_imperative_block(&mut self, block: &Block, ctx: &MutationContext) {
        for stmt in &block.stmts {
            self.walk_imperative_stmt(stmt, ctx);
        }
    }

    fn walk_imperative_stmt(&mut self, stmt: &Stmt, ctx: &MutationContext) {
        match &stmt.kind {
            // ── Canonical mutate form ──
            StmtKind::Mutate { automaton, .. } => {
                self.check_mutation_target(automaton, stmt.span.start, ctx);
            }
            // ── Mutation-sugar form (Decision #15) ──
            StmtKind::MutateShort { automaton, .. } => {
                self.check_mutation_target(automaton, stmt.span.start, ctx);
            }
            // Other statement kinds are legitimate inside imperative
            // bodies — ProcCall, narrow unsafe stores, plain returns,
            // expression statements. We don't *re-flag* them (Slice 1
            // already rejects them in `@fn`); here they're allowed and
            // we just let any nested expressions flow through.
            //
            // Forward-compat: when the parser gains nested blocks
            // (`if`/`while` body statements) we'll recurse into them
            // via the same `walk_imperative_block` path.
            _ => {}
        }
    }

    /// Apply §5.4's two authorisation rules to a single mutation target.
    /// Order: `#cannot_mutate` (E0306) wins over `#mutates`-membership
    /// (E0302) — if the target is in both, report only the prohibition,
    /// since "you said this, then said don't do this" is the more
    /// specific user error to surface.
    fn check_mutation_target(&mut self, automaton: &str, at: usize, ctx: &MutationContext) {
        if ctx.cannot_mutate.iter().any(|a| a == automaton) {
            self.errors.push(CheckError::WriteToCannotMutate {
                automaton: automaton.to_owned(),
                enclosing: ctx.display_name.clone(),
                at,
            });
            return;
        }
        if !ctx.mutates.iter().any(|a| a == automaton) {
            self.errors.push(CheckError::WriteToUndeclaredAutomaton {
                automaton: automaton.to_owned(),
                enclosing: ctx.display_name.clone(),
                at,
            });
        }
    }

    /// Walk a statement in `@fn` context. Statement-form `#`-constructs
    /// produce E0101; any expressions encountered are walked for
    /// expression-form `#`-construct detection.
    fn walk_stmt(&mut self, stmt: &Stmt) {
        match &stmt.kind {
            StmtKind::Mutate { .. } => self.errors.push(CheckError::ImperativeInFunctional {
                construct: "#mutate",
                at: stmt.span.start,
            }),
            StmtKind::MutateShort { .. } => {
                self.errors.push(CheckError::ImperativeInFunctional {
                    construct: "Auto.field <op>= …",
                    at: stmt.span.start,
                });
            }
            StmtKind::ProcCall { args, .. } => {
                self.errors.push(CheckError::ImperativeInFunctional {
                    construct: "#> proc()",
                    at: stmt.span.start,
                });
                // Still walk arguments for expression-form constructs nested
                // inside (e.g. `#> log(#unchecked_load<u32>(p))`).
                for a in args {
                    self.walk_expr(a);
                }
            }
            StmtKind::UncheckedStore { ptr, value, .. } => {
                self.errors.push(CheckError::ImperativeInFunctional {
                    construct: "#unchecked_store",
                    at: stmt.span.start,
                });
                self.walk_expr(ptr);
                self.walk_expr(value);
            }
            StmtKind::VolatileStore { ptr, value, .. } => {
                self.errors.push(CheckError::ImperativeInFunctional {
                    construct: "#volatile_store",
                    at: stmt.span.start,
                });
                self.walk_expr(ptr);
                self.walk_expr(value);
            }
            StmtKind::Let { value, .. } | StmtKind::LetShort { value, .. } => {
                self.walk_expr(value);
            }
            StmtKind::Expr(e) => self.walk_expr(e),
            StmtKind::Return(Some(e)) => self.walk_expr(e),
            StmtKind::Return(None) => {}
            // Forward-compat for new statement kinds.
            _ => {}
        }
    }

    /// Walk an expression in `@fn` context. Expression-form `#`-constructs
    /// produce E0101; cross-boundary calls produce E0102.
    fn walk_expr(&mut self, expr: &Expr) {
        match &expr.kind {
            // ── Expression-form #-constructs ──
            ExprKind::UncheckedLoad { ptr, .. } => {
                self.errors.push(CheckError::ImperativeInFunctional {
                    construct: "#unchecked_load",
                    at: expr.span.start,
                });
                self.walk_expr(ptr);
            }
            ExprKind::VolatileLoad { ptr, .. } => {
                self.errors.push(CheckError::ImperativeInFunctional {
                    construct: "#volatile_load",
                    at: expr.span.start,
                });
                self.walk_expr(ptr);
            }
            ExprKind::UncheckedCast { value, .. } => {
                self.errors.push(CheckError::ImperativeInFunctional {
                    construct: "#unchecked_cast",
                    at: expr.span.start,
                });
                self.walk_expr(value);
            }
            ExprKind::UncheckedOffset { ptr, n, .. } => {
                self.errors.push(CheckError::ImperativeInFunctional {
                    construct: "#unchecked_offset",
                    at: expr.span.start,
                });
                self.walk_expr(ptr);
                self.walk_expr(n);
            }
            ExprKind::StateRead(_) => {
                self.errors.push(CheckError::ImperativeInFunctional {
                    construct: "Auto@state",
                    at: expr.span.start,
                });
            }

            // ── FieldAccess on an automaton receiver is a `#`-construct ──
            // The resolver already classified Auto.field accesses as
            // BindingRef::AutomatonField — we use that as the signal.
            ExprKind::FieldAccess { obj, .. } => {
                if matches!(
                    self.resolution.lookup(expr.span),
                    Some(BindingRef::AutomatonField { .. })
                ) {
                    self.errors.push(CheckError::ImperativeInFunctional {
                        construct: "automaton-field read",
                        at: expr.span.start,
                    });
                }
                // Still walk the receiver to find any nested #-constructs.
                self.walk_expr(obj);
            }

            // ── Path that resolves to an automaton symbol is a `#`-leak ──
            // (e.g. `let x := Counter;` in @fn — even without a field access,
            // the bare reference exposes imperative state.)
            ExprKind::Path(_) => {
                if matches!(
                    self.resolution.lookup(expr.span),
                    Some(BindingRef::TopLevel(Symbol {
                        kind: SymbolKind::Automaton,
                        ..
                    }))
                ) {
                    self.errors.push(CheckError::ImperativeInFunctional {
                        construct: "bare automaton reference",
                        at: expr.span.start,
                    });
                }
            }

            // ── Call expression: cross-boundary call check (E0102) ──
            ExprKind::Call { callee, args } => {
                self.walk_expr(callee);
                for a in args {
                    self.walk_expr(a);
                }
                // After walking the callee, check whether it resolved to a
                // top-level Effect or Interrupt. Either is a cross-boundary
                // call from `@fn`.
                if let Some(BindingRef::TopLevel(sym)) = self.resolution.lookup(callee.span) {
                    let kind = match sym.kind {
                        SymbolKind::Effect => Some("#effect"),
                        SymbolKind::Interrupt => Some("#interrupt"),
                        _ => None,
                    };
                    if let Some(callee_kind) = kind {
                        self.errors.push(CheckError::CrossBoundaryCall {
                            callee_name: sym.name.clone(),
                            callee_kind,
                            at: expr.span.start,
                        });
                    }
                }
            }

            // ── Compound forms: recurse ──
            ExprKind::Paren(inner) => self.walk_expr(inner),
            ExprKind::Tuple(elems) | ExprKind::Array(elems) => {
                for e in elems {
                    self.walk_expr(e);
                }
            }
            ExprKind::ArrayRepeat { value, count } => {
                self.walk_expr(value);
                self.walk_expr(count);
            }
            ExprKind::Index { obj, index } => {
                self.walk_expr(obj);
                self.walk_expr(index);
            }
            ExprKind::MethodCall { obj, args, .. } => {
                self.walk_expr(obj);
                for a in args {
                    self.walk_expr(a);
                }
            }
            ExprKind::Unary { operand, .. } | ExprKind::Ref { operand, .. } => {
                self.walk_expr(operand);
            }
            ExprKind::Binary { lhs, rhs, .. } => {
                self.walk_expr(lhs);
                self.walk_expr(rhs);
            }
            ExprKind::Cast { value, .. } => self.walk_expr(value),
            ExprKind::Range { lo, hi, .. } => {
                self.walk_expr(lo);
                self.walk_expr(hi);
            }

            // ── Atoms with no children, no `#`-form ──
            ExprKind::IntLit(_)
            | ExprKind::HexLit(_)
            | ExprKind::BinLit(_)
            | ExprKind::FloatLit(_)
            | ExprKind::CharLit(_)
            | ExprKind::ByteLit(_)
            | ExprKind::StringLit(_)
            | ExprKind::BoolLit(_)
            | ExprKind::Null => {}

            // Forward-compat for new expression kinds.
            _ => {}
        }
    }
}

/// Per-callable mutation-authorisation context used by Slice 2.
///
/// Constructed once at the top of each `#effect` / `#interrupt` /
/// `#transition` body and threaded through the body's statements.
/// Borrows the `mutates` and `cannot_mutate` lists from the AST node;
/// the walker doesn't own them.
struct MutationContext<'a> {
    /// Display name of the enclosing callable, e.g.
    /// `"#effect bump"`, `"#transition tick in #automaton Counter"`.
    /// Goes into the diagnostic verbatim so users see *their* identifier
    /// in the error message, not "the enclosing context."
    display_name: String,
    /// Automaton names this callable is permitted to mutate. For
    /// `#effect` / `#interrupt`: the `#mutates: [...]` clause. For
    /// `#transition` of `Owner`: the singleton `[Owner]`.
    mutates: &'a [String],
    /// Automaton names this callable is explicitly forbidden from
    /// mutating. For `#effect`: the `#cannot_mutate: [...]` clause.
    /// Empty for `#interrupt` (AST shape doesn't carry the field today)
    /// and for `#transition`.
    cannot_mutate: &'a [String],
}

#[cfg(test)]
mod tests {
    use super::*;
    use clifford_lexer::tokenize;
    use clifford_parser::parse;
    use clifford_resolve::resolve;

    fn check_str(src: &str) -> Result<(), Vec<CheckError>> {
        let tokens = tokenize(src).expect("tokenize");
        let program = parse(&tokens).expect("parse");
        let resolution = resolve(&program).expect("resolve");
        check(&program, &resolution)
    }

    // ── Empty / clean programs ───────────────────────────────────────────

    #[test]
    fn empty_program_is_clean() {
        check_str("").unwrap();
    }

    #[test]
    fn empty_fn_body_is_clean() {
        check_str("@fn nothing() { }").unwrap();
    }

    #[test]
    fn pure_arithmetic_in_fn_is_clean() {
        check_str("@fn add(a: u32, b: u32) -> u32 { let c: u32 = a + b; return c; }").unwrap();
    }

    #[test]
    fn fn_calling_another_fn_is_clean() {
        check_str(
            "@fn helper() -> u32 { return 1u32; } \
             @fn caller() -> u32 { return helper(); }",
        )
        .unwrap();
    }

    // ── #-layer items are not walked by §5.5 ─────────────────────────────

    #[test]
    fn effect_with_imperative_body_is_clean() {
        // `#effect` body legitimately contains `#mutate`, `Auto.field <op>=`,
        // `#> proc()`, etc. Slice 1 doesn't walk these — that's later slices.
        check_str(
            "#automaton Counter { value: u32; } \
             #effect bump() #mutates: [Counter] { Counter.value = 1u32; }",
        )
        .unwrap();
    }

    #[test]
    fn transition_body_with_state_changes_is_clean() {
        check_str(
            "#automaton Counter { value: u32; \
             #transition tick { Counter.value = Counter.value + 1u32; } }",
        )
        .unwrap();
    }

    // ── Statement-form #-constructs in @fn ───────────────────────────────

    #[test]
    fn mutate_in_fn_is_e0101() {
        let errors = check_str(
            "#automaton Counter { value: u32; } \
             @fn cheat() { #mutate Counter { value = 1u32 }; }",
        )
        .unwrap_err();
        assert!(errors.iter().any(|e| matches!(
            e,
            CheckError::ImperativeInFunctional { construct: "#mutate", .. }
        )));
    }

    #[test]
    fn mutate_short_in_fn_is_e0101() {
        let errors = check_str(
            "#automaton Counter { value: u32; } \
             @fn cheat() { Counter.value = 1u32; }",
        )
        .unwrap_err();
        assert!(errors.iter().any(|e| matches!(
            e,
            CheckError::ImperativeInFunctional { construct: "Auto.field <op>= …", .. }
        )));
    }

    #[test]
    fn proc_call_in_fn_is_e0101() {
        let errors = check_str(
            "#effect bump() #mutates: [] { } \
             @fn cheat() { #> bump(); }",
        )
        .unwrap_err();
        assert!(errors.iter().any(|e| matches!(
            e,
            CheckError::ImperativeInFunctional { construct: "#> proc()", .. }
        )));
    }

    #[test]
    fn unchecked_store_in_fn_is_e0101() {
        let errors = check_str(
            "@fn cheat(p: u32) { #unchecked_store<u32>(p, 0u32); }",
        )
        .unwrap_err();
        assert!(errors.iter().any(|e| matches!(
            e,
            CheckError::ImperativeInFunctional { construct: "#unchecked_store", .. }
        )));
    }

    #[test]
    fn volatile_store_in_fn_is_e0101() {
        let errors = check_str(
            "@fn cheat(p: u32) { #volatile_store<u8>(p, 0u8); }",
        )
        .unwrap_err();
        assert!(errors.iter().any(|e| matches!(
            e,
            CheckError::ImperativeInFunctional { construct: "#volatile_store", .. }
        )));
    }

    // ── Expression-form #-constructs in @fn ──────────────────────────────

    #[test]
    fn unchecked_load_in_fn_is_e0101() {
        let errors = check_str(
            "@fn cheat(p: u32) -> u8 { return #unchecked_load<u8>(p); }",
        )
        .unwrap_err();
        assert!(errors.iter().any(|e| matches!(
            e,
            CheckError::ImperativeInFunctional { construct: "#unchecked_load", .. }
        )));
    }

    #[test]
    fn volatile_load_in_fn_is_e0101() {
        let errors = check_str(
            "@fn cheat(p: u32) -> u8 { return #volatile_load<u8>(p); }",
        )
        .unwrap_err();
        assert!(errors.iter().any(|e| matches!(
            e,
            CheckError::ImperativeInFunctional { construct: "#volatile_load", .. }
        )));
    }

    #[test]
    fn unchecked_cast_in_fn_is_e0101() {
        let errors = check_str(
            r#"@fn cheat(x: u32) -> i32 { return #unchecked_cast<u32, i32>("safe", x); }"#,
        )
        .unwrap_err();
        assert!(errors.iter().any(|e| matches!(
            e,
            CheckError::ImperativeInFunctional { construct: "#unchecked_cast", .. }
        )));
    }

    #[test]
    fn unchecked_offset_in_fn_is_e0101() {
        let errors = check_str(
            "@fn cheat(p: u32) -> u32 { let _q := #unchecked_offset<u8>(p, 4i32); return p; }",
        )
        .unwrap_err();
        assert!(errors.iter().any(|e| matches!(
            e,
            CheckError::ImperativeInFunctional { construct: "#unchecked_offset", .. }
        )));
    }

    #[test]
    fn state_read_in_fn_is_e0101() {
        let errors = check_str(
            "#automaton Sm { #states: [A, B]; } \
             @fn peek() { let _s := Sm@state; }",
        )
        .unwrap_err();
        assert!(errors.iter().any(|e| matches!(
            e,
            CheckError::ImperativeInFunctional { construct: "Auto@state", .. }
        )));
    }

    #[test]
    fn automaton_field_read_in_fn_is_e0101() {
        let errors = check_str(
            "#automaton Counter { value: u32; } \
             @fn peek() -> u32 { return Counter.value; }",
        )
        .unwrap_err();
        assert!(errors.iter().any(|e| matches!(
            e,
            CheckError::ImperativeInFunctional { construct: "automaton-field read", .. }
        )));
    }

    #[test]
    fn bare_automaton_reference_in_fn_is_e0101() {
        let errors = check_str(
            "#automaton Counter { value: u32; } \
             @fn leak() { let _c := Counter; }",
        )
        .unwrap_err();
        assert!(errors.iter().any(|e| matches!(
            e,
            CheckError::ImperativeInFunctional { construct: "bare automaton reference", .. }
        )));
    }

    // ── Cross-boundary calls (E0102) ─────────────────────────────────────

    #[test]
    fn fn_calling_effect_via_call_is_e0102() {
        let errors = check_str(
            "#effect bump() #mutates: [] { } \
             @fn cheat() { bump(); }",
        )
        .unwrap_err();
        assert!(errors.iter().any(|e| matches!(
            e,
            CheckError::CrossBoundaryCall {
                callee_kind: "#effect",
                ref callee_name,
                ..
            } if callee_name == "bump"
        )));
    }

    #[test]
    fn fn_calling_interrupt_via_call_is_e0102() {
        let errors = check_str(
            "#interrupt UART_RX() #mutates: [] #priority: HIGH { } \
             @fn cheat() { UART_RX(); }",
        )
        .unwrap_err();
        assert!(errors.iter().any(|e| matches!(
            e,
            CheckError::CrossBoundaryCall { callee_kind: "#interrupt", .. }
        )));
    }

    #[test]
    fn fn_calling_fn_is_not_e0102() {
        // Function → function calls are downward-allowed and produce no
        // boundary error.
        check_str(
            "@fn helper() -> u32 { return 1u32; } \
             @fn caller() -> u32 { return helper(); }",
        )
        .unwrap();
    }

    // ── Nested constructs: errors collected in one pass ──────────────────

    #[test]
    fn multiple_violations_collected() {
        let errors = check_str(
            "#automaton Counter { value: u32; } \
             #effect bump() #mutates: [] { } \
             @fn cheats() { Counter.value = 1u32; bump(); let _v := Counter.value; }",
        )
        .unwrap_err();
        // We expect at least the MutateShort, the cross-boundary call, and
        // the field read all reported.
        assert!(errors.len() >= 3, "got {} errors: {:?}", errors.len(), errors);
        assert!(errors
            .iter()
            .any(|e| matches!(e, CheckError::ImperativeInFunctional { construct: "Auto.field <op>= …", .. })));
        assert!(errors
            .iter()
            .any(|e| matches!(e, CheckError::CrossBoundaryCall { .. })));
        assert!(errors.iter().any(|e| matches!(
            e,
            CheckError::ImperativeInFunctional { construct: "automaton-field read", .. }
        )));
    }

    // ── Nested expression: #-form inside a binary expression ─────────────

    #[test]
    fn nested_unchecked_load_in_arithmetic_is_e0101() {
        // `#unchecked_load<u32>(p) + 1u32` inside `@fn` — the load is a
        // `#`-construct even though it's nested in an arithmetic expression.
        let errors = check_str(
            "@fn cheat(p: u32) -> u32 { return #unchecked_load<u32>(p) + 1u32; }",
        )
        .unwrap_err();
        assert!(errors.iter().any(|e| matches!(
            e,
            CheckError::ImperativeInFunctional { construct: "#unchecked_load", .. }
        )));
    }

    // ── Realistic clean program ──────────────────────────────────────────

    #[test]
    fn realistic_clean_program() {
        let src = "\
            @fn cmd_check(min_len: u32) -> bool $ [Pure] {\n  \
              let len: u32 = min_len + 4u32;\n  \
              return len > 0u32;\n\
            }\n\
            #automaton Counter { value: u32; }\n\
            #effect bump() #mutates: [Counter] {\n  \
              Counter.value = Counter.value + 1u32;\n\
            }\n\
        ";
        check_str(src).unwrap();
    }

    // ── Local bindings don't trigger boundary errors ─────────────────────

    #[test]
    fn local_bindings_are_clean() {
        // Path expressions resolving to params or let-bindings (Local, not
        // TopLevel) must not trigger the bare-automaton-reference rule.
        check_str("@fn f(x: u32) -> u32 { let y: u32 = x; return y; }").unwrap();
    }

    // ─── Slice 2: §5.4 mutation-authorisation checking ───────────────────

    // ── #effect: declared automaton accepted ─────────────────────────────

    #[test]
    fn effect_mutates_declared_automaton_is_clean() {
        // `#effect bump #mutates: [Counter]` writing `Counter.value` is
        // authorised — Counter appears in the #mutates list.
        check_str(
            "#automaton Counter { value: u32; } \
             #effect bump() #mutates: [Counter] { Counter.value = 1u32; }",
        )
        .unwrap();
    }

    #[test]
    fn effect_mutates_one_of_multiple_declared_is_clean() {
        check_str(
            "#automaton A { x: u32; } \
             #automaton B { y: u32; } \
             #effect both() #mutates: [A, B] { A.x = 1u32; B.y = 2u32; }",
        )
        .unwrap();
    }

    #[test]
    fn effect_canonical_mutate_form_is_clean() {
        // The bulk-write `#mutate Counter { value = 1u32 };` form is also
        // authorised when Counter is declared in `#mutates`.
        check_str(
            "#automaton Counter { value: u32; } \
             #effect bump() #mutates: [Counter] { #mutate Counter { value = 1u32 }; }",
        )
        .unwrap();
    }

    // ── #effect: undeclared automaton rejected (E0302) ───────────────────

    #[test]
    fn effect_writes_undeclared_automaton_is_e0302() {
        // `#effect rogue` declares `#mutates: [A]` but writes to `B` —
        // E0302, since B is not authorised.
        let errors = check_str(
            "#automaton A { x: u32; } \
             #automaton B { y: u32; } \
             #effect rogue() #mutates: [A] { B.y = 1u32; }",
        )
        .unwrap_err();
        let saw = errors.iter().any(|e| matches!(
            e,
            CheckError::WriteToUndeclaredAutomaton { automaton, enclosing, .. }
                if automaton == "B" && enclosing == "#effect rogue"
        ));
        assert!(saw, "expected E0302 with automaton=B; got {errors:?}");
    }

    #[test]
    fn effect_with_empty_mutates_writes_anything_is_e0302() {
        // `#effect pure() #mutates: []` writing to *any* automaton is E0302.
        // The empty-list pure-effect case must be enforceable.
        let errors = check_str(
            "#automaton A { x: u32; } \
             #effect pure() #mutates: [] { A.x = 1u32; }",
        )
        .unwrap_err();
        let saw = errors.iter().any(|e| matches!(
            e,
            CheckError::WriteToUndeclaredAutomaton { automaton, .. } if automaton == "A"
        ));
        assert!(saw, "expected E0302 from #mutates: []; got {errors:?}");
    }

    #[test]
    fn effect_canonical_mutate_form_to_undeclared_is_e0302() {
        // E0302 fires for the canonical `#mutate B { ... }` form too,
        // not just sugar.
        let errors = check_str(
            "#automaton A { x: u32; } \
             #automaton B { y: u32; } \
             #effect rogue() #mutates: [A] { #mutate B { y = 1u32 }; }",
        )
        .unwrap_err();
        let saw = errors.iter().any(|e| matches!(
            e,
            CheckError::WriteToUndeclaredAutomaton { automaton, .. } if automaton == "B"
        ));
        assert!(saw, "expected E0302 for canonical #mutate B; got {errors:?}");
    }

    // ── #effect: cannot_mutate prohibition (E0306) ───────────────────────

    #[test]
    fn effect_cannot_mutate_excludes_explicit_target_is_e0306() {
        // `#effect bad() #mutates: [A] #cannot_mutate: [A]` is a self-
        // contradiction the compiler should flag — A is both permitted
        // and prohibited; the prohibition wins per §5.4.
        let errors = check_str(
            "#automaton A { x: u32; } \
             #effect bad() #mutates: [A] #cannot_mutate: [A] { A.x = 1u32; }",
        )
        .unwrap_err();
        let saw = errors.iter().any(|e| matches!(
            e,
            CheckError::WriteToCannotMutate { automaton, enclosing, .. }
                if automaton == "A" && enclosing == "#effect bad"
        ));
        assert!(saw, "expected E0306 with automaton=A; got {errors:?}");
    }

    #[test]
    fn effect_cannot_mutate_unrelated_target_is_silent() {
        // `#cannot_mutate: [B]` while writing to `A` is fine — B isn't
        // touched, so no prohibition violation. A is in #mutates so no
        // E0302 either.
        check_str(
            "#automaton A { x: u32; } \
             #automaton B { y: u32; } \
             #effect ok_only() #mutates: [A] #cannot_mutate: [B] { A.x = 1u32; }",
        )
        .unwrap();
    }

    #[test]
    fn effect_cannot_mutate_wins_over_e0302_priority() {
        // If a target is BOTH absent from #mutates AND in #cannot_mutate,
        // we report E0306 (the more specific user error) and not E0302.
        // The diagnostic surfaces the explicit prohibition rather than
        // the general undeclared-target rule.
        let errors = check_str(
            "#automaton A { x: u32; } \
             #automaton B { y: u32; } \
             #effect bad() #mutates: [A] #cannot_mutate: [B] { B.y = 1u32; }",
        )
        .unwrap_err();
        let saw_e0306 = errors.iter().any(|e| matches!(
            e,
            CheckError::WriteToCannotMutate { automaton, .. } if automaton == "B"
        ));
        let saw_e0302 = errors.iter().any(|e| matches!(
            e,
            CheckError::WriteToUndeclaredAutomaton { automaton, .. } if automaton == "B"
        ));
        assert!(saw_e0306, "expected E0306 when target is in cannot_mutate; got {errors:?}");
        assert!(!saw_e0302, "should NOT also emit E0302; got {errors:?}");
    }

    // ── #interrupt body authorisation ────────────────────────────────────

    #[test]
    fn interrupt_mutates_declared_automaton_is_clean() {
        check_str(
            "#automaton Counter { value: u32; } \
             #interrupt SysTick() #mutates: [Counter] #priority: HIGH { \
               Counter.value = Counter.value + 1u32; \
             }",
        )
        .unwrap();
    }

    #[test]
    fn interrupt_writes_undeclared_automaton_is_e0302() {
        let errors = check_str(
            "#automaton A { x: u32; } \
             #automaton B { y: u32; } \
             #interrupt SysTick() #mutates: [A] #priority: HIGH { B.y = 1u32; }",
        )
        .unwrap_err();
        let saw = errors.iter().any(|e| matches!(
            e,
            CheckError::WriteToUndeclaredAutomaton { automaton, enclosing, .. }
                if automaton == "B" && enclosing == "#interrupt SysTick"
        ));
        assert!(saw, "expected E0302 from #interrupt; got {errors:?}");
    }

    // ── #transition body authorisation ───────────────────────────────────

    #[test]
    fn transition_mutates_owning_automaton_is_clean() {
        // A `#transition` of `Counter` may write to `Counter` without an
        // explicit `#mutates` (Decision #5: implicit `[Owner]`).
        check_str(
            "#automaton Counter { value: u32; \
                #transition tick { Counter.value = Counter.value + 1u32; } \
              }",
        )
        .unwrap();
    }

    #[test]
    fn transition_writes_other_automaton_is_e0302() {
        // A `#transition` of `Counter` writing to `Logger` is E0302 — the
        // implicit permitted-mutation set is `[Counter]` only.
        let errors = check_str(
            "#automaton Counter { value: u32; \
                #transition tick { Logger.last = 1u32; } \
              } \
              #automaton Logger { last: u32; }",
        )
        .unwrap_err();
        let saw = errors.iter().any(|e| matches!(
            e,
            CheckError::WriteToUndeclaredAutomaton { automaton, enclosing, .. }
                if automaton == "Logger"
                && enclosing == "#transition tick in #automaton Counter"
        ));
        assert!(saw, "expected E0302 from cross-auto transition; got {errors:?}");
    }

    // ── Multiple errors collected ────────────────────────────────────────

    #[test]
    fn multiple_authorisation_errors_in_one_pass() {
        // One body, two violations: undeclared write to B + cannot-mutate
        // violation on C. Both should appear.
        let errors = check_str(
            "#automaton A { x: u32; } \
             #automaton B { y: u32; } \
             #automaton C { z: u32; } \
             #effect mixed() #mutates: [A] #cannot_mutate: [C] { \
               B.y = 1u32; \
               C.z = 2u32; \
             }",
        )
        .unwrap_err();
        let saw_b = errors.iter().any(|e| matches!(
            e,
            CheckError::WriteToUndeclaredAutomaton { automaton, .. } if automaton == "B"
        ));
        let saw_c = errors.iter().any(|e| matches!(
            e,
            CheckError::WriteToCannotMutate { automaton, .. } if automaton == "C"
        ));
        assert!(saw_b && saw_c, "expected both E0302/B and E0306/C; got {errors:?}");
    }

    // ── @fn boundary still enforced (no Slice-1 regression) ──────────────

    #[test]
    fn fn_boundary_check_still_runs_alongside_s2() {
        // Mixed program: an @fn with an imperative leak (E0101) AND an
        // effect with an unauthorised write (E0302). Both phases run;
        // both diagnostics surface.
        let errors = check_str(
            "#automaton Counter { value: u32; } \
             #automaton Other   { z: u32; } \
             @fn cheat() { Counter.value = 1u32; } \
             #effect rogue() #mutates: [Counter] { Other.z = 1u32; }",
        )
        .unwrap_err();
        let saw_e0101 = errors.iter().any(|e| matches!(
            e,
            CheckError::ImperativeInFunctional { .. }
        ));
        let saw_e0302 = errors.iter().any(|e| matches!(
            e,
            CheckError::WriteToUndeclaredAutomaton { automaton, .. } if automaton == "Other"
        ));
        assert!(saw_e0101 && saw_e0302, "expected both E0101 and E0302; got {errors:?}");
    }
}
