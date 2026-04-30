//! # clifford-ortho
//!
//! The GA Orthogonality Engine — the heart of Clifford. Implements §7 of
//! `docs/CLIFFORD_SPEC.md` and the product-existence theorem stated formally
//! in Appendix B.
//!
//! ## What this crate proves
//!
//! For every pair of computations (X, Y) where `can_concur(X, Y)`, the engine
//! verifies the wedge-product orthogonality condition:
//!
//! ```text
//! behavior(X) ∧ behavior(Y) ≠ 0  of grade  |behavior(X)| + |behavior(Y)|
//! ```
//!
//! Per Emergent Rule 6, this wedge-product check is the *constructive existence
//! proof* for the product category C_X × C_Y in Clifford's small-category
//! interpretation of automata (Decision #5 + Appendix B). It is not an
//! algorithmic shortcut; it is the theorem.
//!
//! ## Why this crate is special
//!
//! Per CLAUDE.md §4 ("The GA Orthogonality Engine — Special Standards"):
//!
//! - **100% line + branch coverage required** (track via `cargo-llvm-cov`).
//! - **Property tests required** for every public function (via `proptest`).
//! - **Two reviewers** for every PR — not just one.
//! - **Error messages name original source identifiers**, never raw `e_n`
//!   indices (unless `--verbose-basis` is on).
//! - **Every transformation preserves a documented invariant.** State the
//!   invariant in a comment, then test it.
//! - **No "optimisation" without a benchmark.** The XOR-bitmask representation
//!   is already O(1); resist cleverness.
//!
//! ## Implementation strategy
//!
//! - **Blade representation:** u64 bitmask (1 bit per basis vector). Supports
//!   up to 64 combined dimensions per compilation unit; v0.2 will switch to a
//!   fixed-size bit array for n > 64. The XOR-bitmask wedge-product is
//!   structurally identical to garust's representation; vendor-vs-in-tree
//!   decision deferred per §7.8.
//! - **Basis assignment:** automaton fields first (declaration order, with
//!   `#basis` overrides honoured), then traits (canonical order: predeclared
//!   `Pure`/`Readable`/`Observable`/`Opaque` first, then user-declared traits
//!   in `@trait` declaration order). Per Emergent Rule 1, traits get globally
//!   consistent basis vectors.
//! - **Behaviour multivector:** outer product of basis vectors corresponding
//!   to fields the effect writes (transitively through `#>` calls) and traits
//!   it carries. Sum across an automaton's effects.
//! - **Concurrency inference:** §7.3's sound-conservative heuristic, plus
//!   user-supplied `@sequential(A, B)` overrides (Decision #11).
//! - **Read-write race honesty:** v0.1 catches write-write races at field
//!   granularity only; read-write races deferred to v0.2 graded read/write
//!   algebra extension (§7.2).

#![forbid(unsafe_code)]
#![warn(missing_docs)]

use thiserror::Error;

/// Errors produced by the orthogonality engine.
///
/// Reserves the `E05xx` range. The most important error is `E0520:
/// orthogonality violation` — its message must name the conflicting fields
/// and traits by source identifier per §7.5.
#[derive(Debug, Error)]
pub enum OrthoError {
    /// Placeholder for Phase 3 scaffolding.
    #[error("E0500: GA orthogonality engine not yet implemented")]
    NotYetImplemented,
}

/// XOR-bitmask wedge-product on two blades.
///
/// Returns `Some(a | b)` when the wedge is non-zero (no shared basis vector),
/// `None` when the wedge is zero (some basis vector squared).
///
/// This is the algorithmic core of Clifford's concurrency safety proof. Per
/// Emergent Rule 6 it is the constructive existence test for the
/// product-category morphism `(f_A, f_B)`.
///
/// # Examples
///
/// ```
/// use clifford_ortho::outer_product;
///
/// // Disjoint bitmasks: wedge is the union.
/// assert_eq!(outer_product(0b0011, 0b1100), Some(0b1111));
///
/// // Sharing a bit: wedge is zero.
/// assert_eq!(outer_product(0b0011, 0b0110), None);
/// ```
///
/// # Invariant
///
/// `outer_product(a, b).is_some() ⟺ a & b == 0` for all `a, b: u64`.
#[must_use]
pub fn outer_product(a: u64, b: u64) -> Option<u64> {
    if a & b != 0 {
        None
    } else {
        Some(a | b)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn disjoint_bitmasks_wedge_to_union() {
        assert_eq!(outer_product(0, 0), Some(0));
        assert_eq!(outer_product(0b0011, 0b1100), Some(0b1111));
        assert_eq!(outer_product(0b0001, 0b1000), Some(0b1001));
    }

    #[test]
    fn sharing_any_bit_yields_none() {
        assert_eq!(outer_product(0b0011, 0b0010), None);
        assert_eq!(outer_product(0b1111, 0b1000), None);
        assert_eq!(outer_product(u64::MAX, 1), None);
    }

    // Property tests with proptest go here once the v0.1 implementation lands.
    // The mandatory invariant per CLAUDE.md §4.1 is:
    //
    //     outer_product(a, b).is_some() ⟺ a & b == 0
    //
    // for all a, b: u64.
}
