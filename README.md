# Clifford — `cliffordc`

> A general-purpose systems language whose unifying claim is **statically-proven
> concurrency safety via Geometric Algebra**. C-direct imperative layer,
> Haskell-clean functional layer, separated by sigils. Embedded firmware is the
> canonical first target; the language is not embedded-only.

**Status:** Pre-implementation. Spec at `docs/CLIFFORD_SPEC.md` v0.5.0-draft.
Design decisions locked in `docs/DECISIONS.md` (#1–#19).

## What this is

Clifford is the language. `cliffordc` is its compiler — written in Rust, targets
LLVM IR, no runtime, no garbage collector. The novel piece is the GA orthogonality
engine (§7 of the spec) that proves concurrency safety as a wedge product in a
graded algebra.

If you came expecting "safer Rust," you came expecting wrong: Clifford makes
different commitments (sigil layering, automaton-as-state-owner, body-scoped
references with no lifetime annotations, sigma loops, nominal access types).
See the spec for the architecture and `docs/DECISIONS.md` for the rationale.

## The Safety Triangle

Clifford's three layers, each with its own discipline, meeting at a shared GA proof:

```
                   Functional (@)
                  Pure transformation;
                 no time, no state.
                       ▲
                      ╱ ╲
                     ╱   ╲
            $ [Pure] /     \ Snapshots,
            traits  /       \ value returns
                   ╱         ╲
                  ╱  GA proof ╲       ← where the three meet:
                 ╱   wedges    ╲        every effect's behaviour
                ╱   non-zero    ╲       multivector wedges to
               ╱_________________╲      a non-zero blade of full
              ╱                   ╲     grade against every other
   Imperative (#)               Hardware (access<T>)
  States and effects;           Physical reality;
  #automaton, #effect,          register blocks,
  #transition, #mutate.         narrow unsafe primitives.
```

- **`@` Functional:** pure transformation. Default `$ [Pure]`, default-immutable,
  Haskell-clean. No time, no state, no I/O. Can call other `@`-layer code only.
- **`#` Imperative:** state and effects in time. `#automaton`, `#effect`,
  `#transition`, `#mutate`. Can call `@` freely (downward); cannot be called
  from `@` (upward boundary enforced).
- **`access<T>` Hardware:** nominal pointer types backed by physical memory
  layouts. Accessed via narrow unsafe primitives (`#unchecked_load`,
  `#volatile_store`, `#unchecked_offset`, …) — each its own grep target.

The GA orthogonality engine is the *theorem* that makes the three commit
together: every concurrent computation pair must wedge to a non-zero blade of
full grade, which is exactly the well-formedness condition for the product
category `C_A × C_B`. See `docs/CLIFFORD_SPEC.md` Appendix B for the formal
statement.

## Build

```sh
# Toolchain is pinned in rust-toolchain.toml; rustup will fetch it on first build.
cargo build
cargo test
cargo clippy --all-targets -- -D warnings
cargo fmt -- --check
```

The `cliffordc` binary will live in `crates/cli` once Phase 5 is complete.

## Repository layout

See `CLAUDE.md` §2 for the full layout. Quick orientation:

```
.
├── CLAUDE.md            ← engineering charter; read before any PR
├── docs/
│   ├── CLIFFORD_SPEC.md ← the normative spec (1,600+ lines, 19 decisions)
│   └── DECISIONS.md     ← locked design decisions with rationale
├── crates/
│   ├── lexer/   parser/   ast/       ← Phase 0
│   ├── resolve/ types/    check/     ← Phase 1
│   ├── effect/                       ← Phase 2
│   ├── ortho/                        ← Phase 3 — the GA engine; the heart
│   ├── codegen/                      ← Phase 4 — LLVM IR via inkwell
│   ├── stdlib/                       ← Phase 5
│   └── cli/                          ← Phase 5 driver
├── tests/   ← conformance tests, organized by phase
├── benches/ ← criterion.rs perf benchmarks
└── examples/← example .cl programs
```

Inter-crate dependencies flow forward only:
`lexer → parser → ast → resolve → types → check → effect → ortho → codegen`.
No backward edges. Each crate is independently buildable.

## Quality bar

PhD-level. See `CLAUDE.md` §1.3 for the specifics, but the headline rules:

- Correctness over cleverness.
- The spec is the contract; PRs cite §-numbers.
- Tests are part of the deliverable. The `ortho` crate (§7 GA engine) requires
  100% line + branch coverage and property tests.
- No `unsafe` outside `crates/codegen` and `crates/stdlib`.
- No `unwrap()` in non-test code.

## Releases

- **v0.1**: compiles Appendix A examples to a Cortex-M target; smoke-tested in
  QEMU; ships a non-firmware reference application demonstrating general-purpose
  use.
- **v0.2**: stdlib feature-complete (allocators, automaton-based event loops,
  IO primitives); Decision #12 (`#staged`) and Decision #18 (`#audit`) land.
- **v1.0**: spec frozen.

## License

Dual-licensed MIT OR Apache-2.0. See `LICENSE-MIT` and `LICENSE-APACHE`. By
opening a PR, the contributor agrees to license their contribution under the
project's terms.

The spec (`docs/CLIFFORD_SPEC.md`) is licensed CC-BY-SA 4.0 — anyone can use it
to build their own implementation, with attribution.

## Contact

Maintainer: Goose (Gustavo Delgadillo) — `gustavo.delgadillo@gmail.com`.
