//! # clifford-effect
//!
//! Effect & FSM extraction for the Clifford compiler. Implements §6 of
//! `docs/CLIFFORD_SPEC.md` and the categorical foundation formalised in
//! Appendix B.
//!
//! ## Per-automaton outputs
//!
//! For each `#automaton A` declaration, this phase constructs:
//!
//! 1. **The category C_A** (§6.1): objects = `A.#states` (or synthetic
//!    `[Ready]` for monoid automata per Decision #5 Rule 4); morphisms =
//!    `#transition` declarations + implicit identities. Standard reachability
//!    and deadlock analysis run uniformly across multi-state and monoid forms.
//! 2. **Per-effect mutation profile** (§6.2): the set of `(automaton, field)`
//!    pairs each effect actually writes via `#mutate` statements (canonical or
//!    sugar form), unioned with the transitive `#mutates` of `#>`-called
//!    callees. Verified to be a subset of the declared `#mutates` clause.
//! 3. **Per-effect read profile** (§6.2 inferred): fields and `static` paths
//!    each effect reads.
//! 4. **Effect-procedure call graph** (§6.3): edges are `#> name(args)` calls;
//!    each edge labelled with `CallContext` (Transition, Identity, Generic per
//!    Refinement #5b's generalisation).
//! 5. **State-tag update points** (§6.4): the body-completion location at
//!    which each `#transition`'s state-tag write fires.
//! 6. **Interrupt-overlap set R(A)** (Refinement #5e): `{ I | I is a
//!    #interrupt and I.#mutates names A transitively }`. Drives §8.4's
//!    transition-atomicity wrapping decision.
//!
//! ## Output
//!
//! The phase produces an `EffectGraph` value consumed by `clifford-ortho`
//! (§7 GA orthogonality engine) and `clifford-codegen` (§8 lowering).

#![forbid(unsafe_code)]
#![warn(missing_docs)]

use thiserror::Error;

/// Errors produced during effect / FSM extraction.
#[derive(Debug, Error)]
pub enum EffectError {
    /// Placeholder for Phase 2 scaffolding.
    #[error("E0400: effect / FSM extractor not yet implemented")]
    NotYetImplemented,
}

#[cfg(test)]
mod tests {
    #[test]
    fn smoke() {}
}
