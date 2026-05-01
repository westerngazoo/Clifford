//! # clifford-ast
//!
//! Shared AST types for the Clifford compiler. Implements §3 (Parser
//! Behavior) of `docs/CLIFFORD_SPEC.md` — specifically the AST node kinds
//! catalogued there.
//!
//! ## Why a separate crate
//!
//! The AST is consumed by every phase from `parser` onward. Putting it in its
//! own crate (rather than re-exporting from `parser`) keeps the dependency
//! pipeline clean (per CLAUDE.md §2: no backward edges) and lets `resolve`,
//! `types`, `check`, etc. depend on AST without depending on the parser.
//!
//! ## Sigil layer is preserved on every node
//!
//! Per §3 of the spec and Decision #1 (sigil layering), the parser stamps
//! every item and statement with its sigil layer (`@` functional, `#`
//! imperative). That stamp lives on the AST node and is consumed by
//! `clifford-check` (§5.5) to enforce the cross-boundary rules without
//! re-scanning source.
//!
//! ## Implementation status
//!
//! First slice (this PR): the [`Program`] / [`Item`] / [`FnDecl`] /
//! [`AutomatonDecl`] skeleton. Items carry name + span only — bodies,
//! parameters, return types, trait lists, automaton fields, transitions,
//! effects all come in subsequent slices alongside their parser
//! implementations.

#![forbid(unsafe_code)]
#![warn(missing_docs)]

use clifford_lexer::Span;

/// Which sigil layer an item or statement belongs to.
///
/// Per Decision #1 in `docs/DECISIONS.md`, every AST node carries this stamp
/// from parsing forward. The type checker (§5.5) reads it to enforce that
/// `@`-layer code cannot contain `#`-layer constructs (Emergent Rule 4).
///
/// # Examples
///
/// ```
/// use clifford_ast::Layer;
/// let l = Layer::Functional;
/// assert!(l.is_functional());
/// assert!(!l.is_imperative());
/// ```
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Layer {
    /// Functional layer (`@`-prefixed). `@fn`, `@type`, `@trait`, `@module`.
    /// Default-immutable, default-`$ [Pure]`. Cannot contain `#`-constructs.
    Functional,
    /// Imperative layer (`#`-prefixed). `#automaton`, `#effect`, `#interrupt`,
    /// `#interface`, `#impl`, `#test`, `#mutate`, `#transition`, `#> proc()`,
    /// narrow unsafe primitives. Can call `@fn` freely; can perform mutation.
    Imperative,
}

impl Layer {
    /// True if this is the functional layer.
    #[must_use]
    pub fn is_functional(self) -> bool {
        matches!(self, Self::Functional)
    }

    /// True if this is the imperative layer.
    #[must_use]
    pub fn is_imperative(self) -> bool {
        matches!(self, Self::Imperative)
    }
}

/// The root of the parsed AST: a sequence of top-level items.
///
/// Per §2.1 of the spec, a program is an unordered sequence of items. Order
/// preservation in this `Vec` is a deliberate choice for reproducible
/// diagnostics and golden-file tests; semantics do not depend on order.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Program {
    /// Source span covering the entire file.
    pub span: Span,
    /// Top-level items in source order.
    pub items: Vec<Item>,
}

/// A top-level item.
///
/// First-slice variants only (`@fn`, `#automaton`). The full set per §2.1 —
/// `@type`, `@trait`, `@module`, `#effect` (top-level per Refinement #5a),
/// `#interrupt`, `#interface`, `#impl`, `#test`, `static`, `const`,
/// `extern_block`, `use_decl`, `@sequential` attribute — arrives in
/// subsequent parser slices.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum Item {
    /// An `@fn` declaration.
    Fn(FnDecl),
    /// An `#automaton` declaration.
    Automaton(AutomatonDecl),
}

impl Item {
    /// Which sigil layer does this item live in?
    ///
    /// Derived from the variant rather than stored, since the layer is fully
    /// determined by the item kind (no `@fn` is ever in the imperative
    /// layer; no `#automaton` is ever in the functional layer). Storing the
    /// layer would invite drift between variant and field.
    #[must_use]
    pub fn layer(&self) -> Layer {
        match self {
            Self::Fn(_) => Layer::Functional,
            Self::Automaton(_) => Layer::Imperative,
        }
    }

    /// The source span covering the whole item, from leading sigil to
    /// closing brace.
    #[must_use]
    pub fn span(&self) -> Span {
        match self {
            Self::Fn(d) => d.span,
            Self::Automaton(d) => d.span,
        }
    }
}

/// An `@fn name() { … }` declaration.
///
/// First-slice scope: name + span only. Generic parameters, value parameters,
/// return type, trait list (`$ [TraitList]` — Decision #2), where-clause,
/// extern modifier, and body all arrive in subsequent slices.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FnDecl {
    /// The function's name.
    pub name: String,
    /// Source span covering `@fn name() { }` end-to-end.
    pub span: Span,
}

/// An `#automaton Name { … }` declaration.
///
/// First-slice scope: name + span only. `#address` register-block annotation
/// (Decision #6), `#basis` clause (Decision #4), `#states` list, automaton
/// fields, named transitions (Refinement #5b) all arrive in subsequent
/// slices.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AutomatonDecl {
    /// The automaton's name.
    pub name: String,
    /// Source span covering `#automaton Name { }` end-to-end.
    pub span: Span,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn layer_predicates() {
        assert!(Layer::Functional.is_functional());
        assert!(!Layer::Functional.is_imperative());
        assert!(Layer::Imperative.is_imperative());
        assert!(!Layer::Imperative.is_functional());
    }

    #[test]
    fn empty_program() {
        let p = Program::default();
        assert!(p.items.is_empty());
        assert_eq!(p.span.start, 0);
        assert_eq!(p.span.end, 0);
    }

    #[test]
    fn item_layer_is_derived_from_variant() {
        let f = Item::Fn(FnDecl {
            name: "foo".into(),
            span: Span::new(0, 10),
        });
        assert_eq!(f.layer(), Layer::Functional);

        let a = Item::Automaton(AutomatonDecl {
            name: "Bar".into(),
            span: Span::new(0, 14),
        });
        assert_eq!(a.layer(), Layer::Imperative);
    }
}
