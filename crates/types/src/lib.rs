//! # clifford-types
//!
//! Hindley–Milner type inference + structural trait resolution for the
//! Clifford compiler. Implements §4 (Type System) and §5.2–§5.3 of
//! `docs/CLIFFORD_SPEC.md`.
//!
//! ## Responsibilities
//!
//! - HM inference for local bindings (§4.8): integer literal default `i32`,
//!   float literal default `f64`, generic parameter inference from arguments.
//! - Structural trait satisfaction (§5.3): a type satisfies a trait iff it
//!   has methods with matching signatures; `Self` substituted by the candidate.
//! - Built-in trait obligations (§4.5): `Pure`, `Readable`, `Observable`,
//!   `Opaque`. Default `$ [Pure]` for unannotated `@fn` (Emergent Rule 2).
//! - Nominal access type identity (Decision #19): `access<T>` and `access const<T>`
//!   carry per-`@type` distinct identity; `#unchecked_cast` is the only
//!   cross-type bridge.
//! - Function-pointer types include trait list as part of identity (§2.7).
//!
//! ## Phase boundary
//!
//! Types runs after `clifford-resolve`. Output is the typed AST consumed by
//! `clifford-check`, `clifford-effect`, and `clifford-codegen`.

#![forbid(unsafe_code)]
#![warn(missing_docs)]

use thiserror::Error;

/// Errors produced during type checking.
///
/// Reserves the `E02xx` range for type-system errors per §10's error-code
/// conventions (`E0201` declared-trait body violation, etc.).
#[derive(Debug, Error)]
pub enum TypeError {
    /// Placeholder for Phase 1 scaffolding.
    #[error("E0200: type checker not yet implemented")]
    NotYetImplemented,
}

#[cfg(test)]
mod tests {
    #[test]
    fn smoke() {}
}
