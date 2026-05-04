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
//! **Slice 2:** body name resolution. Public entry point [`resolve`] walks
//! every `@fn` / `#effect` / `#interrupt` body, building a scope chain
//! (parameters + `let` / `let :=` bindings), and resolves every
//! single-segment `Path([X])` expression to a [`BindingRef`]. Mutation-sugar
//! and `#mutate` automaton names resolve to `#automaton` symbols (E0403).
//!
//! **Slice 3 (this PR):** `#transition` body walking with implicit `Self`
//! binding, `Self.field` / `Auto.field` field-access validation against the
//! enclosing or named automaton's field set (E0405), `#mutate` / mutation-
//! sugar field-name validation, and `#> proc(args)` callee resolution with
//! [`CallContext`] tagging per Refinement #5b — Identity (top-level
//! `#effect`), Transition (named transition of an automaton in `#mutates`
//! scope), or unknown (E0404). Generic-context (interface-method) calls and
//! `#impl` method bodies remain deferred until parser slice 9+ produces the
//! AST for them.

#![forbid(unsafe_code)]
#![warn(missing_docs)]

use std::collections::{HashMap, HashSet};

use clifford_ast::{
    AutomatonDecl, Block, EffectDecl, Expr, ExprKind, FnDecl, InterruptDecl, Item, Layer, Param,
    Program, Stmt, StmtKind, TransitionDecl,
};
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

    /// A `#> name(args)` procedure call references a name that doesn't
    /// resolve to either a top-level `#effect` or a `#transition` of an
    /// automaton in the enclosing `#mutates` scope.
    ///
    /// The diagnostic intentionally lists both options the user has, since
    /// the most common cause is a typo in either an effect name or a
    /// transition name.
    #[error("E0404: unknown procedure `{name}` at byte {at} (must be a top-level `#effect` or a `#transition` of an automaton in `#mutates` scope)")]
    UnknownProc {
        /// The unresolved procedure name.
        name: String,
        /// Byte offset of the reference.
        at: usize,
    },

    /// A `Self.field` / `Auto.field` reference, or a `#mutate` field-assign,
    /// names a field that doesn't exist on the target automaton.
    ///
    /// `automaton` is the automaton's name; `field` is the bad field name;
    /// `at` is the byte offset of the reference. Diagnostics that want the
    /// list of valid fields fetch them from the AST via the automaton's
    /// item index.
    #[error("E0405: `{automaton}` has no field `{field}` (referenced at byte {at})")]
    UnknownField {
        /// Name of the automaton being indexed into.
        automaton: String,
        /// The bad field name.
        field: String,
        /// Byte offset of the reference.
        at: usize,
    },

    /// A reference outside the owning automaton's `#transition`s tried to
    /// access a `#hidden` field (Decision #25). The field exists on the
    /// automaton but is encapsulated: only the automaton's own
    /// transitions may name it. From everywhere else (other automata's
    /// transitions, `#effect` / `#interrupt` bodies whose `#mutates` lists
    /// this automaton, `@fn` bodies) the field is invisible — its basis
    /// vector cannot enter outside callables' `actual_writes`, which is
    /// the *algebraic-trivial-orthogonality* property §3.7 promises.
    ///
    /// The diagnostic carries the owning automaton's name, the field name
    /// (so the user sees their identifier), and the reference's byte
    /// offset. Distinct from `UnknownField` (E0405): the field is real,
    /// just not visible from here.
    #[error("E0407: `{automaton}.{field}` is `#hidden`; only `{automaton}`'s own `#transition`s may access it (referenced at byte {at})")]
    HiddenFieldNotAccessible {
        /// Owning automaton name.
        automaton: String,
        /// Field name (the one marked `#hidden`).
        field: String,
        /// Byte offset of the reference.
        at: usize,
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
/// by kind, (b) recover the original identifier without consulting the
/// [`SymbolTable`] index, (c) navigate back to the declaring AST node via
/// `item_index`, and (d) point users at the original declaration in
/// diagnostics via `span`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Symbol {
    /// The declaring item's identifier — same as the [`SymbolTable`] key.
    /// Carried on the symbol so downstream consumers that hold a `Symbol`
    /// (e.g. inside a [`BindingRef`]) can produce diagnostics or look up
    /// side tables without reverse-iterating the [`SymbolTable`].
    pub name: String,
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
                            name: name.to_owned(),
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

/// What a name reference (or compound reference) in expression / statement
/// position resolves to.
///
/// Produced by [`resolve`] for every resolvable AST node — `Path([X])`,
/// `Self`, `obj.field`, `Auto@state`, `#mutate Auto { … }`,
/// `Auto.field <op>= …`, `#> proc(args)`. The variant tells downstream
/// phases what *kind* of binding the reference resolves to without
/// requiring them to re-walk the AST.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum BindingRef {
    /// Resolved to a top-level item via the [`SymbolTable`].
    TopLevel(Symbol),
    /// Resolved to a parameter or `let` binding in the enclosing block-scope chain.
    Local(LocalBinding),
    /// A bare `Self` reference resolved to the enclosing `#automaton` (only
    /// inside `#transition` bodies). Carries the automaton's [`Symbol`] for
    /// downstream consumers.
    SelfRef {
        /// The enclosing automaton.
        automaton: Symbol,
    },
    /// An `Auto.field` or `Self.field` field-access whose `field` was
    /// validated against the automaton's declared field set. Recorded under
    /// the *outer* expression's span (the `FieldAccess` expression), or
    /// under the field-assign's span for `#mutate { field = … }` /
    /// mutation-sugar uses.
    AutomatonField {
        /// The automaton being indexed into.
        automaton: Symbol,
        /// The validated field name.
        field_name: String,
    },
    /// A `#> proc(args)` procedure call resolved to a target with a known
    /// [`CallContext`].
    Proc {
        /// The procedure name.
        name: String,
        /// Source span of the target declaration (effect or transition).
        target_span: Span,
        /// Which context this call falls into per Refinement #5b.
        ctx: CallContext,
    },
}

/// Per Refinement #5b, every `#> name(args)` call site is tagged with the
/// kind of callee it resolves to. Determined at resolution time from the
/// callee's declaration kind plus the enclosing `#mutates` / `#transition`
/// context.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum CallContext {
    /// Caller invokes a top-level `#effect`. The call is *identity* in the
    /// transition sense — no state-tag change happens at the call site.
    Identity,
    /// Caller invokes a `#transition` of an automaton in `#mutates` scope.
    /// The state-tag of the named automaton may change as a result.
    Transition,
    // Generic (interface dispatch via `Iface::method`) is reserved for the
    // future slice that resolves Decision #16's plugin-mutator surface.
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

    // Build side tables once: each automaton's field-name set and
    // transition-name set. Used during walking for `Auto.field` validation,
    // `Self.field` validation, and `#> proc(args)` Transition-context
    // resolution.
    let automaton_meta = build_automaton_meta(program);

    let mut walker = Walker {
        symbols: &symbols,
        automaton_meta: &automaton_meta,
        bindings: HashMap::new(),
        errors: Vec::new(),
        scopes: Vec::new(),
        enclosing: None,
    };

    for item in &program.items {
        match item {
            Item::Fn(decl) => walker.walk_fn_decl(decl),
            Item::Effect(decl) => walker.walk_effect_decl(decl),
            Item::Interrupt(decl) => walker.walk_interrupt_decl(decl),
            Item::Automaton(decl) => walker.walk_automaton_decl(decl),
            // `@type`, `@trait`, `#interface`, `#impl`, `#test`, `@sequential`
            // have no executable bodies that this resolver walks. Impl method
            // bodies arrive when parser slice 9+ produces them.
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

/// Side tables for each `#automaton` declaration: the set of declared field
/// names, the subset marked `#hidden` (Decision #25), and the set of
/// declared transition names. Lookups are O(1).
struct AutomatonMeta {
    /// Map: automaton name → set of field names.
    fields: HashMap<String, HashSet<String>>,
    /// Map: automaton name → set of `#hidden`-marked field names per
    /// Decision #25. Always a subset of `fields[name]`. Lookup answers
    /// "is `Auto.field` hidden?" in O(1) for the visibility check in
    /// [`Walker::require_field`].
    hidden_fields: HashMap<String, HashSet<String>>,
    /// Map: automaton name → map of transition name → transition source span.
    /// The span lets `BindingRef::Proc` point back at the transition's
    /// declaration site.
    transitions: HashMap<String, HashMap<String, Span>>,
}

fn build_automaton_meta(program: &Program) -> AutomatonMeta {
    let mut fields: HashMap<String, HashSet<String>> = HashMap::new();
    let mut hidden_fields: HashMap<String, HashSet<String>> = HashMap::new();
    let mut transitions: HashMap<String, HashMap<String, Span>> = HashMap::new();
    for item in &program.items {
        if let Item::Automaton(decl) = item {
            let f: HashSet<String> = decl.fields.iter().map(|f| f.name.clone()).collect();
            let h: HashSet<String> = decl
                .fields
                .iter()
                .filter(|f| f.hidden)
                .map(|f| f.name.clone())
                .collect();
            let t: HashMap<String, Span> = decl
                .transitions
                .iter()
                .map(|t| (t.name.clone(), t.span))
                .collect();
            fields.insert(decl.name.clone(), f);
            hidden_fields.insert(decl.name.clone(), h);
            transitions.insert(decl.name.clone(), t);
        }
    }
    AutomatonMeta {
        fields,
        hidden_fields,
        transitions,
    }
}

/// Internal walker state carried through one resolution pass.
struct Walker<'a> {
    symbols: &'a SymbolTable,
    automaton_meta: &'a AutomatonMeta,
    bindings: HashMap<Span, BindingRef>,
    errors: Vec<ResolveError>,
    /// Stack of nested scopes. Innermost is `last()`. Lookup walks
    /// outward from `last()` to `first()`.
    scopes: Vec<Scope>,
    /// The body-context the walker is currently inside, if any. Used for
    /// `Self` resolution (transition bodies) and `#> proc` Transition-context
    /// lookup (effects/interrupts via `#mutates`).
    enclosing: Option<EnclosingContext>,
}

/// Body-context information needed by `#> proc` resolution and by `Self`
/// binding inside `#transition` bodies.
#[derive(Debug, Clone)]
struct EnclosingContext {
    /// When walking a `#transition` body, the enclosing automaton's name.
    /// `None` for `@fn` / `#effect` / `#interrupt` bodies.
    transition_of: Option<String>,
    /// The set of automaton names available for Transition-context proc
    /// lookups. For `#effect` / `#interrupt` bodies this is the `#mutates`
    /// list. For `#transition` bodies this is `[transition_of]`. For `@fn`
    /// bodies this is empty.
    mutates: Vec<String>,
}

impl EnclosingContext {
    fn for_fn() -> Self {
        Self {
            transition_of: None,
            mutates: Vec::new(),
        }
    }

    fn for_effect_or_interrupt(mutates: &[String]) -> Self {
        Self {
            transition_of: None,
            mutates: mutates.to_vec(),
        }
    }

    fn for_transition(automaton: &str) -> Self {
        Self {
            transition_of: Some(automaton.to_owned()),
            mutates: vec![automaton.to_owned()],
        }
    }
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
    ///
    /// Special case: `Self` inside a `#transition` body resolves to a
    /// [`BindingRef::SelfRef`] pointing at the enclosing automaton. Outside
    /// transition bodies, `Self` falls through to the normal local/top-level
    /// lookup (and almost certainly errors as undefined — that's correct;
    /// `Self` is meaningless in `@fn` / `#effect` / `#interrupt` bodies).
    fn resolve_name(&mut self, name: &str, at: Span) {
        if name == "Self" {
            if let Some(EnclosingContext {
                transition_of: Some(auto_name),
                ..
            }) = &self.enclosing
            {
                if let Some(automaton) = self.symbols.lookup(auto_name) {
                    self.bindings.insert(
                        at,
                        BindingRef::SelfRef {
                            automaton: automaton.clone(),
                        },
                    );
                    return;
                }
            }
            // Fall through to error — Self outside a transition body has no
            // binding.
        }

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

    /// Validate that `field` exists on the automaton named `automaton`,
    /// and is *visible* from the current enclosing context per Decision #25.
    ///
    /// Records nothing on success; pushes one of:
    ///
    /// - [`ResolveError::UnknownField`] (E0405) — the field doesn't exist
    ///   on the automaton at all.
    /// - [`ResolveError::HiddenFieldNotAccessible`] (E0407) — the field
    ///   exists but is `#hidden`, and the current callable is not a
    ///   `#transition` of the *same* automaton. Per Decision #25's
    ///   algebraic-trivial-orthogonality reading, the hidden field's
    ///   basis vector cannot enter the `actual_writes` of any callable
    ///   outside the owning automaton's surface, so the engine's wedge
    ///   product never collapses against it from outside.
    ///
    /// If `automaton` doesn't even resolve to an `#automaton` symbol, this
    /// helper is a no-op — the upstream `require_automaton` call will have
    /// already pushed [`ResolveError::NotAnAutomaton`], and emitting
    /// E0405 on top would be redundant noise.
    ///
    /// Hidden-field visibility rule (Decision #25 §"Surface syntax"):
    ///
    /// - Inside a `#transition` of automaton `A` (`enclosing.transition_of
    ///   == Some(A)`), every field of `A` (hidden or not) is accessible.
    /// - Everywhere else (`@fn` bodies, `#effect` / `#interrupt` bodies
    ///   even when they declare `#mutates: [A]`, transitions of a
    ///   *different* automaton), `#hidden` fields of `A` are inaccessible.
    fn require_field(&mut self, automaton: &str, field: &str, at: Span) {
        let Some(field_set) = self.automaton_meta.fields.get(automaton) else {
            return;
        };
        if !field_set.contains(field) {
            self.errors.push(ResolveError::UnknownField {
                automaton: automaton.to_owned(),
                field: field.to_owned(),
                at: at.start,
            });
            return;
        }
        // The field exists. Now check Decision #25 visibility.
        let is_hidden = self
            .automaton_meta
            .hidden_fields
            .get(automaton)
            .is_some_and(|hs| hs.contains(field));
        if !is_hidden {
            return;
        }
        // Hidden field — only accessible from a transition of the same automaton.
        let inside_owning_transition = self
            .enclosing
            .as_ref()
            .and_then(|e| e.transition_of.as_deref())
            .is_some_and(|t| t == automaton);
        if !inside_owning_transition {
            self.errors.push(ResolveError::HiddenFieldNotAccessible {
                automaton: automaton.to_owned(),
                field: field.to_owned(),
                at: at.start,
            });
        }
    }

    /// Resolve a `#> name(args)` callee to a [`BindingRef::Proc`].
    ///
    /// Resolution order per Refinement #5b:
    /// 1. Top-level `#effect` with this name → [`CallContext::Identity`].
    /// 2. `#transition` with this name in any automaton listed in the
    ///    enclosing `#mutates` scope → [`CallContext::Transition`].
    /// 3. Otherwise → [`ResolveError::UnknownProc`].
    ///
    /// (1) is checked before (2) because effects and transitions live in
    /// distinct namespaces; a single name can't be both a top-level effect
    /// and a transition (effects are top-level Symbols; transitions live
    /// inside automata). If a transition shadows an effect by name in the
    /// same module, (1) wins — though `clifford-check` would likely diagnose
    /// the namespace shadow separately in a later phase.
    fn resolve_proc_call(&mut self, name: &str, at: Span) {
        // (1) Top-level `#effect`.
        if let Some(sym) = self.symbols.lookup(name) {
            if matches!(sym.kind, SymbolKind::Effect) {
                self.bindings.insert(
                    at,
                    BindingRef::Proc {
                        name: name.to_owned(),
                        target_span: sym.span,
                        ctx: CallContext::Identity,
                    },
                );
                return;
            }
        }

        // (2) `#transition` of an automaton in the enclosing `#mutates`
        //     (or transition's own automaton).
        if let Some(enc) = &self.enclosing {
            for auto_name in &enc.mutates {
                if let Some(transitions) = self.automaton_meta.transitions.get(auto_name) {
                    if let Some(span) = transitions.get(name) {
                        self.bindings.insert(
                            at,
                            BindingRef::Proc {
                                name: name.to_owned(),
                                target_span: *span,
                                ctx: CallContext::Transition,
                            },
                        );
                        return;
                    }
                }
            }
        }

        // (3) Unknown.
        self.errors.push(ResolveError::UnknownProc {
            name: name.to_owned(),
            at: at.start,
        });
    }

    /// Verify that a name resolves to an `#automaton` symbol. Used by
    /// `Mutate`, `MutateShort`, and `StateRead`. Records the resolution as
    /// a [`BindingRef::TopLevel`] under `at` on success; pushes
    /// [`ResolveError::NotAnAutomaton`] on failure.
    fn require_automaton(&mut self, name: &str, at: Span) {
        match self.symbols.lookup(name) {
            Some(sym) if matches!(sym.kind, SymbolKind::Automaton) => {
                self.bindings.insert(at, BindingRef::TopLevel(sym.clone()));
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
        let prev = self.enclosing.replace(EnclosingContext::for_fn());
        self.push_scope();
        for param in &decl.params {
            self.declare_param(param);
        }
        self.walk_block(&decl.body);
        self.pop_scope();
        self.enclosing = prev;
    }

    fn walk_effect_decl(&mut self, decl: &EffectDecl) {
        let prev = self
            .enclosing
            .replace(EnclosingContext::for_effect_or_interrupt(&decl.mutates));
        self.push_scope();
        for param in &decl.params {
            self.declare_param(param);
        }
        self.walk_block(&decl.body);
        self.pop_scope();
        self.enclosing = prev;
    }

    fn walk_interrupt_decl(&mut self, decl: &InterruptDecl) {
        let prev = self
            .enclosing
            .replace(EnclosingContext::for_effect_or_interrupt(&decl.mutates));
        self.push_scope();
        for param in &decl.params {
            self.declare_param(param);
        }
        self.walk_block(&decl.body);
        self.pop_scope();
        self.enclosing = prev;
    }

    fn walk_automaton_decl(&mut self, decl: &AutomatonDecl) {
        // Walk every `#transition` body. `Self` resolves to this automaton
        // and `Self.field` validates against this automaton's field set.
        for transition in &decl.transitions {
            self.walk_transition_decl(decl, transition);
        }
    }

    fn walk_transition_decl(&mut self, automaton: &AutomatonDecl, transition: &TransitionDecl) {
        let prev = self
            .enclosing
            .replace(EnclosingContext::for_transition(&automaton.name));
        self.push_scope();
        // Transitions take no parameters in the current AST (parser slice 8
        // doesn't accept them). When that lands, declare them here.
        self.walk_block(&transition.body);
        self.pop_scope();
        self.enclosing = prev;
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
                // Validate every field-assign's name against the automaton's
                // declared fields (E0405 if absent). Then walk the index
                // (if any) and the value expression.
                for fa in assigns {
                    self.require_field(automaton, &fa.field, fa.span);
                    if let Some(idx) = &fa.index {
                        self.walk_expr(idx);
                    }
                    self.walk_expr(&fa.value);
                }
            }
            StmtKind::MutateShort {
                automaton,
                field,
                value,
                ..
            } => {
                self.require_automaton(automaton, stmt.span);
                // Sugar form: validate the single field name as well.
                self.require_field(automaton, field, stmt.span);
                self.walk_expr(value);
            }
            StmtKind::ProcCall { name, args } => {
                self.resolve_proc_call(name, stmt.span);
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
            ExprKind::FieldAccess { obj, field } => {
                // Walk the receiver first so its resolution lands in the
                // bindings map.
                self.walk_expr(obj);
                // If the receiver resolved to an automaton (either via
                // `Path([Auto])` resolving to `SymbolKind::Automaton`, or
                // `Self` in a transition body), validate the field name and
                // record an `AutomatonField` binding under the *outer*
                // FieldAccess expression's span.
                let auto_sym: Option<Symbol> = match self.bindings.get(&obj.span) {
                    Some(BindingRef::TopLevel(s)) if matches!(s.kind, SymbolKind::Automaton) => {
                        Some(s.clone())
                    }
                    Some(BindingRef::SelfRef { automaton }) => Some(automaton.clone()),
                    _ => None,
                };
                if let Some(sym) = auto_sym {
                    self.require_field(&sym.name, field, expr.span);
                    self.bindings.insert(
                        expr.span,
                        BindingRef::AutomatonField {
                            automaton: sym,
                            field_name: field.clone(),
                        },
                    );
                }
            }
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
        let table = build_str("@fn pure_thing() { } #automaton imperative_thing { }").unwrap();
        assert_eq!(table.lookup("pure_thing").unwrap().layer, Layer::Functional);
        assert_eq!(
            table.lookup("imperative_thing").unwrap().layer,
            Layer::Imperative
        );
    }

    // ── Items that DO NOT contribute to the table ────────────────────────

    #[test]
    fn impl_does_not_populate_table() {
        let table =
            build_str("#interface Serial { } #automaton Counter { } #impl Serial for Counter { }")
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
        let table = build_str("#automaton A { } #automaton B { } @sequential(A, B);").unwrap();
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
        let errors = build_str("@fn dup() { } @fn dup() { } @fn dup() { }").unwrap_err();
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
        let table = build_str(r#"#test "first" { } #test "second" { } #test "first" { }"#).unwrap();
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
        let names: std::collections::HashSet<_> = table.all().map(|(n, _)| n.as_str()).collect();
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
        assert!(matches!(local.kind, LocalKind::Param { mutable: false }));
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
        let res = resolve_str("@fn f() -> u32 { let x: u32 = 1; return x; }").unwrap();
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
        let res =
            resolve_str("#effect e() #mutates: [] { let mut x: u32 = 1; let _y := x; }").unwrap();
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
        let res = resolve_str("@fn f(x: u32) -> u32 { let x: u32 = x + 1; return x; }").unwrap();
        // Find the `x` reference inside the `let` initializer (the `x + 1`).
        // The first `Path([x])` in the body's first stmt's value expr.
        let prog_tokens =
            tokenize("@fn f(x: u32) -> u32 { let x: u32 = x + 1; return x; }").unwrap();
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
        let res =
            resolve_str("@fn helper() { } @fn caller() { let helper := 1; let _x := helper; }")
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
        let errors = resolve_str("@fn f() -> u32 { let x := alpha; let y := beta; return gamma; }")
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
        let errors =
            resolve_str("#effect e() #mutates: [] { #mutate NotAThing { f = 1 }; }").unwrap_err();
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
            ResolveError::NotAnAutomaton {
                found: "function",
                ..
            }
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
            .filter(|b| {
                matches!(
                    b,
                    BindingRef::Local(LocalBinding {
                        kind: LocalKind::Let { .. },
                        ..
                    })
                )
            })
            .count();
        assert_eq!(local_count, 3);
    }

    // ── Recursion through expression structure ───────────────────────────

    #[test]
    fn names_inside_compound_expressions_resolve() {
        // Names buried inside Binary, Index, Call, etc. all resolve.
        let res =
            resolve_str("@fn f(buf: &[u8], i: u32, j: u32) -> u8 { return buf[i + j]; }").unwrap();
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
        let res = resolve_str("@fn f(n: u32) { let _x: u32 = n; let _arr := [0; n]; }").unwrap();
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
        // `#> log(x);` — both the `log` callee and the `x` argument resolve.
        // Slice 3 added proc-call resolution; we declare `log` as a
        // top-level `#effect` so it resolves cleanly.
        let res = resolve_str(
            "#effect log(b: u32) #mutates: [] { } \
             #effect e(x: u32) #mutates: [] { #> log(x); }",
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
        let res =
            resolve_str("#effect e(p: u32) #mutates: [] { let _v := #volatile_load<u8>(p); }")
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

    // ─── Slice 3: transitions, Self, ProcCall, field validation ─────────

    use clifford_ast::AutomatonField;

    /// Find the first BindingRef whose variant matches a predicate.
    fn find_binding<F>(res: &Resolution, pred: F) -> Option<&BindingRef>
    where
        F: Fn(&BindingRef) -> bool,
    {
        res.bindings.values().find(|b| pred(b))
    }

    fn count_bindings<F>(res: &Resolution, pred: F) -> usize
    where
        F: Fn(&BindingRef) -> bool,
    {
        res.bindings.values().filter(|b| pred(b)).count()
    }

    // ── Self in transition bodies ────────────────────────────────────────

    #[test]
    fn self_resolves_in_transition_body() {
        let res = resolve_str(
            "#automaton Counter { value: u32; \
             #transition tick { let _x := Self; } }",
        )
        .unwrap();
        let b =
            find_binding(&res, |b| matches!(b, BindingRef::SelfRef { .. })).expect("Self resolved");
        match b {
            BindingRef::SelfRef { automaton } => {
                assert_eq!(automaton.name, "Counter");
                assert_eq!(automaton.kind, SymbolKind::Automaton);
            }
            _ => unreachable!(),
        }
    }

    #[test]
    fn self_outside_transition_is_undefined() {
        let errors = resolve_str("@fn confused() -> u32 { return Self; }").unwrap_err();
        // Self outside a transition body has no binding → falls through to
        // UndefinedName.
        assert!(errors.iter().any(|e| matches!(
            e,
            ResolveError::UndefinedName { name, .. } if name == "Self"
        )));
    }

    // ── Self.field validation ────────────────────────────────────────────

    #[test]
    fn self_field_validates_against_enclosing_automaton() {
        let res = resolve_str(
            "#automaton Counter { value: u32; \
             #transition tick { let _x := Self.value; } }",
        )
        .unwrap();
        let b = find_binding(&res, |b| matches!(b, BindingRef::AutomatonField { .. }))
            .expect("Self.value resolved");
        match b {
            BindingRef::AutomatonField {
                automaton,
                field_name,
            } => {
                assert_eq!(automaton.name, "Counter");
                assert_eq!(field_name, "value");
            }
            _ => unreachable!(),
        }
    }

    #[test]
    fn self_field_unknown_is_e0405() {
        let errors = resolve_str(
            "#automaton Counter { value: u32; \
             #transition tick { let _x := Self.bogus; } }",
        )
        .unwrap_err();
        assert!(errors.iter().any(|e| matches!(
            e,
            ResolveError::UnknownField {
                automaton,
                field,
                ..
            } if automaton == "Counter" && field == "bogus"
        )));
    }

    // ── Auto.field validation in expression position ─────────────────────

    #[test]
    fn auto_field_read_validates() {
        let res = resolve_str(
            "#automaton Counter { value: u32; } \
             #effect peek() #mutates: [] { let _x := Counter.value; }",
        )
        .unwrap();
        let b = find_binding(&res, |b| matches!(b, BindingRef::AutomatonField { .. }))
            .expect("Counter.value resolved");
        match b {
            BindingRef::AutomatonField { field_name, .. } => {
                assert_eq!(field_name, "value");
            }
            _ => unreachable!(),
        }
    }

    #[test]
    fn auto_field_unknown_is_e0405() {
        let errors = resolve_str(
            "#automaton Counter { value: u32; } \
             #effect peek() #mutates: [] { let _x := Counter.bogus; }",
        )
        .unwrap_err();
        assert!(errors.iter().any(|e| matches!(
            e,
            ResolveError::UnknownField { field, .. } if field == "bogus"
        )));
    }

    #[test]
    fn field_access_on_non_automaton_does_not_validate() {
        // `param.field` where `param` is a struct parameter (not an automaton)
        // — slice 3 doesn't yet have type info to validate this, so the
        // resolver SILENTLY recurses into the receiver and stops. No error,
        // no AutomatonField binding.
        let res = resolve_str("@fn f(p: SomeStruct) -> u32 { return p.x; }").unwrap();
        // No AutomatonField binding produced.
        assert!(find_binding(&res, |b| matches!(b, BindingRef::AutomatonField { .. })).is_none());
    }

    // ── Mutate / MutateShort field validation ────────────────────────────

    #[test]
    fn mutate_validates_field_names() {
        let res = resolve_str(
            "#automaton Counter { value: u32; flags: u8; } \
             #effect e() #mutates: [Counter] { #mutate Counter { value = 1, flags = 0 }; }",
        )
        .unwrap();
        // No errors → both `value` and `flags` validated successfully.
        let _ = res;
    }

    #[test]
    fn mutate_unknown_field_is_e0405() {
        let errors = resolve_str(
            "#automaton Counter { value: u32; } \
             #effect e() #mutates: [Counter] { #mutate Counter { mystery = 1 }; }",
        )
        .unwrap_err();
        assert!(errors.iter().any(|e| matches!(
            e,
            ResolveError::UnknownField {
                automaton,
                field,
                ..
            } if automaton == "Counter" && field == "mystery"
        )));
    }

    #[test]
    fn mutate_short_unknown_field_is_e0405() {
        let errors = resolve_str(
            "#automaton Counter { value: u32; } \
             #effect e() #mutates: [Counter] { Counter.mystery = 1; }",
        )
        .unwrap_err();
        assert!(errors.iter().any(|e| matches!(
            e,
            ResolveError::UnknownField { field, .. } if field == "mystery"
        )));
    }

    #[test]
    fn mutate_unknown_automaton_skips_field_check() {
        // When the automaton is undefined, we get E0403 NotAnAutomaton
        // but NOT also E0405 UnknownField — the field check is skipped to
        // avoid redundant noise (you already know what's wrong).
        let errors =
            resolve_str("#effect e() #mutates: [] { #mutate NotAThing { whatever = 1 }; }")
                .unwrap_err();
        let has_e0403 = errors
            .iter()
            .any(|e| matches!(e, ResolveError::NotAnAutomaton { .. }));
        let has_e0405 = errors
            .iter()
            .any(|e| matches!(e, ResolveError::UnknownField { .. }));
        assert!(has_e0403);
        assert!(
            !has_e0405,
            "should not double-report a field on a non-automaton"
        );
    }

    // ── #> proc resolution + CallContext tagging ─────────────────────────

    #[test]
    fn proc_call_to_top_level_effect_is_identity() {
        let res = resolve_str(
            "#effect log(b: u8) #mutates: [] { } \
             #effect main_effect() #mutates: [] { #> log(0); }",
        )
        .unwrap();
        let b = find_binding(&res, |b| matches!(b, BindingRef::Proc { .. }))
            .expect("Proc binding present");
        match b {
            BindingRef::Proc {
                name,
                ctx: CallContext::Identity,
                ..
            } => assert_eq!(name, "log"),
            other => panic!("expected Identity Proc, got {:?}", other),
        }
    }

    #[test]
    fn proc_call_to_transition_in_mutates_scope_is_transition_context() {
        let res = resolve_str(
            "#automaton Counter { value: u32; \
             #transition tick { Counter.value = 1; } } \
             #effect bumper() #mutates: [Counter] { #> tick(); }",
        )
        .unwrap();
        // The `#> tick()` from inside `bumper` should resolve to the
        // Counter::tick transition with CallContext::Transition.
        let b = find_binding(&res, |b| {
            matches!(
                b,
                BindingRef::Proc {
                    ctx: CallContext::Transition,
                    ..
                }
            )
        })
        .expect("Transition Proc present");
        match b {
            BindingRef::Proc { name, ctx, .. } => {
                assert_eq!(name, "tick");
                assert_eq!(*ctx, CallContext::Transition);
            }
            _ => unreachable!(),
        }
    }

    #[test]
    fn proc_call_inside_transition_finds_sibling_transition() {
        // Inside a transition's body, `#> other_transition()` should resolve
        // because the enclosing context is the same automaton.
        let res = resolve_str(
            "#automaton Sm { \
             #transition first { #> second(); } \
             #transition second { } \
             }",
        )
        .unwrap();
        let b = find_binding(&res, |b| matches!(b, BindingRef::Proc { .. }))
            .expect("Proc binding present");
        match b {
            BindingRef::Proc { name, ctx, .. } => {
                assert_eq!(name, "second");
                assert_eq!(*ctx, CallContext::Transition);
            }
            _ => unreachable!(),
        }
    }

    #[test]
    fn proc_call_unknown_name_is_e0404() {
        let errors = resolve_str("#effect e() #mutates: [] { #> mystery(); }").unwrap_err();
        assert!(errors.iter().any(|e| matches!(
            e,
            ResolveError::UnknownProc { name, .. } if name == "mystery"
        )));
    }

    #[test]
    fn proc_call_to_function_is_e0404() {
        // `@fn helper` is not a proc-call target — only `#effect` and
        // `#transition` qualify. Calling it via `#>` is E0404.
        let errors = resolve_str(
            "@fn helper() { } \
             #effect e() #mutates: [] { #> helper(); }",
        )
        .unwrap_err();
        assert!(errors.iter().any(|e| matches!(
            e,
            ResolveError::UnknownProc { name, .. } if name == "helper"
        )));
    }

    #[test]
    fn proc_call_to_transition_outside_mutates_scope_is_e0404() {
        // The transition exists but the caller's `#mutates` doesn't include
        // its automaton, so it's not in scope.
        let errors = resolve_str(
            "#automaton Counter { value: u32; \
             #transition tick { Counter.value = 1; } } \
             #effect outsider() #mutates: [] { #> tick(); }",
        )
        .unwrap_err();
        assert!(errors.iter().any(|e| matches!(
            e,
            ResolveError::UnknownProc { name, .. } if name == "tick"
        )));
    }

    #[test]
    fn proc_call_target_span_points_at_target_decl() {
        let src = "#effect log(b: u8) #mutates: [] { } \
                   #effect e() #mutates: [] { #> log(0); }";
        let res = resolve_str(src).unwrap();
        let log_sym = res.symbols.lookup("log").unwrap();
        let b = find_binding(&res, |b| matches!(b, BindingRef::Proc { .. })).expect("Proc binding");
        match b {
            BindingRef::Proc { target_span, .. } => {
                assert_eq!(*target_span, log_sym.span);
            }
            _ => unreachable!(),
        }
    }

    // ── Transition body walking ──────────────────────────────────────────

    #[test]
    fn transition_body_let_bindings_resolve() {
        // `let next: u32 = Self.value + 1;` inside a transition body.
        let res = resolve_str(
            "#automaton Counter { value: u32; \
             #transition tick { let next: u32 = Self.value; let _x := next; } }",
        )
        .unwrap();
        // `next` (used in the second let RHS) resolves as a Local Let.
        let next_refs = count_bindings(&res, |b| {
            matches!(
                b,
                BindingRef::Local(LocalBinding {
                    name,
                    kind: LocalKind::Let { .. },
                    ..
                }) if name == "next"
            )
        });
        assert_eq!(next_refs, 1);
    }

    #[test]
    fn transition_body_self_field_then_arithmetic() {
        // `Self.value + 1` — Self resolves, Self.value validates, the `1`
        // literal has no resolution.
        let res = resolve_str(
            "#automaton Counter { value: u32; \
             #transition tick { let _x: u32 = Self.value + 1; } }",
        )
        .unwrap();
        let self_field_count =
            count_bindings(&res, |b| matches!(b, BindingRef::AutomatonField { .. }));
        assert_eq!(self_field_count, 1);
    }

    // ── Mixed program: every slice-3 feature together ────────────────────

    #[test]
    fn realistic_program_with_transitions_and_proc_calls() {
        let src = "\
            #automaton Counter {\n  \
              value: u32;\n  \
              #transition tick { Counter.value = Counter.value + 1; }\n  \
              #transition reset { Counter.value = 0; }\n\
            }\n\
            #effect bump() #mutates: [Counter] {\n  \
              #> tick();\n  \
              return;\n\
            }\n\
            #effect zero() #mutates: [Counter] {\n  \
              #> reset();\n\
            }\n\
        ";
        let res = resolve_str(src).expect("resolve");

        // Two Proc bindings — one Transition each.
        let proc_count = count_bindings(&res, |b| {
            matches!(
                b,
                BindingRef::Proc {
                    ctx: CallContext::Transition,
                    ..
                }
            )
        });
        assert_eq!(proc_count, 2);

        // Counter.value field accesses across two transitions and two reads
        // (`Counter.value = Counter.value + 1` has one read on the RHS).
        let field_count = count_bindings(&res, |b| {
            matches!(
                b,
                BindingRef::AutomatonField { field_name, .. } if field_name == "value"
            )
        });
        assert!(field_count >= 1);
    }

    // ── Suppression: type info matches reality on AutomatonField bindings ─

    #[test]
    fn automaton_field_binding_carries_correct_automaton() {
        let res = resolve_str(
            "#automaton A { x: u32; } \
             #automaton B { y: u32; } \
             #effect e() #mutates: [A, B] { let _a := A.x; let _b := B.y; }",
        )
        .unwrap();
        // Two AutomatonField bindings, one for A.x and one for B.y.
        let mut saw_a_x = false;
        let mut saw_b_y = false;
        for b in res.bindings.values() {
            if let BindingRef::AutomatonField {
                automaton,
                field_name,
            } = b
            {
                if automaton.name == "A" && field_name == "x" {
                    saw_a_x = true;
                }
                if automaton.name == "B" && field_name == "y" {
                    saw_b_y = true;
                }
            }
        }
        assert!(saw_a_x && saw_b_y);
    }

    // Compiles but unused — keeps the import alive for future tests.
    #[allow(dead_code)]
    fn _autoref(_: &AutomatonField) {}

    #[test]
    fn duplicate_item_and_undefined_name_both_reported() {
        // Mix slice-1 and slice-2 errors in one program.
        let errors =
            resolve_str("@fn dup() { } @fn dup() { } @fn caller() { use_undefined; }").unwrap_err();
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

    // ── Decision #25: `#hidden` field encapsulation ─────────────────────

    /// Hidden fields are accessible from inside the owning automaton's
    /// transitions — that's the *whole point* of the feature, so verify
    /// transitions get their normal access first. (The mutate-short form
    /// `Auto.field = value;` is the canonical write syntax inside
    /// transitions per Decision #15; `Self.field` reads work too.)
    #[test]
    fn hidden_field_accessible_from_owning_transition() {
        let src = "\
            #automaton Counter { \
              value: u32; \
              scratch: u32 #hidden; \
              #transition tick { \
                Counter.scratch = 1u32; \
                Counter.value   = 0u32; \
              } \
            }\
        ";
        let res = resolve_str(src);
        assert!(
            res.is_ok(),
            "expected own-transition access to succeed, got {res:?}"
        );
    }

    /// From an `#effect` body whose `#mutates` *does* list the owning
    /// automaton, hidden fields are still inaccessible. The `#mutates`
    /// declaration grants *automaton* access, not *hidden-field* access —
    /// per Decision #25 only the automaton's own transitions see the
    /// hidden basis vectors.
    #[test]
    fn hidden_field_e0407_from_effect_with_mutates() {
        let src = "\
            #automaton Counter { \
              value: u32; \
              scratch: u32 #hidden; \
            } \
            #effect bump() #mutates: [Counter] { \
              Counter.scratch = 1u32; \
            }\
        ";
        let errors = resolve_str(src).expect_err("expected E0407");
        let saw = errors.iter().any(|e| {
            matches!(
                e,
                ResolveError::HiddenFieldNotAccessible { automaton, field, .. }
                    if automaton == "Counter" && field == "scratch"
            )
        });
        assert!(
            saw,
            "expected E0407 HiddenFieldNotAccessible; got {errors:?}"
        );
    }

    /// From a `Self.field` reference in a *different* automaton's
    /// transition, hidden fields of `Counter` aren't even referenceable
    /// (the receiver doesn't resolve to `Counter`); from a `Counter.field`
    /// reference in another automaton's transition, E0407 fires.
    #[test]
    fn hidden_field_e0407_from_other_automaton_transition() {
        let src = "\
            #automaton A { \
              secret: u32 #hidden; \
              public: u32; \
            } \
            #automaton B { \
              tag: u32; \
              #transition spy { \
                A.secret = 7u32; \
              } \
            }\
        ";
        let errors = resolve_str(src).expect_err("expected E0407");
        let saw = errors.iter().any(|e| {
            matches!(
                e,
                ResolveError::HiddenFieldNotAccessible { automaton, field, .. }
                    if automaton == "A" && field == "secret"
            )
        });
        assert!(
            saw,
            "expected E0407 from other-automaton transition; got {errors:?}"
        );
    }

    /// From an `@fn` body, hidden fields are inaccessible (even if the
    /// `@fn` happens to take a path-position argument naming the owning
    /// automaton). Today `@fn`s don't take automaton-state arguments
    /// directly, so the easiest probe is a `Counter.field` read.
    #[test]
    fn hidden_field_e0407_from_pure_fn() {
        let src = "\
            #automaton Counter { \
              scratch: u32 #hidden; \
            } \
            @fn peek() -> u32 { \
              return Counter.scratch; \
            }\
        ";
        let errors = resolve_str(src).expect_err("expected E0407");
        let saw = errors.iter().any(|e| {
            matches!(
                e,
                ResolveError::HiddenFieldNotAccessible { automaton, field, .. }
                    if automaton == "Counter" && field == "scratch"
            )
        });
        assert!(saw, "expected E0407 from @fn; got {errors:?}");
    }

    /// A non-hidden field on the same automaton remains accessible from
    /// outside callables (the negative control: `#hidden` is opt-in,
    /// not blanket).
    #[test]
    fn non_hidden_field_remains_accessible_from_effect() {
        let src = "\
            #automaton Counter { \
              value: u32; \
              scratch: u32 #hidden; \
            } \
            #effect bump() #mutates: [Counter] { \
              Counter.value = 1u32; \
            }\
        ";
        let res = resolve_str(src);
        assert!(
            res.is_ok(),
            "expected non-hidden field to remain accessible; got {res:?}"
        );
    }

    /// E0407 (hidden-field) and E0405 (unknown-field) are distinct and
    /// don't bleed into each other: a real-but-hidden field gets E0407;
    /// an absent field gets E0405; both can fire from the same body.
    #[test]
    fn hidden_field_distinct_from_unknown_field() {
        let src = "\
            #automaton Counter { \
              scratch: u32 #hidden; \
            } \
            #effect bad() #mutates: [Counter] { \
              Counter.scratch = 1u32; \
              Counter.bogus  = 2u32; \
            }\
        ";
        let errors = resolve_str(src).expect_err("expected mixed errors");
        let hidden_count = errors
            .iter()
            .filter(|e| matches!(e, ResolveError::HiddenFieldNotAccessible { .. }))
            .count();
        let unknown_count = errors
            .iter()
            .filter(|e| matches!(e, ResolveError::UnknownField { .. }))
            .count();
        assert_eq!(hidden_count, 1, "want one E0407, got errors {errors:?}");
        assert_eq!(unknown_count, 1, "want one E0405, got errors {errors:?}");
    }

    /// Hidden access from a transition of automaton `A` to a hidden
    /// field of automaton `B` (named via `B.field`, not `Self.field`)
    /// is denied. The visibility check is by *owning* automaton, not by
    /// "any transition I happen to be in."
    #[test]
    fn hidden_field_e0407_cross_automaton_via_full_path_in_transition() {
        let src = "\
            #automaton A { \
              ax: u32; \
              #transition reach { \
                B.bsecret = 1u32; \
              } \
            } \
            #automaton B { \
              bsecret: u32 #hidden; \
            }\
        ";
        let errors = resolve_str(src).expect_err("expected E0407");
        let saw = errors.iter().any(|e| {
            matches!(
                e,
                ResolveError::HiddenFieldNotAccessible { automaton, field, .. }
                    if automaton == "B" && field == "bsecret"
            )
        });
        assert!(saw, "expected E0407 cross-automaton path; got {errors:?}");
    }

    /// Hidden array-typed fields are accessible from the owning
    /// automaton's transition (using the indexed `#mutate` block form).
    /// Exercises the `assigns[].index` walk path through `require_field`.
    #[test]
    fn hidden_array_field_indexed_write_in_own_transition_works() {
        let src = "\
            #automaton Counter { \
              cache: [u8; 4] #hidden; \
              #transition init { \
                #mutate Counter { cache[0usize] = 0u8 }; \
              } \
            }\
        ";
        let res = resolve_str(src);
        assert!(
            res.is_ok(),
            "expected indexed hidden-array write in own transition to succeed, got {res:?}"
        );
    }
}
