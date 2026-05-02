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
//! **Slice 1 (this PR):** §5.5 sigil-layer boundary checking. Public entry
//! point [`check`] walks every `@fn` body and rejects any `#`-construct it
//! finds — `#mutate` / mutation sugar / `#> proc()` / narrow unsafe
//! primitives / automaton-field reads — with `E0101 ImperativeInFunctional`.
//! Cross-boundary calls (an `@fn` body calling a `#effect` or `#interrupt`
//! via regular call syntax) emit `E0102 CrossBoundaryCall`. `#`-layer
//! bodies (`#effect`, `#interrupt`, `#transition`) may call `@fn`s freely
//! (downward call is permitted per Decision #1 / Emergent Rule 4); they
//! are not walked by this slice.
//!
//! Mutability checking (§5.4), trait-list verification (§5.6), reference
//! provenance (§5.7), and sigma bounds (§5.8) all arrive in subsequent
//! slices.

#![forbid(unsafe_code)]
#![warn(missing_docs)]

use clifford_ast::{Block, Expr, ExprKind, FnDecl, Item, Program, Stmt, StmtKind};
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
        if let Item::Fn(decl) = item {
            walker.walk_fn_decl(decl);
        }
        // `#`-layer items (`#effect`, `#interrupt`, `#automaton.transitions`)
        // are NOT walked by §5.5 — imperative constructs are legal there.
        // Subsequent check slices (§5.4 mutability, §5.6 trait-list, etc.)
        // will walk them with different rules.
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
}
