//! # clifford-resolve
//!
//! Name resolution for the Clifford compiler. Implements §5.1 step 1 of
//! `docs/CLIFFORD_SPEC.md`: bind every identifier to a definition, including
//! call-site context classification per Refinement #5b.
//!
//! ## Responsibilities (final scope)
//!
//! - Resolve every `path` and `ident` in the AST to its declaring item.
//! - Tag every `#> name(args)` call site with `CallContext` (Transition,
//!   Identity, or Generic) per Refinement #5b's generalisation:
//!   *transition-context ⟺ callee resolves to a `#transition`; identity-context
//!   ⟺ callee resolves to an `#effect`; generic-context for interface methods*.
//! - Resolve `<Auto>::<StateName>` state references and `<Auto>@state` reads
//!   per Refinement #5d.
//! - Resolve interface implementations and verify coherence (Decision #16).
//!
//! ## Phase boundary
//!
//! Resolution runs after parsing and before type checking. The output is a
//! `Resolution` value carrying the symbol tables and binding decisions; the
//! AST is left immutable. Downstream phases (`clifford-types`,
//! `clifford-check`, `clifford-effect`) consume both the AST and the
//! `Resolution` to do their work.
//!
//! ## Implementation status
//!
//! **Slice 1:** top-level [`SymbolTable`] — global namespace name → declaring
//! item; duplicate detection (E0401).
//!
//! **Slice 2 (this PR):** body name resolution. Public entry point
//! [`resolve`] walks every `@fn` / `#effect` / `#interrupt` body, building a
//! scope chain (parameters at the bottom; `let` and `let :=` bindings stacked
//! above), and resolves every single-segment `Path([X])` expression to a
//! [`BindingRef`] — either a top-level [`Symbol`] or a [`LocalBinding`].
//! `Auto@state` reads, `#mutate`, and `Auto.field <op>= …` mutation-sugar
//! statements verify their automaton-name component resolves to an
//! `#automaton` symbol (E0403). `#> proc(args)`, `#impl` method bodies,
//! `#transition` body walking with `Self` field-access, multi-segment path
//! semantics, and CallContext tagging per Refinement #5b are slice 3.

#![forbid(unsafe_code)]
#![warn(missing_docs)]

use std::collections::HashMap;

use clifford_ast::{Block, EffectDecl, Expr, ExprKind, FnDecl, InterruptDecl, Item, Layer, Param, Program, Stmt, StmtKind};
use clifford_lexer::Span;
use thiserror::Error;

/// Errors produced during name resolution.
///
/// Reserves the `E04xx` range per `docs/CLIFFORD_SPEC.md` §10. Resolver errors
/// are collected (not fail-fast) so that a single resolution pass surfaces
/// every duplicate / missing-name diagnostic the user has — the parser-style
/// "all errors at once" contract from `CLAUDE.md §6 Phase 0`.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum ResolveError {
    /// A top-level item name is declared more than once at the same scope.
    ///
    /// Both the original and the conflicting span are carried so diagnostics
    /// can render both. The originally-declared item wins for resolution
    /// purposes (the duplicate is suppressed); downstream phases see only the
    /// first occurrence in the [`SymbolTable`]. This matches rustc's
    /// "first-wins, duplicate is the error" convention.
    #[error("E0401: duplicate item `{name}` at byte {duplicate_at}; first declared at byte {original_at}")]
    DuplicateItem {
        /// The conflicting item name.
        name: String,
        /// Byte offset where the original (first) declaration began.
        original_at: usize,
        /// Byte offset where the duplicate declaration began.
        duplicate_at: usize,
    },

    /// A name appears in expression position but does not resolve to a local
    /// binding (parameter or `let`) nor to a top-level [`Symbol`].
    ///
    /// Diagnostics carry the unresolved name and the byte offset of the
    /// reference. Type-position name resolution (in `parse_type` outputs) is
    /// not handled by this slice and may produce different errors downstream.
    #[error("E0402: undefined name `{name}` at byte {at}")]
    UndefinedName {
        /// The unresolved identifier.
        name: String,
        /// Byte offset where the reference began.
        at: usize,
    },

    /// A statement that requires an `#automaton`-kind symbol references a
    /// name that resolves to a different kind (or doesn't resolve).
    ///
    /// Emitted for `#mutate Auto { … }`, `Auto.field <op>= …` mutation sugar,
    /// and `Auto@state` reads. Carries the kind that was actually found so
    /// the diagnostic can say "expected automaton, found function" instead of
    /// just "wrong kind."
    #[error("E0403: name `{name}` at byte {at} is not an `#automaton` (found {found})")]
    NotAnAutomaton {
        /// The misused name.
        name: String,
        /// Byte offset of the reference.
        at: usize,
        /// What kind the name actually resolves to (or `"undefined"` if
        /// it doesn't resolve at all).
        found: &'static str,
    },
}

/// Which kind of top-level item a [`Symbol`] refers to.
///
/// Distinct from `clifford_ast::Item` because some `Item` variants have no
/// resolvable name (`@sequential` is an attribute on a pair of names; `#impl`
/// is identified by `(interface_name, automaton_name)`, not a single ident).
/// Those variants do not produce [`Symbol`]s; see [`SymbolTable::build`] for
/// the inclusion list.
///
/// The kind discriminates how downstream phases should treat references to
/// the symbol — e.g. a `Path([X])` resolving to `SymbolKind::Automaton X`
/// is a candidate for the `X.field` / `X@state` postfix forms, while one
/// resolving to `SymbolKind::Fn X` is not.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum SymbolKind {
    /// `@fn name(...) { … }`
    Fn,
    /// `@type Name = …;`
    Type,
    /// `@trait Name { … }`
    Trait,
    /// `#automaton Name { … }`
    Automaton,
    /// `#effect name(...) #mutates: [...] { … }`
    Effect,
    /// `#interrupt NAME(...) #mutates: [...] #priority: ... { … }`
    Interrupt,
    /// `#interface Name { … }`
    Interface,
}

/// A resolved top-level symbol.
///
/// Holds enough information for downstream phases to (a) classify the symbol
/// by kind, (b) navigate back to the declaring AST node via `item_index`,
/// and (c) point users at the original declaration in diagnostics via `span`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Symbol {
    /// What kind of item this symbol refers to.
    pub kind: SymbolKind,
    /// Index into [`Program::items`] for the declaring item.
    /// Stable for the lifetime of the AST.
    pub item_index: usize,
    /// Sigil layer (functional / imperative) — derivable from `kind` but
    /// pre-computed here so consumers can branch on it without re-deriving.
    pub layer: Layer,
    /// Source span of the *declaring item*, end-to-end. Used by error
    /// messages that want to point at the original declaration ("…first
    /// declared here").
    pub span: Span,
}

/// The top-level namespace of a [`Program`] — every named item indexed by
/// its identifier.
///
/// Per Decision #1 there is currently a single global namespace shared by
/// `@`- and `#`-layer items. The module system (deferred) will introduce
/// nested namespaces; for now the spec's `clifford::core::option`-style paths
/// are *type-position only* and resolve through `crates/types`, not here.
///
/// `@sequential(A, B);` and `#impl Iface for Auto { … }` declarations do
/// **not** populate the symbol table — the former carries no name, the
/// latter is identified by the `(interface, automaton)` pair and resolved
/// in a later slice that handles interface coherence (Decision #16).
/// `#test "name" { … }` blocks also do not populate the table — tests are
/// independent compilation units identified by their description string,
/// not by an identifier.
///
/// # Lookup semantics
///
/// First-declaration wins. If the source contains two `@fn foo` declarations,
/// only the first appears in the table; the second produces
/// [`ResolveError::DuplicateItem`] and is suppressed from the namespace.
/// This matches rustc's behaviour and keeps the table representable even in
/// the presence of conflicting declarations.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SymbolTable {
    items: HashMap<String, Symbol>,
}

impl SymbolTable {
    /// Build a [`SymbolTable`] by walking the items of a [`Program`].
    ///
    /// Returns `Ok` with the populated table when every named item has a
    /// distinct identifier. Returns `Err` with the full list of
    /// [`ResolveError`]s when any duplicate-item conflicts are encountered;
    /// in that case the *partial* table (containing only the first
    /// occurrences) is discarded and the caller must rely on the error list
    /// alone. To inspect a partial table even on error, use
    /// [`SymbolTable::build_partial`].
    ///
    /// # Examples
    ///
    /// ```
    /// use clifford_lexer::tokenize;
    /// use clifford_parser::parse;
    /// use clifford_resolve::{SymbolKind, SymbolTable};
    ///
    /// let tokens = tokenize("@fn main() { } #automaton Counter { }").unwrap();
    /// let program = parse(&tokens).unwrap();
    /// let table = SymbolTable::build(&program).unwrap();
    ///
    /// assert_eq!(table.lookup("main").unwrap().kind, SymbolKind::Fn);
    /// assert_eq!(table.lookup("Counter").unwrap().kind, SymbolKind::Automaton);
    /// assert!(table.lookup("missing").is_none());
    /// ```
    ///
    /// # Errors
    ///
    /// Returns `Err(Vec<ResolveError>)` when duplicate item names are found.
    /// The error vector is non-empty and ordered by source position.
    pub fn build(program: &Program) -> Result<Self, Vec<ResolveError>> {
        let (table, errors) = Self::build_partial(program);
        if errors.is_empty() {
            Ok(table)
        } else {
            Err(errors)
        }
    }

    /// Like [`Self::build`] but always returns a (possibly partial)
    /// [`SymbolTable`] alongside any errors.
    ///
    /// The returned table contains exactly the *first* occurrence of every
    /// named item. Duplicates are absent from the table and present in the
    /// error vector. Useful for IDE-style consumers that want to keep
    /// resolving even past a duplicate-name error.
    #[must_use]
    pub fn build_partial(program: &Program) -> (Self, Vec<ResolveError>) {
        let mut items: HashMap<String, Symbol> = HashMap::new();
        let mut errors: Vec<ResolveError> = Vec::new();

        for (index, item) in program.items.iter().enumerate() {
            let Some((name, kind, span)) = symbol_for_item(item) else {
                continue;
            };
            match items.get(name) {
                Some(existing) => {
                    errors.push(ResolveError::DuplicateItem {
                        name: name.to_owned(),
                        original_at: existing.span.start,
                        duplicate_at: span.start,
                    });
                }
                None => {
                    items.insert(
                        name.to_owned(),
                        Symbol {
                            kind,
                            item_index: index,
                            layer: item.layer(),
                            span,
                        },
                    );
                }
            }
        }

        (Self { items }, errors)
    }

    /// Look up a symbol by its identifier. Returns `None` if no top-level
    /// item declared that name.
    #[must_use]
    pub fn lookup(&self, name: &str) -> Option<&Symbol> {
        self.items.get(name)
    }

    /// Iterate over every (name, symbol) pair. Order is unspecified
    /// (`HashMap` iteration order is non-deterministic).
    pub fn all(&self) -> impl Iterator<Item = (&String, &Symbol)> {
        self.items.iter()
    }

    /// Number of distinct top-level symbols recorded.
    #[must_use]
    pub fn len(&self) -> usize {
        self.items.len()
    }

    /// True if no top-level symbols were recorded.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.items.is_empty()
    }
}

// ─── Slice 2: body name resolution ──────────────────────────────────────────

/// What a name reference in expression position resolves to.
///
/// Produced by [`resolve`] for every single-segment `Path([X])` expression
/// found while walking `@fn` / `#effect` / `#interrupt` bodies. Multi-segment
/// paths receive a `BindingRef` for their first segment only in this slice;
/// full multi-segment semantics are slice 3 work.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BindingRef {
    /// Resolved to a top-level item via the [`SymbolTable`].
    TopLevel(Symbol),
    /// Resolved to a parameter or `let` binding in the enclosing block-scope chain.
    Local(LocalBinding),
}

/// A local binding produced by a parameter or `let` statement.
///
/// Carries enough information for downstream phases to reason about
/// mutability, type annotation presence, and source position without
/// re-walking the block.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LocalBinding {
    /// The bound name.
    pub name: String,
    /// What flavour of local this is (param vs let vs let-short).
    pub kind: LocalKind,
    /// Source span of the *binding site* — where the name was introduced.
    /// Used by diagnostics that point at "first defined here."
    pub def_span: Span,
}

/// Flavours of local binding.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum LocalKind {
    /// `mut? name: T` — function/effect/interrupt parameter.
    Param {
        /// `true` if declared `mut name: T`.
        mutable: bool,
    },
    /// `let mut? name (: T)? = expr;` — explicit `let`. `ty_annotated` is
    /// `true` iff the source carried a `: T` annotation (used by the type
    /// checker to distinguish "user-asserted type" from "fully inferred").
    Let {
        /// `true` for `let mut`.
        mutable: bool,
        /// `true` if the binding had an explicit type annotation.
        ty_annotated: bool,
    },
    /// `let name := expr;` — Decision #8 short binding. Always immutable,
    /// always inferred.
    LetShort,
}

/// The output of [`resolve`] — the top-level [`SymbolTable`] plus a per-AST
/// resolution map keyed by the byte-span of each resolved expression.
///
/// Indexed by `Span` because the AST does not carry node IDs; each
/// `Path` / `StateRead` / mutation-statement automaton-name reference has a
/// unique span (byte ranges are unique per source position), making `Span`
/// a sound key.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Resolution {
    /// Top-level namespace (from slice 1).
    pub symbols: SymbolTable,
    /// Map: span of the resolved AST node → what it resolves to.
    /// For `Expr::Path` and `Expr::StateRead` the span is the expression's
    /// own `Expr.span`. For `Stmt::Mutate` / `Stmt::MutateShort` the span
    /// is the statement's `Stmt.span` (the automaton-name resolution is
    /// implicit; consumers that want the automaton-name span specifically
    /// re-derive it from the AST).
    pub bindings: HashMap<Span, BindingRef>,
}

impl Resolution {
    /// Look up the resolution for a node by its source span.
    #[must_use]
    pub fn lookup(&self, span: Span) -> Option<&BindingRef> {
        self.bindings.get(&span)
    }

    /// Number of resolved references recorded.
    #[must_use]
    pub fn binding_count(&self) -> usize {
        self.bindings.len()
    }
}

/// Resolve a [`Program`] end-to-end: build the top-level [`SymbolTable`],
/// then walk every body and resolve every name reference.
///
/// This is the slice-2 entry point. Returns `Ok(Resolution)` when every name
/// resolves successfully; `Err(Vec<ResolveError>)` accumulates *all*
/// duplicate-item, undefined-name, and not-an-automaton errors in one pass.
///
/// # Errors
///
/// Returns the list of every [`ResolveError`] encountered. The list is
/// ordered by source position. The list is non-empty when returned; on
/// success `Ok(Resolution)` is returned with a fully-populated
/// [`Resolution::bindings`] map.
///
/// # Examples
///
/// ```
/// use clifford_lexer::tokenize;
/// use clifford_parser::parse;
/// use clifford_resolve::resolve;
///
/// let tokens = tokenize("@fn add(a: u32, b: u32) -> u32 { let c: u32 = a + b; return c; }").unwrap();
/// let program = parse(&tokens).unwrap();
/// let res = resolve(&program).expect("resolve");
/// assert_eq!(res.symbols.lookup("add").unwrap().kind, clifford_resolve::SymbolKind::Fn);
/// // `a`, `b`, `c` all resolved as Local bindings inside the body.
/// assert!(res.binding_count() >= 3);
/// ```
pub fn resolve(program: &Program) -> Result<Resolution, Vec<ResolveError>> {
    let (symbols, mut errors) = SymbolTable::build_partial(program);

    let mut walker = Walker {
        symbols: &symbols,
        bindings: HashMap::new(),
        errors: Vec::new(),
        scopes: Vec::new(),
    };

    for item in &program.items {
        match item {
            Item::Fn(decl) => walker.walk_fn_decl(decl),
            Item::Effect(decl) => walker.walk_effect_decl(decl),
            Item::Interrupt(decl) => walker.walk_interrupt_decl(decl),
            // Other items have no bodies that this slice walks. `#automaton`
            // fields and `#transition` bodies arrive in slice 3 alongside
            // `Self` and `Auto.field` resolution.
            _ => {}
        }
    }

    // Destructure the walker so the borrow on `symbols` ends here, freeing
    // it to be moved into `Resolution` below.
    let Walker {
        bindings,
        errors: walker_errors,
        ..
    } = walker;
    errors.extend(walker_errors);
    if errors.is_empty() {
        Ok(Resolution { symbols, bindings })
    } else {
        Err(errors)
    }
}

/// Internal walker state carried through one resolution pass.
struct Walker<'a> {
    symbols: &'a SymbolTable,
    bindings: HashMap<Span, BindingRef>,
    errors: Vec<ResolveError>,
    /// Stack of nested scopes. Innermost is `last()`. Lookup walks
    /// outward from `last()` to `first()`.
    scopes: Vec<Scope>,
}

/// One lexical scope frame. Holds the local bindings declared in this scope.
#[derive(Default)]
struct Scope {
    locals: HashMap<String, LocalBinding>,
}

impl<'a> Walker<'a> {
    fn push_scope(&mut self) {
        self.scopes.push(Scope::default());
    }

    fn pop_scope(&mut self) {
        let _ = self.scopes.pop();
    }

    /// Insert a local binding into the current (innermost) scope. If a
    /// binding with the same name already exists in this scope, it is
    /// shadowed (the new one wins for subsequent lookups). This matches
    /// Rust / OCaml / standard expression-language shadowing semantics.
    /// Cross-scope shadowing (a `let` in an inner block hides an outer
    /// binding for the duration of the inner block) is also supported via
    /// scope stacking.
    fn declare(&mut self, binding: LocalBinding) {
        if let Some(scope) = self.scopes.last_mut() {
            scope.locals.insert(binding.name.clone(), binding);
        }
    }

    /// Look up a name in the scope chain (innermost first). Returns the
    /// matching local binding if any.
    fn lookup_local(&self, name: &str) -> Option<&LocalBinding> {
        for scope in self.scopes.iter().rev() {
            if let Some(b) = scope.locals.get(name) {
                return Some(b);
            }
        }
        None
    }

    /// Resolve a single-segment name to a [`BindingRef`]. Locals shadow
    /// top-level symbols (a `let foo` inside a function hides the global
    /// `@fn foo` for the rest of the block).
    fn resolve_name(&mut self, name: &str, at: Span) {
        let resolved = if let Some(local) = self.lookup_local(name) {
            BindingRef::Local(local.clone())
        } else if let Some(symbol) = self.symbols.lookup(name) {
            BindingRef::TopLevel(symbol.clone())
        } else {
            self.errors.push(ResolveError::UndefinedName {
                name: name.to_owned(),
                at: at.start,
            });
            return;
        };
        self.bindings.insert(at, resolved);
    }

    /// Verify that a name resolves to an `#automaton` symbol. Used by
    /// `Mutate`, `MutateShort`, and `StateRead`. Records the resolution as
    /// a [`BindingRef::TopLevel`] under `at` on success; pushes
    /// [`ResolveError::NotAnAutomaton`] on failure.
    fn require_automaton(&mut self, name: &str, at: Span) {
        match self.symbols.lookup(name) {
            Some(sym) if matches!(sym.kind, SymbolKind::Automaton) => {
                self.bindings
                    .insert(at, BindingRef::TopLevel(sym.clone()));
            }
            Some(sym) => {
                self.errors.push(ResolveError::NotAnAutomaton {
                    name: name.to_owned(),
                    at: at.start,
                    found: kind_name(sym.kind),
                });
            }
            None => {
                self.errors.push(ResolveError::NotAnAutomaton {
                    name: name.to_owned(),
                    at: at.start,
                    found: "undefined",
                });
            }
        }
    }

    fn walk_fn_decl(&mut self, decl: &FnDecl) {
        self.push_scope();
        for param in &decl.params {
            self.declare_param(param);
        }
        self.walk_block(&decl.body);
        self.pop_scope();
    }

    fn walk_effect_decl(&mut self, decl: &EffectDecl) {
        self.push_scope();
        for param in &decl.params {
            self.declare_param(param);
        }
        self.walk_block(&decl.body);
        self.pop_scope();
    }

    fn walk_interrupt_decl(&mut self, decl: &InterruptDecl) {
        self.push_scope();
        for param in &decl.params {
            self.declare_param(param);
        }
        self.walk_block(&decl.body);
        self.pop_scope();
    }

    fn declare_param(&mut self, param: &Param) {
        self.declare(LocalBinding {
            name: param.name.clone(),
            kind: LocalKind::Param {
                mutable: param.mutable,
            },
            def_span: param.span,
        });
    }

    fn walk_block(&mut self, block: &Block) {
        self.push_scope();
        for stmt in &block.stmts {
            self.walk_stmt(stmt);
        }
        self.pop_scope();
    }

    fn walk_stmt(&mut self, stmt: &Stmt) {
        match &stmt.kind {
            StmtKind::Let {
                mutable,
                name,
                ty,
                value,
            } => {
                // Walk the initializer FIRST so `let x = x + 1` references
                // the outer `x`, not the new binding being introduced.
                self.walk_expr(value);
                self.declare(LocalBinding {
                    name: name.clone(),
                    kind: LocalKind::Let {
                        mutable: *mutable,
                        ty_annotated: ty.is_some(),
                    },
                    def_span: stmt.span,
                });
            }
            StmtKind::LetShort { name, value } => {
                self.walk_expr(value);
                self.declare(LocalBinding {
                    name: name.clone(),
                    kind: LocalKind::LetShort,
                    def_span: stmt.span,
                });
            }
            StmtKind::Expr(e) => self.walk_expr(e),
            StmtKind::Return(Some(e)) => self.walk_expr(e),
            StmtKind::Return(None) => {}
            StmtKind::Mutate { automaton, assigns } => {
                self.require_automaton(automaton, stmt.span);
                for fa in assigns {
                    if let Some(idx) = &fa.index {
                        self.walk_expr(idx);
                    }
                    self.walk_expr(&fa.value);
                }
            }
            StmtKind::MutateShort {
                automaton, value, ..
            } => {
                self.require_automaton(automaton, stmt.span);
                self.walk_expr(value);
            }
            StmtKind::ProcCall { args, .. } => {
                // Slice 3 will resolve the proc name and tag CallContext.
                // For now: walk arguments so any name references inside
                // them resolve.
                for a in args {
                    self.walk_expr(a);
                }
            }
            StmtKind::UncheckedStore { ptr, value, .. }
            | StmtKind::VolatileStore { ptr, value, .. } => {
                self.walk_expr(ptr);
                self.walk_expr(value);
            }
            // `Stmt` is `#[non_exhaustive]`. Forward-compat: new statement
            // kinds default to "no body work." Add an explicit arm when the
            // statement carries expressions or introduces bindings.
            _ => {}
        }
    }

    fn walk_expr(&mut self, expr: &Expr) {
        match &expr.kind {
            // ── Atoms with no children, no resolution ──
            ExprKind::IntLit(_)
            | ExprKind::HexLit(_)
            | ExprKind::BinLit(_)
            | ExprKind::FloatLit(_)
            | ExprKind::CharLit(_)
            | ExprKind::ByteLit(_)
            | ExprKind::StringLit(_)
            | ExprKind::BoolLit(_)
            | ExprKind::Null => {}

            // ── Path: resolve in scope chain or top-level ──
            ExprKind::Path(segments) => {
                if let Some(first) = segments.first() {
                    // Single-segment: full resolution.
                    // Multi-segment: resolve only the first segment for now.
                    self.resolve_name(first, expr.span);
                }
            }

            // ── StateRead: the automaton name must be an automaton ──
            ExprKind::StateRead(name) => {
                self.require_automaton(name, expr.span);
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
            ExprKind::FieldAccess { obj, .. } => self.walk_expr(obj),
            ExprKind::Index { obj, index } => {
                self.walk_expr(obj);
                self.walk_expr(index);
            }
            ExprKind::Call { callee, args } => {
                self.walk_expr(callee);
                for a in args {
                    self.walk_expr(a);
                }
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
            ExprKind::UncheckedLoad { ptr, .. } | ExprKind::VolatileLoad { ptr, .. } => {
                self.walk_expr(ptr);
            }
            ExprKind::UncheckedCast { value, .. } => self.walk_expr(value),
            ExprKind::UncheckedOffset { ptr, n, .. } => {
                self.walk_expr(ptr);
                self.walk_expr(n);
            }
            // `ExprKind` is `#[non_exhaustive]`. New variants default to no
            // recursion — add an explicit arm when introducing one.
            _ => {}
        }
    }
}

/// Human-readable name for a [`SymbolKind`] used in `NotAnAutomaton`
/// diagnostics.
fn kind_name(k: SymbolKind) -> &'static str {
    match k {
        SymbolKind::Fn => "function",
        SymbolKind::Type => "type",
        SymbolKind::Trait => "trait",
        SymbolKind::Automaton => "automaton",
        SymbolKind::Effect => "effect",
        SymbolKind::Interrupt => "interrupt",
        SymbolKind::Interface => "interface",
    }
}

/// Extract `(name, kind, span)` for items that contribute to the symbol
/// table. Returns `None` for nameless items (`@sequential`, `#impl`, `#test`).
fn symbol_for_item(item: &Item) -> Option<(&str, SymbolKind, Span)> {
    match item {
        Item::Fn(d) => Some((&d.name, SymbolKind::Fn, d.span)),
        Item::Type(d) => Some((&d.name, SymbolKind::Type, d.span)),
        Item::Trait(d) => Some((&d.name, SymbolKind::Trait, d.span)),
        Item::Automaton(d) => Some((&d.name, SymbolKind::Automaton, d.span)),
        Item::Effect(d) => Some((&d.name, SymbolKind::Effect, d.span)),
        Item::Interrupt(d) => Some((&d.name, SymbolKind::Interrupt, d.span)),
        Item::Interface(d) => Some((&d.name, SymbolKind::Interface, d.span)),
        Item::Impl(_) | Item::Test(_) | Item::Sequential(_) => None,
        // `Item` is `#[non_exhaustive]`. New variants default to "no
        // symbol" — name the variant explicitly when you add it so the
        // compiler tells you to revisit this dispatch.
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use clifford_lexer::tokenize;
    use clifford_parser::parse;

    fn build_str(src: &str) -> Result<SymbolTable, Vec<ResolveError>> {
        let tokens = tokenize(src).expect("tokenize");
        let program = parse(&tokens).expect("parse");
        SymbolTable::build(&program)
    }

    fn build_partial_str(src: &str) -> (SymbolTable, Vec<ResolveError>) {
        let tokens = tokenize(src).expect("tokenize");
        let program = parse(&tokens).expect("parse");
        SymbolTable::build_partial(&program)
    }

    // ── Empty / single-item baseline ─────────────────────────────────────

    #[test]
    fn empty_program_has_empty_table() {
        let table = build_str("").unwrap();
        assert!(table.is_empty());
        assert_eq!(table.len(), 0);
    }

    #[test]
    fn single_fn_declares_one_symbol() {
        let table = build_str("@fn main() { }").unwrap();
        assert_eq!(table.len(), 1);
        let main = table.lookup("main").unwrap();
        assert_eq!(main.kind, SymbolKind::Fn);
        assert_eq!(main.layer, Layer::Functional);
        assert_eq!(main.item_index, 0);
    }

    #[test]
    fn single_automaton_declares_one_symbol() {
        let table = build_str("#automaton Counter { }").unwrap();
        let counter = table.lookup("Counter").unwrap();
        assert_eq!(counter.kind, SymbolKind::Automaton);
        assert_eq!(counter.layer, Layer::Imperative);
    }

    #[test]
    fn missing_lookup_returns_none() {
        let table = build_str("@fn one() { }").unwrap();
        assert!(table.lookup("two").is_none());
    }

    // ── Every item kind that contributes a symbol ────────────────────────

    #[test]
    fn every_named_item_kind_lands_in_table() {
        let src = "\
            @fn fn_thing() { }\n\
            @type type_thing = u8;\n\
            @trait trait_thing { }\n\
            #automaton automaton_thing { }\n\
            #effect effect_thing() #mutates: [] { }\n\
            #interrupt interrupt_thing() #mutates: [] #priority: HIGH { }\n\
            #interface interface_thing { }\n\
        ";
        let table = build_str(src).unwrap();
        assert_eq!(table.len(), 7);
        for (name, kind) in [
            ("fn_thing", SymbolKind::Fn),
            ("type_thing", SymbolKind::Type),
            ("trait_thing", SymbolKind::Trait),
            ("automaton_thing", SymbolKind::Automaton),
            ("effect_thing", SymbolKind::Effect),
            ("interrupt_thing", SymbolKind::Interrupt),
            ("interface_thing", SymbolKind::Interface),
        ] {
            let s = table
                .lookup(name)
                .unwrap_or_else(|| panic!("missing symbol {name}"));
            assert_eq!(s.kind, kind, "kind mismatch for {name}");
        }
    }

    #[test]
    fn item_index_matches_program_order() {
        let src = "\
            @fn first() { }\n\
            #automaton Second { }\n\
            @type Third = u8;\n\
        ";
        let table = build_str(src).unwrap();
        assert_eq!(table.lookup("first").unwrap().item_index, 0);
        assert_eq!(table.lookup("Second").unwrap().item_index, 1);
        assert_eq!(table.lookup("Third").unwrap().item_index, 2);
    }

    #[test]
    fn layer_is_derived_from_item_kind() {
        let table = build_str(
            "@fn pure_thing() { } #automaton imperative_thing { }",
        )
        .unwrap();
        assert_eq!(table.lookup("pure_thing").unwrap().layer, Layer::Functional);
        assert_eq!(
            table.lookup("imperative_thing").unwrap().layer,
            Layer::Imperative
        );
    }

    // ── Items that DO NOT contribute to the table ────────────────────────

    #[test]
    fn impl_does_not_populate_table() {
        let table = build_str(
            "#interface Serial { } #automaton Counter { } #impl Serial for Counter { }",
        )
        .unwrap();
        // 2 named items: Serial and Counter. Impl is anonymous.
        assert_eq!(table.len(), 2);
        assert!(table.lookup("Serial").is_some());
        assert!(table.lookup("Counter").is_some());
    }

    #[test]
    fn test_block_does_not_populate_table() {
        let table = build_str(r#"#test "smoke" { } @fn helper() { }"#).unwrap();
        assert_eq!(table.len(), 1);
        assert!(table.lookup("helper").is_some());
        // The test description is NOT a symbol-table key.
        assert!(table.lookup("smoke").is_none());
    }

    #[test]
    fn sequential_attribute_does_not_populate_table() {
        let table = build_str(
            "#automaton A { } #automaton B { } @sequential(A, B);",
        )
        .unwrap();
        // A and B are symbols; the @sequential attribute itself is not.
        assert_eq!(table.len(), 2);
    }

    // ── Duplicate detection ──────────────────────────────────────────────

    #[test]
    fn duplicate_fn_is_e0401() {
        let errors = build_str("@fn foo() { } @fn foo() { }").unwrap_err();
        assert_eq!(errors.len(), 1);
        assert!(matches!(
            errors[0],
            ResolveError::DuplicateItem { ref name, .. } if name == "foo"
        ));
    }

    #[test]
    fn duplicate_across_kinds_collides() {
        // Single global namespace per Decision #1 — `foo` cannot be both
        // an `@fn` and an `#automaton`.
        let errors = build_str("@fn foo() { } #automaton foo { }").unwrap_err();
        assert_eq!(errors.len(), 1);
        assert!(matches!(
            errors[0],
            ResolveError::DuplicateItem { ref name, .. } if name == "foo"
        ));
    }

    #[test]
    fn three_way_duplicate_emits_two_errors() {
        // Three declarations of `dup`: errors for the 2nd and 3rd, not 1st.
        let errors = build_str(
            "@fn dup() { } @fn dup() { } @fn dup() { }",
        )
        .unwrap_err();
        assert_eq!(errors.len(), 2);
        for e in &errors {
            assert!(matches!(
                e,
                ResolveError::DuplicateItem { name, .. } if name == "dup"
            ));
        }
    }

    #[test]
    fn duplicate_carries_both_spans() {
        let src = "@fn dup() { } @fn dup() { }";
        let errors = build_str(src).unwrap_err();
        match &errors[0] {
            ResolveError::DuplicateItem {
                original_at,
                duplicate_at,
                ..
            } => {
                // First `@fn dup` starts at byte 0; second starts later.
                assert_eq!(*original_at, 0);
                assert!(*duplicate_at > *original_at);
                // The duplicate position should land on the second `@fn`.
                assert!(src[*duplicate_at..].starts_with("@fn dup"));
            }
            other => panic!("expected DuplicateItem, got {:?}", other),
        }
    }

    #[test]
    fn first_declaration_wins_in_partial_table() {
        // build_partial returns both the table and errors, never empty.
        let src = "@fn dup() { } @fn dup() { }";
        let (table, errors) = build_partial_str(src);
        assert_eq!(errors.len(), 1);
        // The table has the FIRST occurrence (item_index = 0), not the second.
        let dup = table.lookup("dup").expect("first dup wins");
        assert_eq!(dup.item_index, 0);
    }

    #[test]
    fn duplicate_does_not_block_other_resolutions() {
        // Even with one duplicate, every OTHER name still resolves.
        let (table, errors) = build_partial_str(
            "@fn dup() { } @fn dup() { } #automaton Counter { } @fn helper() { }",
        );
        assert_eq!(errors.len(), 1);
        assert!(table.lookup("dup").is_some());
        assert!(table.lookup("Counter").is_some());
        assert!(table.lookup("helper").is_some());
    }

    // ── Nameless items don't trigger duplicate detection on each other ──

    #[test]
    fn multiple_impls_do_not_collide() {
        // Two #impls don't have names, so neither populates the table nor
        // errors. (Interface coherence — "two impls of Serial for Usart1" —
        // is a separate Decision-#16 check, not symbol-table duplication.)
        let table = build_str(
            "#interface Serial { } #automaton A { } #automaton B { } \
             #impl Serial for A { } #impl Serial for B { }",
        )
        .unwrap();
        assert_eq!(table.len(), 3); // Serial, A, B
    }

    #[test]
    fn multiple_tests_do_not_collide() {
        let table =
            build_str(r#"#test "first" { } #test "second" { } #test "first" { }"#)
                .unwrap();
        assert!(table.is_empty());
    }

    #[test]
    fn multiple_sequential_attrs_do_not_collide() {
        let table = build_str(
            "#automaton A { } #automaton B { } #automaton C { } \
             @sequential(A, B); @sequential(A, C); @sequential(B, C);",
        )
        .unwrap();
        assert_eq!(table.len(), 3);
    }

    // ── Realistic program ────────────────────────────────────────────────

    #[test]
    fn realistic_program_resolves_top_level() {
        let src = "\
            @type LedState = | Off | On;\n\
            @trait Tick { }\n\
            #automaton Counter { value: u32; }\n\
            #effect bump() #mutates: [Counter] { }\n\
            #interrupt USART1_IRQHandler() #mutates: [Counter] #priority: HIGH { }\n\
            #interface Serial { }\n\
            #impl Serial for Counter { }\n\
            @sequential(Counter, Counter);\n\
            @fn cmd_is_help(buf: &[u8]) -> bool $ [Pure] { }\n\
            #effect main() #mutates: [Counter] { }\n\
        ";
        let table = build_str(src).unwrap();

        // 8 named items (10 items total minus #impl and @sequential).
        assert_eq!(table.len(), 8);

        for (name, kind, layer) in [
            ("LedState", SymbolKind::Type, Layer::Functional),
            ("Tick", SymbolKind::Trait, Layer::Functional),
            ("Counter", SymbolKind::Automaton, Layer::Imperative),
            ("bump", SymbolKind::Effect, Layer::Imperative),
            (
                "USART1_IRQHandler",
                SymbolKind::Interrupt,
                Layer::Imperative,
            ),
            ("Serial", SymbolKind::Interface, Layer::Imperative),
            ("cmd_is_help", SymbolKind::Fn, Layer::Functional),
            ("main", SymbolKind::Effect, Layer::Imperative),
        ] {
            let s = table
                .lookup(name)
                .unwrap_or_else(|| panic!("missing {name}"));
            assert_eq!(s.kind, kind, "{name} kind");
            assert_eq!(s.layer, layer, "{name} layer");
        }
    }

    // ── all() iterator ───────────────────────────────────────────────────

    #[test]
    fn all_returns_every_symbol() {
        let table = build_str("@fn a() { } @fn b() { } @fn c() { }").unwrap();
        let names: std::collections::HashSet<_> =
            table.all().map(|(n, _)| n.as_str()).collect();
        assert_eq!(names.len(), 3);
        assert!(names.contains("a"));
        assert!(names.contains("b"));
        assert!(names.contains("c"));
    }

    // ─── Slice 2: body name resolution ───────────────────────────────────

    fn resolve_str(src: &str) -> Result<Resolution, Vec<ResolveError>> {
        let tokens = tokenize(src).expect("tokenize");
        let program = parse(&tokens).expect("parse");
        resolve(&program)
    }

    /// Find the first resolution whose `name` matches and return it. Useful
    /// for tests that just want to check "did `x` resolve to a Local Param?"
    /// without caring about the exact AST node it came from.
    fn find_local_named<'a>(res: &'a Resolution, name: &str) -> Option<&'a LocalBinding> {
        res.bindings.values().find_map(|b| match b {
            BindingRef::Local(local) if local.name == name => Some(local),
            _ => None,
        })
    }

    /// Find the first resolution whose `name` matches a top-level symbol.
    fn find_top_level_named<'a>(res: &'a Resolution, name: &str) -> Option<&'a Symbol> {
        res.bindings.values().find_map(|b| match b {
            BindingRef::TopLevel(sym) => {
                // We need to check the source name — but Symbol doesn't carry
                // it (the SymbolTable does, keyed by name). Cross-reference
                // by looking up in the table.
                res.symbols
                    .lookup(name)
                    .filter(|s| s.span == sym.span)
                    .map(|_| sym)
            }
            _ => None,
        })
    }

    // ── Empty programs / trivial bodies ──────────────────────────────────

    #[test]
    fn empty_program_resolves() {
        let res = resolve_str("").unwrap();
        assert_eq!(res.binding_count(), 0);
    }

    #[test]
    fn empty_fn_body_has_no_bindings() {
        let res = resolve_str("@fn nothing() { }").unwrap();
        assert_eq!(res.binding_count(), 0);
    }

    // ── Local resolution via parameters ──────────────────────────────────

    #[test]
    fn param_resolves_to_local() {
        let res = resolve_str("@fn f(x: u32) -> u32 { return x; }").unwrap();
        let local = find_local_named(&res, "x").expect("x resolved");
        assert!(matches!(
            local.kind,
            LocalKind::Param { mutable: false }
        ));
    }

    #[test]
    fn mut_param_carries_mutability() {
        let res = resolve_str("@fn f(mut x: u32) -> u32 { return x; }").unwrap();
        let local = find_local_named(&res, "x").expect("x resolved");
        assert!(matches!(local.kind, LocalKind::Param { mutable: true }));
    }

    // ── Local resolution via let bindings ────────────────────────────────

    #[test]
    fn let_binding_resolves() {
        let res =
            resolve_str("@fn f() -> u32 { let x: u32 = 1; return x; }").unwrap();
        let local = find_local_named(&res, "x").expect("x resolved");
        assert!(matches!(
            local.kind,
            LocalKind::Let {
                mutable: false,
                ty_annotated: true,
            }
        ));
    }

    #[test]
    fn let_short_binding_resolves() {
        let res = resolve_str("@fn f() -> u32 { let x := 1; return x; }").unwrap();
        let local = find_local_named(&res, "x").expect("x resolved");
        assert!(matches!(local.kind, LocalKind::LetShort));
    }

    #[test]
    fn let_mut_carries_mutability() {
        let res = resolve_str(
            "#effect e() #mutates: [] { let mut x: u32 = 1; let _y := x; }",
        )
        .unwrap();
        let local = find_local_named(&res, "x").expect("x resolved");
        assert!(matches!(
            local.kind,
            LocalKind::Let {
                mutable: true,
                ty_annotated: true,
            }
        ));
    }

    #[test]
    fn let_value_sees_outer_x_not_new_x() {
        // `let x = x + 1;` — the RHS `x` MUST resolve to the outer `x`,
        // not the binding being introduced. (Standard let semantics.)
        let res = resolve_str(
            "@fn f(x: u32) -> u32 { let x: u32 = x + 1; return x; }",
        )
        .unwrap();
        // Find the `x` reference inside the `let` initializer (the `x + 1`).
        // The first `Path([x])` in the body's first stmt's value expr.
        let prog_tokens = tokenize("@fn f(x: u32) -> u32 { let x: u32 = x + 1; return x; }")
            .unwrap();
        let prog = parse(&prog_tokens).unwrap();
        let fn_decl = match &prog.items[0] {
            Item::Fn(d) => d,
            _ => panic!(),
        };
        // Statement 0: `let x: u32 = x + 1;` — value is `x + 1`, a Binary
        // whose LHS is the `x` reference we care about.
        let let_stmt = &fn_decl.body.stmts[0];
        let lhs_x_span = match &let_stmt.kind {
            StmtKind::Let { value, .. } => match &value.kind {
                ExprKind::Binary { lhs, .. } => lhs.span,
                _ => panic!("expected Binary"),
            },
            _ => panic!("expected Let"),
        };
        // Should resolve to the parameter (Param), not to the new Let.
        let b = res.lookup(lhs_x_span).expect("lhs x resolved");
        match b {
            BindingRef::Local(LocalBinding {
                kind: LocalKind::Param { .. },
                ..
            }) => {}
            other => panic!("expected param `x` to resolve, got {:?}", other),
        }
    }

    // ── Top-level fall-through ───────────────────────────────────────────

    #[test]
    fn top_level_fn_resolves_when_called_by_name() {
        // `@fn caller() { other; }` — the `other` reference resolves to
        // the top-level `@fn other`.
        let res = resolve_str("@fn other() { } @fn caller() { other; }").unwrap();
        let sym = find_top_level_named(&res, "other").expect("other resolved");
        assert_eq!(sym.kind, SymbolKind::Fn);
    }

    #[test]
    fn local_shadows_top_level() {
        // `@fn helper() { } @fn caller() { let helper := 1; helper; }`
        // The `helper` reference resolves to the local Let, not the top-level.
        let res = resolve_str(
            "@fn helper() { } @fn caller() { let helper := 1; let _x := helper; }",
        )
        .unwrap();
        // Find a LetShort binding for `_x` whose value is a Path resolving
        // to the local `helper`. The simplest check: iterate bindings.
        let mut found_local_helper = false;
        for b in res.bindings.values() {
            if let BindingRef::Local(LocalBinding {
                name,
                kind: LocalKind::LetShort,
                ..
            }) = b
            {
                if name == "helper" {
                    found_local_helper = true;
                }
            }
        }
        assert!(
            found_local_helper,
            "expected at least one Path to resolve to local `helper`"
        );
    }

    // ── Undefined names ──────────────────────────────────────────────────

    #[test]
    fn undefined_name_in_body_is_e0402() {
        let errors = resolve_str("@fn f() -> u32 { return mystery; }").unwrap_err();
        assert_eq!(errors.len(), 1);
        assert!(matches!(
            errors[0],
            ResolveError::UndefinedName { ref name, .. } if name == "mystery"
        ));
    }

    #[test]
    fn multiple_undefined_names_are_all_collected() {
        let errors = resolve_str(
            "@fn f() -> u32 { let x := alpha; let y := beta; return gamma; }",
        )
        .unwrap_err();
        let names: Vec<_> = errors
            .iter()
            .filter_map(|e| match e {
                ResolveError::UndefinedName { name, .. } => Some(name.as_str()),
                _ => None,
            })
            .collect();
        assert_eq!(names, vec!["alpha", "beta", "gamma"]);
    }

    // ── Automaton-name verification (Mutate / MutateShort / StateRead) ───

    #[test]
    fn mutate_resolves_automaton_name() {
        let res = resolve_str(
            "#automaton Counter { value: u32; } \
             #effect e() #mutates: [Counter] { #mutate Counter { value = 1 }; }",
        )
        .unwrap();
        // The Mutate stmt's span carries the resolution.
        let mut found = false;
        for b in res.bindings.values() {
            if let BindingRef::TopLevel(Symbol {
                kind: SymbolKind::Automaton,
                ..
            }) = b
            {
                found = true;
            }
        }
        assert!(found, "expected Counter to resolve as Automaton");
    }

    #[test]
    fn mutate_short_resolves_automaton_name() {
        let res = resolve_str(
            "#automaton Counter { value: u32; } \
             #effect e() #mutates: [Counter] { Counter.value = 1; }",
        )
        .unwrap();
        let any_automaton = res.bindings.values().any(|b| {
            matches!(
                b,
                BindingRef::TopLevel(Symbol {
                    kind: SymbolKind::Automaton,
                    ..
                })
            )
        });
        assert!(any_automaton);
    }

    #[test]
    fn mutate_unknown_automaton_is_e0403() {
        let errors = resolve_str(
            "#effect e() #mutates: [] { #mutate NotAThing { f = 1 }; }",
        )
        .unwrap_err();
        assert!(errors.iter().any(|e| matches!(
            e,
            ResolveError::NotAnAutomaton {
                name,
                found: "undefined",
                ..
            } if name == "NotAThing"
        )));
    }

    #[test]
    fn mutate_wrong_kind_is_e0403() {
        // `@fn Counter() { }` — the name is a function, but Mutate wants an automaton.
        let errors = resolve_str(
            "@fn Counter() { } #effect e() #mutates: [] { #mutate Counter { f = 1 }; }",
        )
        .unwrap_err();
        assert!(errors.iter().any(|e| matches!(
            e,
            ResolveError::NotAnAutomaton {
                name,
                found: "function",
                ..
            } if name == "Counter"
        )));
    }

    #[test]
    fn state_read_resolves_automaton() {
        let res = resolve_str(
            "#automaton Sm { #states: [A, B]; } \
             #effect peek() #mutates: [] { let s := Sm@state; }",
        )
        .unwrap();
        let any_automaton = res.bindings.values().any(|b| {
            matches!(
                b,
                BindingRef::TopLevel(Symbol {
                    kind: SymbolKind::Automaton,
                    ..
                })
            )
        });
        assert!(any_automaton);
    }

    #[test]
    fn state_read_on_non_automaton_is_e0403() {
        let errors = resolve_str(
            "@fn NotAuto() { } #effect peek() #mutates: [] { let s := NotAuto@state; }",
        )
        .unwrap_err();
        assert!(errors.iter().any(|e| matches!(
            e,
            ResolveError::NotAnAutomaton { found: "function", .. }
        )));
    }

    // ── Scope chain depth (block nesting) ────────────────────────────────

    #[test]
    fn nested_let_bindings_all_resolve() {
        // Multiple let bindings stacked in one block.
        let res = resolve_str(
            "@fn chain() -> u32 { \
             let a: u32 = 1; \
             let b: u32 = a; \
             let c: u32 = b; \
             return c; \
             }",
        )
        .unwrap();
        // Three references (b's RHS is `a`, c's RHS is `b`, return's `c`)
        // each resolve to a Local Let binding.
        let local_count = res
            .bindings
            .values()
            .filter(|b| matches!(b, BindingRef::Local(LocalBinding { kind: LocalKind::Let { .. }, .. })))
            .count();
        assert_eq!(local_count, 3);
    }

    // ── Recursion through expression structure ───────────────────────────

    #[test]
    fn names_inside_compound_expressions_resolve() {
        // Names buried inside Binary, Index, Call, etc. all resolve.
        let res = resolve_str(
            "@fn f(buf: &[u8], i: u32, j: u32) -> u8 { return buf[i + j]; }",
        )
        .unwrap();
        // Three references: `buf`, `i`, `j` — all are Param locals.
        let param_count = res
            .bindings
            .values()
            .filter(|b| {
                matches!(
                    b,
                    BindingRef::Local(LocalBinding {
                        kind: LocalKind::Param { .. },
                        ..
                    })
                )
            })
            .count();
        assert_eq!(param_count, 3);
    }

    #[test]
    fn names_in_array_repeat_resolve() {
        let res = resolve_str(
            "@fn f(n: u32) { let _x: u32 = n; let _arr := [0; n]; }",
        )
        .unwrap();
        // `n` referenced twice (let value, array-repeat count). Both resolve.
        let n_param_refs = res
            .bindings
            .values()
            .filter(|b| {
                matches!(
                    b,
                    BindingRef::Local(LocalBinding {
                        kind: LocalKind::Param { .. },
                        name,
                        ..
                    }) if name == "n"
                )
            })
            .count();
        assert_eq!(n_param_refs, 2);
    }

    #[test]
    fn proc_call_args_get_walked() {
        // `#> log(x);` — the `x` argument resolves even though slice 2
        // doesn't yet resolve `log` itself.
        let res = resolve_str(
            "#effect e(x: u32) #mutates: [] { #> log(x); }",
        )
        .unwrap();
        let x_refs = res
            .bindings
            .values()
            .filter(|b| {
                matches!(
                    b,
                    BindingRef::Local(LocalBinding {
                        kind: LocalKind::Param { .. },
                        name,
                        ..
                    }) if name == "x"
                )
            })
            .count();
        assert_eq!(x_refs, 1);
    }

    #[test]
    fn unsafe_load_pointer_resolves() {
        let res = resolve_str(
            "#effect e(p: u32) #mutates: [] { let _v := #volatile_load<u8>(p); }",
        )
        .unwrap();
        let p_refs = res
            .bindings
            .values()
            .filter(|b| {
                matches!(
                    b,
                    BindingRef::Local(LocalBinding {
                        kind: LocalKind::Param { .. },
                        name,
                        ..
                    }) if name == "p"
                )
            })
            .count();
        assert_eq!(p_refs, 1);
    }

    // ── Realistic program ────────────────────────────────────────────────

    #[test]
    fn realistic_program_resolves_end_to_end() {
        let src = "\
            #automaton Counter { value: u32; }\n\
            #effect bump() #mutates: [Counter] {\n  \
              let next: u32 = Counter.value + 1;\n  \
              Counter.value = next;\n  \
              return;\n\
            }\n\
            @fn cmd_is_help(buf: &[u8], min_len: usize) -> bool $ [Pure] {\n  \
              let len := min_len + 4;\n  \
              return buf[0] == b'h' && len > 0;\n\
            }\n\
        ";
        let res = resolve_str(src).expect("resolve");
        // Counter is an automaton symbol.
        assert_eq!(
            res.symbols.lookup("Counter").unwrap().kind,
            SymbolKind::Automaton
        );
        // `next` (used in MutateShort RHS) resolves.
        let next_refs = res
            .bindings
            .values()
            .filter(|b| {
                matches!(
                    b,
                    BindingRef::Local(LocalBinding { name, .. }) if name == "next"
                )
            })
            .count();
        assert_eq!(next_refs, 1);
        // `min_len` (used in let value) resolves once.
        let min_len_refs = res
            .bindings
            .values()
            .filter(|b| {
                matches!(
                    b,
                    BindingRef::Local(LocalBinding { name, .. }) if name == "min_len"
                )
            })
            .count();
        assert_eq!(min_len_refs, 1);
        // `len` (used in return RHS) and `buf` (used in return RHS) resolve.
        let len_refs = res
            .bindings
            .values()
            .filter(|b| {
                matches!(
                    b,
                    BindingRef::Local(LocalBinding { name, .. }) if name == "len"
                )
            })
            .count();
        assert_eq!(len_refs, 1);
    }

    #[test]
    fn duplicate_item_and_undefined_name_both_reported() {
        // Mix slice-1 and slice-2 errors in one program.
        let errors = resolve_str(
            "@fn dup() { } @fn dup() { } @fn caller() { use_undefined; }",
        )
        .unwrap_err();
        let dup_count = errors
            .iter()
            .filter(|e| matches!(e, ResolveError::DuplicateItem { .. }))
            .count();
        let undef_count = errors
            .iter()
            .filter(|e| matches!(e, ResolveError::UndefinedName { .. }))
            .count();
        assert_eq!(dup_count, 1);
        assert_eq!(undef_count, 1);
    }
}
