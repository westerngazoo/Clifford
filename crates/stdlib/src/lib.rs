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

/// Slice 38: canonical Clifford-source text for the
/// placeholder default `ShadowSanitizer` impl of
/// `PointerAuditor` (Decision #18). Like
/// [`AUDIT_CL_SOURCE`], this is embedded via
/// `include_str!` so the future runtime-audit wrap-
/// emitting pass can parse and resolve against the
/// canonical text without a path-resolution step.
///
/// **Slice-38 scope:** the `#impl PointerAuditor for
/// ShadowSanitizer { }` registration is parsed and
/// resolved; the method bodies are not yet present
/// because the parser's `#impl` body grammar currently
/// accepts only `{ }`. Method-body support lands in
/// slice 39; this constant becomes the actual default
/// (no-op / always-`true`) impl at that point.
pub const AUDIT_SHADOW_SANITIZER_CL_SOURCE: &str =
    include_str!("../cl/audit_shadow_sanitizer.cl");

/// Slice 38: the canonical full `clifford::audit` module
/// source — interface + default impl concatenated. Future
/// stdlib-loading work (slice 39+) will parse this as a
/// single translation unit so the
/// `#impl PointerAuditor for ShadowSanitizer` registration
/// has the interface in scope.
pub fn audit_module_source() -> String {
    let mut s = String::with_capacity(
        AUDIT_CL_SOURCE.len() + AUDIT_SHADOW_SANITIZER_CL_SOURCE.len() + 2,
    );
    s.push_str(AUDIT_CL_SOURCE);
    s.push('\n');
    s.push_str(AUDIT_SHADOW_SANITIZER_CL_SOURCE);
    s
}

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

    /// Slice 38: the placeholder `ShadowSanitizer` declares an
    /// `#impl PointerAuditor for ShadowSanitizer { }`. The
    /// body is empty until slice 39 lands `#impl` method
    /// bodies; this test locks in the registration shape.
    #[test]
    fn audit_shadow_sanitizer_source_registers_impl() {
        assert!(
            AUDIT_SHADOW_SANITIZER_CL_SOURCE
                .contains("#automaton ShadowSanitizer"),
            "expected ShadowSanitizer automaton",
        );
        assert!(
            AUDIT_SHADOW_SANITIZER_CL_SOURCE
                .contains("#impl PointerAuditor for ShadowSanitizer"),
            "expected #impl PointerAuditor for ShadowSanitizer registration",
        );
    }

    /// Slice 38: the combined `audit_module_source()` —
    /// interface + impl concatenated — parses + resolves
    /// cleanly. This is the canonical translation-unit shape
    /// the future stdlib-loading pass will consume.
    #[test]
    fn audit_module_source_parses_and_resolves() {
        let src = audit_module_source();
        let tokens = tokenize(&src).expect("tokenize combined audit module");
        let program = parse(&tokens).expect("parse combined audit module");
        resolve(&program).expect("resolve combined audit module");
    }

    /// Slice 42: the call-counting ShadowSanitizer declares
    /// four `u32` counter fields and increments the matching
    /// counter from each `validate_*` / `record_*` method.
    /// The test locks in the counter-field set so a future
    /// edit can't silently remove them.
    #[test]
    fn audit_shadow_sanitizer_has_call_counters() {
        for required in &["allocs", "frees", "loads", "stores"] {
            assert!(
                AUDIT_SHADOW_SANITIZER_CL_SOURCE.contains(&format!("{required}: u32")),
                "missing counter field `{required}: u32` in audit_shadow_sanitizer.cl",
            );
        }
        // Each method increments its matching counter.
        for required in &[
            "ShadowSanitizer.allocs += 1u32",
            "ShadowSanitizer.frees += 1u32",
            "ShadowSanitizer.loads += 1u32",
            "ShadowSanitizer.stores += 1u32",
        ] {
            assert!(
                AUDIT_SHADOW_SANITIZER_CL_SOURCE.contains(required),
                "missing counter increment `{required}` in audit_shadow_sanitizer.cl",
            );
        }
    }
}
