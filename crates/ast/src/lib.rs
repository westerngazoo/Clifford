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
/// Slice 2 variants cover most of §2.1's shape-only items. Bodies (function
/// bodies, effect bodies, transition bodies, interface method signatures,
/// impl method bodies) are deferred to subsequent parser slices that build
/// out statement/expression parsing.
///
/// Still deferred per §2.1: `@type`, `@trait`, `@module`, `static`, `const`,
/// `extern_block`, `use_decl`. These need type expressions and value
/// expressions which are slice-3+ work.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum Item {
    /// An `@fn` declaration.
    Fn(FnDecl),
    /// An `#automaton` declaration.
    Automaton(AutomatonDecl),
    /// An `#effect` declaration (top-level per Refinement #5a).
    Effect(EffectDecl),
    /// An `#interrupt` declaration. Differs from `#effect` in that the name
    /// becomes the linker symbol (Decision #10) and `#priority` is required.
    Interrupt(InterruptDecl),
    /// An `#interface` declaration (Decision #16).
    Interface(InterfaceDecl),
    /// An `#impl Interface for Automaton { … }` block (Decision #16).
    Impl(ImplDecl),
    /// A `#test "name" { … }` block (Decision #7).
    Test(TestDecl),
    /// An `@sequential(A, B);` non-concurrency attribute (Decision #11).
    Sequential(SequentialAttr),
}

impl Item {
    /// Which sigil layer does this item live in?
    ///
    /// Derived from the variant rather than stored, since the layer is fully
    /// determined by the item kind. Storing the layer would invite drift
    /// between variant and field.
    ///
    /// `@sequential(A, B);` is treated as functional-layer for
    /// classification purposes — the attribute lives in the functional
    /// layer per its `@` sigil — though it carries no body and serves only
    /// as input to the GA orthogonality engine (§7.3).
    #[must_use]
    pub fn layer(&self) -> Layer {
        match self {
            Self::Fn(_) | Self::Sequential(_) => Layer::Functional,
            Self::Automaton(_)
            | Self::Effect(_)
            | Self::Interrupt(_)
            | Self::Interface(_)
            | Self::Impl(_)
            | Self::Test(_) => Layer::Imperative,
        }
    }

    /// The source span covering the whole item, from leading sigil to its
    /// terminating token (closing brace or terminating semicolon).
    #[must_use]
    pub fn span(&self) -> Span {
        match self {
            Self::Fn(d) => d.span,
            Self::Automaton(d) => d.span,
            Self::Effect(d) => d.span,
            Self::Interrupt(d) => d.span,
            Self::Interface(d) => d.span,
            Self::Impl(d) => d.span,
            Self::Test(d) => d.span,
            Self::Sequential(d) => d.span,
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

/// An `#effect name() #mutates: [A, B] { … }` declaration (top-level per
/// Refinement #5a).
///
/// Slice-2 scope: name + `#mutates` automaton list + `#cannot_mutate` (if
/// present) + span. Empty parameter list, empty body. Parameters, return
/// type, full effect-meta clauses (`#invariant`, `#atomic`), trait list,
/// and body content all arrive in subsequent slices.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EffectDecl {
    /// The effect's name.
    pub name: String,
    /// Automaton names listed in `#mutates: [...]`. May be empty for pure
    /// effects (the spec permits an empty list).
    pub mutates: Vec<String>,
    /// Automaton names listed in `#cannot_mutate: [...]`. Optional.
    pub cannot_mutate: Vec<String>,
    /// Source span covering `#effect name() #mutates: [...] { }` end-to-end.
    pub span: Span,
}

/// An `#interrupt NAME() #mutates: [A] #priority: HIGH { … }` declaration.
///
/// The `name` is the linker symbol per Decision #10 — users write the
/// target-standard interrupt vector name (e.g., `USART1_IRQHandler`).
/// `#priority` is required for `#interrupt` (per §2.5 notes).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InterruptDecl {
    /// Interrupt vector name; becomes the linker symbol (Decision #10).
    pub name: String,
    /// Automaton names listed in `#mutates: [...]`.
    pub mutates: Vec<String>,
    /// Required `#priority: …` per §2.5 effect_meta requirements for `#interrupt`.
    pub priority: PriorityLevel,
    /// Source span covering the full declaration.
    pub span: Span,
}

/// `#priority: LOW | MEDIUM | HIGH | <integer>` per §2.5.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PriorityLevel {
    /// `LOW`
    Low,
    /// `MEDIUM`
    Medium,
    /// `HIGH`
    High,
    /// An explicit integer priority. The raw token text is preserved so
    /// the type checker can validate the numeric range against the target.
    Numeric(String),
}

/// An `#interface Name { … }` declaration (Decision #16).
///
/// Slice-2 scope: name + span only. The interface body — a list of effect
/// signatures `effect name(params) -> ret;` — arrives in slice 3 alongside
/// parameter and return-type parsing.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InterfaceDecl {
    /// The interface's name.
    pub name: String,
    /// Source span covering the full declaration.
    pub span: Span,
}

/// An `#impl Interface for Automaton { … }` block (Decision #16).
///
/// Slice-2 scope: interface name + automaton name + span. Method bodies
/// (the `effect name(params) -> ret { … }` items inside the braces) arrive
/// in slice 3 alongside body parsing.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ImplDecl {
    /// The interface being implemented.
    pub interface_name: String,
    /// The automaton implementing the interface.
    pub automaton_name: String,
    /// Source span covering the full declaration.
    pub span: Span,
}

/// A `#test "description" { … }` block (Decision #7).
///
/// Slice-2 scope: description string + span. Test bodies arrive when
/// statement parsing lands in slice 4. Each test runs in isolation;
/// automata are reset to their declared initial state before each
/// invocation (semantic detail enforced at runtime, not at parse).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TestDecl {
    /// Test description from the string literal.
    pub description: String,
    /// Source span covering the full declaration.
    pub span: Span,
}

/// An `@sequential(AutomatonA, AutomatonB);` top-level attribute (Decision #11).
///
/// Asserts to the GA orthogonality engine (§7.3) that the two named
/// automata never run concurrently. Symmetric: `@sequential(A, B)` and
/// `@sequential(B, A)` carry the same meaning. The attribute is *trusted*
/// — the compiler does not verify it, just consumes it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SequentialAttr {
    /// First automaton in the pair.
    pub a: String,
    /// Second automaton in the pair.
    pub b: String,
    /// Source span covering `@sequential(A, B);` end-to-end.
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
