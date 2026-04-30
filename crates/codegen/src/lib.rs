//! # clifford-codegen
//!
//! LLVM IR code generation for the Clifford compiler. Implements §8 of
//! `docs/CLIFFORD_SPEC.md`.
//!
//! ## Targets
//!
//! Per §8.1: `thumbv6m-none-eabi`, `thumbv7em-none-eabihf`,
//! `riscv32imac-unknown-none-elf`, `riscv64gc-unknown-none-elf`,
//! `x86_64-unknown-linux-gnu` (for testing).
//!
//! ## Lowering responsibilities
//!
//! - **§8.3 Lowering rules:** primitives → LLVM types; ADTs → tagged structs;
//!   `&T` / `&mut T` → `T*` (`noalias` for `&mut`); `access<T>` /
//!   `access const<T>` → `T*` (nominal identity is Clifford-level only,
//!   Decision #19); narrow unsafe primitives → one-to-one LLVM operations
//!   (Decision #17 + #19); sigma loops → counted loops with bounds-check
//!   elision (§5.8 + §8.4).
//! - **§8.4 Automaton/transition/effect lowering:** state struct per non-
//!   register-block automaton; state-tag field for multi-state automata;
//!   one LLVM function per effect, transition, hardware mutator, and per
//!   `(generic_effect, interface_arg)` specialisation (Decision #16);
//!   transition-atomicity wrapping per Refinement #5e (cli/sti or
//!   LDREX/STREX based on R(A) and target); register-block field reads/writes
//!   as volatile loads/stores at `address + offset` (Decision #6); bit-field
//!   RMW with target-atomic when concurrent writer exists (Decision #20).
//! - **§8.5 Interrupt handler emission:** `#interrupt NAME` produces an LLVM
//!   function with linker symbol `NAME`, target-specific calling convention,
//!   `.interrupts` section (Decision #10).
//!
//! ## Optimisation policy
//!
//! - Downward inlining (`@fn` → `#effect`) is permitted and is the standard
//!   optimisation path (§4.7 clarification per analyst feedback).
//! - Upward inlining (`#effect` → `@fn`) is forbidden at all levels (Emergent
//!   Rule 4).
//! - LLVM does most of the heavy lifting; this crate emits clean IR.
//!
//! ## `unsafe`
//!
//! This crate is one of the two allowed `unsafe` sites (per CLAUDE.md §3.1,
//! the other being `clifford-stdlib`). Every `unsafe` block requires a
//! `// SAFETY:` comment proving its invariants.

#![warn(missing_docs)]
// `unsafe` is allowed in this crate per CLAUDE.md §3.1; do NOT add
// `#![forbid(unsafe_code)]` here. Specific unsafe blocks must each justify
// themselves with a `// SAFETY:` comment.

use thiserror::Error;

/// Errors produced during code generation.
///
/// Reserves the `E08xx` range alongside the `E08xx` block in §10 conformance
/// tests. (Codegen errors are typically internal — user errors are caught
/// before this phase.)
#[derive(Debug, Error)]
pub enum CodegenError {
    /// Placeholder for Phase 4 scaffolding.
    #[error("E0810: codegen not yet implemented")]
    NotYetImplemented,
}

#[cfg(test)]
mod tests {
    #[test]
    fn smoke() {}
}
