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
//! Per §3 of the spec and Decision #1 (sigil layering), the parser stamps every
//! item and statement with its sigil layer (`@` functional, `#` imperative).
//! That stamp lives on the AST node and is consumed by `clifford-check` (§5.5)
//! to enforce the cross-boundary rules without re-scanning source.
//!
//! ## Phase 0 scaffolding
//!
//! The full AST per §3 is built out incrementally during Phase 0. Currently
//! exposes only the `Layer` enum and a placeholder `Program` so downstream
//! crates can refer to the types.

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
/// Phase 0 placeholder. The full `Item` enum (covering `@fn`, `@type`,
/// `@trait`, `@module`, `#automaton`, `#effect`, `#interrupt`, `#interface`,
/// `#impl`, `#test`, `static`, `const`, `extern_block`, `use_decl`,
/// `@sequential` attribute) lands during parser implementation.
#[derive(Debug, Clone, Default)]
pub struct Program {
    /// Source span covering the entire file.
    pub span: Span,
    // Items populated during parser implementation (§3 / §2.1).
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
        assert_eq!(p.span.start, 0);
        assert_eq!(p.span.end, 0);
    }
}
