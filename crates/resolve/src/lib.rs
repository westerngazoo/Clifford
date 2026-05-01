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
//! **Slice 1 (this PR):** top-level [`SymbolTable`] only. Walks
//! `Program.items` and produces a global namespace mapping name → declaring
//! item. Detects duplicate item names (E0401). Body resolution, scope chains,
//! `Auto.field` / `Auto@state` / `#> proc` resolution, and interface coherence
//! all arrive in subsequent slices.

#![forbid(unsafe_code)]
#![warn(missing_docs)]

use std::collections::HashMap;

use clifford_ast::{Item, Layer, Program};
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
}
