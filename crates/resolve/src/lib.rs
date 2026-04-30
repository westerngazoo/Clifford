//! # clifford-resolve
//!
//! Name resolution for the Clifford compiler. Implements §5.1 step 1 of
//! `docs/CLIFFORD_SPEC.md`: bind every identifier to a definition, including
//! call-site context classification per Refinement #5b.
//!
//! ## Responsibilities
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
//! Resolution runs after parsing and before type checking. The output is the
//! AST decorated with resolved-binding annotations.

#![forbid(unsafe_code)]
#![warn(missing_docs)]

use thiserror::Error;

/// Errors produced during name resolution.
///
/// Reserves the `E04xx` range.
#[derive(Debug, Error)]
pub enum ResolveError {
    /// Placeholder for Phase 1 scaffolding.
    #[error("E0400: resolver not yet implemented")]
    NotYetImplemented,
}

#[cfg(test)]
mod tests {
    #[test]
    fn smoke() {
        // Phase 1 scaffolding — full tests land per §5.1 implementation.
    }
}
