# Compiler Internals — Architecture Overview

> Companion to `docs/CLIFFORD_SPEC.md` (the *what*) and `docs/DECISIONS.md`
> (the *why*). This document is the *how*: how the `cliffordc` Rust codebase
> is laid out and how data flows through it.

## Pipeline

Inter-crate dependencies flow forward only. No backward edges (CLAUDE.md §2).

```
                  ┌──────────────────┐
   .cl source ──▶ │  clifford-lexer  │  §1 lexical structure
                  └────────┬─────────┘
                           ▼ Vec<Token>
                  ┌──────────────────┐
                  │ clifford-parser  │  §2 grammar, §3 parser behaviour
                  └────────┬─────────┘
                           ▼ Program (AST in clifford-ast)
                  ┌──────────────────┐
                  │ clifford-resolve │  §5.1 step 1: bind every ident
                  └────────┬─────────┘
                           ▼ AST + bindings + CallContext tags
                  ┌──────────────────┐
                  │  clifford-types  │  §4 type system, §5.2–§5.3
                  └────────┬─────────┘
                           ▼ typed AST
                  ┌──────────────────┐
                  │  clifford-check  │  §5.4–§5.8 (mutability, sigil
                  └────────┬─────────┘  boundary, traits, provenance,
                           ▼ verified  sigma bounds)
                           │ typed AST
                  ┌──────────────────┐
                  │ clifford-effect  │  §6, Appendix B (FSM + categorical)
                  └────────┬─────────┘
                           ▼ EffectGraph
                  ┌──────────────────┐
                  │  clifford-ortho  │  §7 GA orthogonality engine —
                  └────────┬─────────┘  the heart of the compiler
                           ▼ verified EffectGraph
                  ┌──────────────────┐
                  │ clifford-codegen │  §8 lowering to LLVM IR
                  └────────┬─────────┘
                           ▼ .ll / .o / .elf
                           │
                  Linker (target-specific) ──▶ .bin / executable
```

## Crate boundaries

- **`clifford-lexer`**: UTF-8 source → `Vec<Token>` with `Span`s. No grammar
  knowledge; produces atomic sigil-prefixed forms (`#automaton`, `@fn`,
  `#unchecked_load`, …) as single tokens.
- **`clifford-ast`**: shared AST types (`Layer`, `Program`, item kinds, …).
  Depends only on `clifford-lexer` (for `Span`).
- **`clifford-parser`**: recursive-descent parser with sigil-driven dispatch
  (§3). Produces a `Program` from a token stream. Decorates every node with
  its sigil-`Layer` stamp.
- **`clifford-resolve`**: name resolution; `CallContext` tagging per
  Refinement #5b's generalisation; state-reference resolution per Refinement
  #5d; interface-impl coherence per Decision #16.
- **`clifford-types`**: HM inference (§4.8), built-in trait obligations (§4.5),
  nominal access type identity (Decision #19), function-pointer types with
  trait list as part of identity.
- **`clifford-check`**: §5.4 mutability, §5.5 sigil-layer boundary, §5.6
  trait-list verification, §5.7 reference provenance (Decision #13's
  six-rule discipline), §5.8 sigma bounds tracking (Decision #14).
- **`clifford-effect`**: §6 — builds the category C_A per automaton, extracts
  per-effect mutation profiles, builds the effect-procedure call graph,
  computes the interrupt-overlap set R(A) for Refinement #5e atomicity.
- **`clifford-ortho`**: §7 — assigns basis vectors (fields + traits), computes
  behaviour multivectors, runs the wedge-product orthogonality check on every
  concurrent pair. Subject to special quality standards (CLAUDE.md §4).
- **`clifford-codegen`**: §8 — LLVM IR emission. Includes register-block
  volatile lowering (Decision #6), narrow unsafe primitive lowering (Decisions
  #17 + #19), bit-field RMW with target-atomic when needed (Decision #20),
  transition-atomicity wrapping per Refinement #5e.
- **`clifford-stdlib`**: build orchestration for the Clifford stdlib (which is
  itself written in Clifford, not Rust).
- **`clifford-cli`**: thin driver. `cliffordc compile`, `cliffordc test`,
  `cliffordc lint`, `cliffordc audit`, `cliffordc inspect`.

## Where the GA engine fits

The wedge-product check (`clifford-ortho`) runs **after** effect/FSM extraction
(`clifford-effect`) and **before** codegen (`clifford-codegen`). By the time
the GA engine runs, every effect has its `actual_writes` set and every
automaton has its category `C_A`; the engine just builds basis vectors,
computes behaviour multivectors, and verifies pairwise orthogonality.

Per Emergent Rule 6 (DECISIONS.md), the wedge-product check is the
*constructive existence proof* for the product category `C_A × C_B` — not a
clever bitmask trick. Appendix B of the spec states the formal theorem.

## ABI: forward-only dependencies

```toml
# crates/parser/Cargo.toml
[dependencies]
clifford-lexer.workspace = true
clifford-ast.workspace   = true
# Forbidden: clifford-types, clifford-check, clifford-effect, etc.
```

`Cargo.toml` enforces this at the toml level. Adding a backward dependency is
a build failure, not a code-review judgement call.

## What this document is not

- It is **not** a tutorial on the GA orthogonality engine. See `docs/ga-engine.md`
  (to be written; placeholder) and Appendix B of the spec.
- It is **not** the spec. The spec is normative; this document is informative
  and may lag behind.
- It is **not** a roadmap. See `docs/CLIFFORD_SPEC.md` §11.
