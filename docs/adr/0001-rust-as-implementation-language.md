# ADR 0001: Rust as the Implementation Language for `cliffordc`

**Status:** Accepted
**Date:** 2026-04-30
**Deciders:** Goose

## Context

Clifford the language is being designed to compete in the safety-critical
embedded systems space. The compiler `cliffordc` needs to be implemented in
some language. The choice constrains:

- Performance characteristics (compile-time targets per CLAUDE.md §7).
- Available LLVM bindings (the back end is locked to LLVM IR per `CLIFFORD_SPEC.md` §8.1).
- Library ecosystem for parser/type-checker tooling.
- Bootstrap path (we are not yet at v0.2 where the stdlib is written in Clifford
  itself; we cannot yet self-host).
- Author familiarity and team-of-one velocity.

## Decision

`cliffordc` is implemented in **Rust**, using the workspace layout in CLAUDE.md
§2 and the toolchain pinned in `rust-toolchain.toml` (currently 1.76.0).

LLVM bindings: `inkwell` is the leading candidate; final pick deferred to
Phase 4 codegen kickoff. Either inkwell or `llvm-sys` works.

## Consequences

### Positive

- **Excellent LLVM bindings.** `inkwell` and `llvm-sys` are mature and
  actively maintained.
- **Strong type system for the implementation.** Algebraic data types make AST
  manipulation straightforward; trait dispatch fits visitor patterns.
- **Memory safety in the compiler itself.** Use-after-free in the type
  checker would be embarrassing for a language that proves UAF-freedom.
- **`thiserror` + `codespan-reporting` give us excellent diagnostics.**
- **Performance acceptable for compile-time targets.** Released-mode Rust
  hits the "compile 10kLoC in <5s" target comfortably.
- **Rich ecosystem for testing.** `proptest` for the GA engine's mandatory
  property tests; `insta` for AST/IR snapshot tests; `criterion` for benches.
- **Author familiarity.** Goose has Rust experience; team-of-one velocity matters.

### Negative

- **`unsafe` exists in Rust** and we use it in `crates/codegen` (LLVM
  bindings) and `crates/stdlib` (memory operations). Mitigated by per-block
  `// SAFETY:` comments per CLAUDE.md §3.1.
- **Rust compile times themselves are not great.** `cliffordc` itself takes a
  while to build. Mitigated by the `[profile.dev]` opt-level=0 setting and
  incremental compilation.
- **Tempting to mirror Rust idioms in Clifford language design.** This is a
  real risk — see DECISIONS.md for the discipline of *not* copying Rust:
  no lifetime annotations, no aggregating `unsafe` block, no raw `*const T`,
  no Iterator trait, no async, no closures (yet). Clifford is not safer-Rust;
  the implementation language is incidental.

### Alternatives considered

- **OCaml.** Strong type system, mature compiler-construction ecosystem
  (Menhir, ppx). Rejected: weaker LLVM bindings, smaller community for
  systems work.
- **Haskell.** Excellent for compilers in academia. Rejected: lazy evaluation
  makes performance reasoning harder; LLVM bindings less mature.
- **C++.** Native LLVM home. Rejected: we'd need to be writing C++ to
  bootstrap a language *better than* C++; the meta-message would be wrong,
  and the safety story would be weak.
- **Self-hosting from day one.** Rejected: cannot self-host before the
  language exists; would push v0.1 by years.

## Compliance

This decision is compatible with:

- CLAUDE.md §0 ("Written in Rust. Targets LLVM IR. No runtime.")
- CLAUDE.md §3.1 (Rust style standards apply to all crates).
- `CLIFFORD_SPEC.md` §11 (Phase 0 explicitly names Rust as the
  implementation language).

## Revisit when

- v0.2 stdlib bootstrap reveals that more of the compiler can move into
  Clifford itself (self-hosting path).
- A future LLVM-binding development in Clifford makes self-hosted codegen
  practical.

Until then: Rust.
