//! # clifford-stdlib
//!
//! Standard library bootstrap for Clifford. Implements §9 of
//! `docs/CLIFFORD_SPEC.md`. Phase 5 of the implementation roadmap (§11).
//!
//! ## What lives here
//!
//! The Clifford standard library — `clifford::core`, `clifford::alloc`,
//! `clifford::sync`, `clifford::hal` — is **written in Clifford itself**, not
//! Rust. Per CLAUDE.md §6 Phase 5: "Stdlib is written in Clifford, not Rust.
//! It is the first dogfooding of the language."
//!
//! This Rust-side crate exists for build orchestration: invoking `cliffordc`
//! against the `.cl` source tree under `stdlib/src/cl/` (path TBD) to produce
//! the shipped stdlib artifacts during a release build. v0.1 scaffolding has
//! no `.cl` sources yet; Phase 5 populates them.
//!
//! ## v0.1 minimum stdlib
//!
//! ```text
//! clifford::core
//!   ├── option       — Option<T>
//!   ├── result       — Result<T, E>
//!   ├── slice        — slice operations
//!   ├── mem          — size_of, align_of, transmute
//!   └── ptr          — null, null access<T>, etc.
//!
//! clifford::alloc
//!   ├── bump         — BumpAlloc
//!   └── pool         — PoolAlloc<BLOCK_SIZE, NUM_BLOCKS>
//!
//! clifford::sync
//!   ├── atomic       — atomic_critical effect primitives
//!   └── mutex        — single-core mutex via #atomic: interrupt_critical
//!
//! clifford::hal
//!   └── (target-specific, vendored per-MCU)
//! ```
//!
//! ## v0.2 additions
//!
//! - `clifford::audit::ShadowSanitizer` (Decision #18)
//! - `clifford::ga` for native multivector types (deferred from Idea #7
//!   in the v0.5 design pass)
//! - `clifford::staged` runtime helpers for `#staged` automata (Decision #12)
//!
//! ## `unsafe`
//!
//! This crate is one of the two allowed `unsafe` sites (the other being
//! `clifford-codegen`). Every `unsafe` block requires a `// SAFETY:` comment.

#![warn(missing_docs)]
// `unsafe` allowed in this crate per CLAUDE.md §3.1.

/// Slice 37: canonical Clifford-source text for the
/// `clifford::audit` module. Currently a single
/// `#interface PointerAuditor { … }` declaration per
/// Decision #18. Embedded as a string constant via
/// `include_str!` so the runtime-audit wrap-emitting
/// pass (future slice) can parse and resolve against
/// the canonical surface without needing a path-resolution
/// step at compile time.
///
/// The source file lives at `crates/stdlib/cl/audit.cl`
/// and is the source of truth — edit there, not the
/// embedded string. The `include_str!` makes the file
/// participate in `cargo build`'s dependency tracking
/// (a change to `audit.cl` rebuilds this crate).
pub const AUDIT_CL_SOURCE: &str = include_str!("../cl/audit.cl");

#[cfg(test)]
mod tests {
    use super::*;
    use clifford_lexer::tokenize;
    use clifford_parser::parse;
    use clifford_resolve::resolve;

    #[test]
    fn smoke() {}

    /// Slice 37: verify the canonical `clifford::audit`
    /// interface text parses + resolves cleanly through the
    /// full pipeline. Catches regressions where a change to
    /// the .cl source (or a backwards-incompatible compiler
    /// change to interface parsing) silently breaks the
    /// stdlib bootstrap.
    #[test]
    fn audit_pointer_auditor_interface_parses_and_resolves() {
        let tokens = tokenize(AUDIT_CL_SOURCE).expect("tokenize audit.cl");
        let program = parse(&tokens).expect("parse audit.cl");
        resolve(&program).expect("resolve audit.cl");
    }

    #[test]
    fn audit_cl_source_declares_pointer_auditor() {
        // Light sanity check on the embedded source. If a
        // future edit accidentally removes the interface or
        // renames it, this test fires.
        assert!(
            AUDIT_CL_SOURCE.contains("#interface PointerAuditor"),
            "expected #interface PointerAuditor in audit.cl"
        );
        // Every required method is present.
        for required in &[
            "record_alloc(",
            "record_free(",
            "validate_load(",
            "validate_store(",
        ] {
            assert!(
                AUDIT_CL_SOURCE.contains(required),
                "missing required method `{required}` in audit.cl",
            );
        }
    }
}
