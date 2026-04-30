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
//! ## Error code ranges
//!
//! - `E01xx`: sigil-boundary violations (§5.5).
//! - `E02xx`: trait-list obligations (§5.6).
//! - `E03xx`: mutability and mutation-context violations (§5.4).
//! - `E07xx`: reference provenance / body-scoped borrowing (§5.7, Decision #13).
//! - `E08xx`: sigma bounds tracking (§5.8, Decision #14).

#![forbid(unsafe_code)]
#![warn(missing_docs)]

use thiserror::Error;

/// Errors produced during semantic checking.
#[derive(Debug, Error)]
pub enum CheckError {
    /// Placeholder for Phase 1 scaffolding.
    #[error("E0300: check phase not yet implemented")]
    NotYetImplemented,
}

#[cfg(test)]
mod tests {
    #[test]
    fn smoke() {}
}
