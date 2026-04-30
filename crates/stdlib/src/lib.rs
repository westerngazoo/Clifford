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

#[cfg(test)]
mod tests {
    #[test]
    fn smoke() {}
}
