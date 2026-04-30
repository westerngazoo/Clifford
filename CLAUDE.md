# CLAUDE.md — Clifford Compiler Project

> Engineering charter, conventions, and quality standards for any agent (human or AI) contributing to the Clifford compiler.
>
> Read this **before** touching any code. Re-read §3 before every PR.

---

## 0. Project Identity

**Clifford** is a general-purpose systems language whose unifying claim is statically-proven concurrency safety via Geometric Algebra. It combines a C-compatible foundation with default immutability, a functional core (Haskell-clean), and an automaton-based imperative layer (C-direct) whose concurrency safety is verified by a GA orthogonality engine in the compiler.

**Embedded firmware is the canonical first target** because the safety properties matter most there and because the constraints (no GC, no runtime, deterministic timing) discipline the design. The language is *not* embedded-only — the same constructs work for servers, robotics, scientific computing, game engines, and anywhere else the safety claims are valuable. Targeting beyond firmware happens through Phase 5 stdlib (allocators, automaton-based event loops for I/O) without language-level changes.

**The compiler is called `cliffordc`.** Written in Rust. Targets LLVM IR. No runtime.

This is not a toy. It is a research-grade language project with the engineering rigor of a production compiler. We are aiming for **publishable work** (the GA orthogonality engine is genuinely novel) and **deployable systems software** starting with firmware. Both standards apply simultaneously.

**Reference documents (in priority order):**
1. `docs/CLIFFORD_SPEC.md` — the normative technical specification. Source of truth.
2. `docs/DECISIONS.md` — locked design decisions with rationale.
3. `docs/clifford_spec_draft0.docx` — early design rationale and worked examples. Background only.
4. This file (`CLAUDE.md`) — engineering conventions for contributors.

If a contributor finds a contradiction between this file and `docs/CLIFFORD_SPEC.md`, the spec wins. Where the spec and `docs/DECISIONS.md` disagree, `DECISIONS.md` wins until reconciled into the spec. Open an issue to reconcile.

---

## 1. Engineering Philosophy

### 1.1 Five Principles, Non-Negotiable

1. **Correctness over cleverness.** A boring algorithm with a written proof beats a clever one with a hunch. If you cannot explain why the code is correct in three sentences, the code is not ready.
2. **The spec is the contract.** Code that does not implement a spec section, or implements it differently, is a bug. If the spec is wrong, fix the spec first, then the code.
3. **Tests are part of the deliverable.** A feature without conformance tests is not done. A test that passes by accident is worse than no test.
4. **Small surfaces, deep modules.** Each crate exposes the minimum public API. Internal complexity is acceptable; leaked complexity is not.
5. **No shortcuts in the proof engine.** §7 of the spec — the GA orthogonality engine — is the part most likely to be wrong and most consequential when wrong. Hold it to the highest standard. Every check needs a unit test that exhibits the failure mode it prevents.

### 1.2 Anti-Principles (things we deliberately reject)

- **"Make it work, then make it right."** No. Make it right the first time, or don't merge.
- **"We can refactor later."** Architectural debt accumulates exponentially in compilers. Refactor before merging.
- **"It's just a prototype."** This is the prototype. It is also the production code. Treat it accordingly.
- **"Add a TODO."** TODOs are issues. File the issue, link it from the comment, or remove the comment.

### 1.3 The PhD-Level Bar

When we say "world-class," we mean concretely:

- Every algorithm in the compiler has a literature reference, an in-tree explanation, or both.
- Every error message gives the user enough context to fix the problem without reading the compiler source.
- Every public API has rustdoc with at least one example.
- Every commit message explains *why*, not just *what*.
- Every PR description states which spec section it implements and which test demonstrates it.
- Every benchmark has a baseline and a regression threshold.

This bar is not optional. It is what separates a compiler that ships from one that withers.

---

## 2. Repository Layout

```
cliffordc/
├── CLAUDE.md                  ← this file
├── CLIFFORD_SPEC.md             ← normative specification
├── README.md                  ← user-facing intro, build instructions
├── CHANGELOG.md               ← keep-a-changelog format
├── LICENSE                    ← MIT/Apache-2.0 dual
├── Cargo.toml                 ← workspace root
├── rust-toolchain.toml        ← pinned toolchain
├── .github/
│   ├── workflows/             ← CI pipelines
│   ├── ISSUE_TEMPLATE/
│   └── PULL_REQUEST_TEMPLATE.md
├── docs/
│   ├── architecture.md        ← compiler internals overview
│   ├── ga-engine.md           ← deep dive into §7
│   ├── adr/                   ← architecture decision records
│   └── papers/                ← references, our own writeups
├── crates/
│   ├── lexer/                 ← Phase 0
│   ├── parser/                ← Phase 0
│   ├── ast/                   ← shared AST types
│   ├── resolve/               ← name resolution
│   ├── types/                 ← Phase 1: HM type checker
│   ├── check/                 ← Phase 1: mutability + trait checks
│   ├── effect/                ← Phase 2: FSM extraction
│   ├── ortho/                 ← Phase 3: GA orthogonality engine
│   ├── codegen/               ← Phase 4: LLVM IR
│   ├── stdlib/                ← Phase 5: clifford::core, clifford::alloc, ...
│   └── cli/                   ← Phase 5: cliffordc binary
├── tests/
│   ├── lex/
│   ├── parse/
│   ├── typecheck/
│   ├── effect/
│   ├── ortho/
│   ├── codegen/
│   └── runtime/               ← QEMU integration tests
├── benches/                   ← criterion.rs benchmarks
└── examples/                  ← example .fe programs
```

**Each crate is independently buildable.** Inter-crate dependencies flow only forward in the pipeline: `lexer → parser → ast → resolve → types → check → effect → ortho → codegen`. No backward edges. The compiler driver in `crates/cli` is the only place that wires them together.

---

## 3. Code Quality Standards

### 3.1 Rust Style

- **Edition:** 2021 (move to 2024 when stable).
- **Toolchain:** pinned in `rust-toolchain.toml`. Never `rustup update` on CI without bumping the pin.
- **Formatter:** `rustfmt` with default settings. Run on save. CI enforces.
- **Linter:** `clippy` with `-D warnings` on CI. Allow lints only with a justifying comment:
  ```rust
  #[allow(clippy::too_many_arguments)] // FSM transition signature is wide by design; see §6.2
  ```
- **No `unsafe`** outside `crates/codegen` and `crates/stdlib`. Unsafe in those crates requires a `// SAFETY:` comment proving the invariants.
- **No `unwrap()`** in non-test code. Use `.expect("...")` with a descriptive message, or propagate with `?`.
- **No `panic!()`** outside the compiler driver's top-level error reporter. Internal errors are `Result`s.

### 3.2 Naming

| Kind | Convention | Example |
|---|---|---|
| Crates | snake_case, short | `ortho`, `effect` |
| Modules | snake_case | `state_graph` |
| Types, traits | UpperCamelCase | `BehaviorMultivector`, `Automaton` |
| Functions, methods | snake_case | `outer_product`, `extract_fsm` |
| Constants | SCREAMING_SNAKE_CASE | `MAX_BASIS_VECTORS` |
| Lifetimes | short lowercase, meaningful | `'src`, `'ast`, not `'a` |
| Type parameters | UpperCamelCase, meaningful | `T`, `Item`, `State` |

**Avoid abbreviations** unless they are domain-standard (`ast`, `ir`, `llvm`, `fsm`, `ga`). When in doubt, write it out.

### 3.3 Documentation

Every public item gets rustdoc. Format:

```rust
/// One-line summary in active voice.
///
/// Longer description if needed. Explain *why* this exists, not just what it does.
/// Reference the spec section: implements §7.4 (Orthogonality Check).
///
/// # Examples
///
/// ```
/// use clifford_ortho::{Blade, outer_product};
/// let a = Blade::from_indices(&[0, 1]);
/// let b = Blade::from_indices(&[2, 3]);
/// assert!(outer_product(a, b).is_some());
/// ```
///
/// # Errors
///
/// Returns `None` if the blades share any basis vector.
///
/// # Panics
///
/// Never panics. (Or: panics if `index >= MAX_BASIS_VECTORS`; see §7.1.)
pub fn outer_product(a: Blade, b: Blade) -> Option<Blade> { ... }
```

Crate-level docs (`//!` at top of `lib.rs`) state which spec sections the crate implements and link to them.

### 3.4 Errors

- Use `thiserror` for library crates, `anyhow` only in the CLI driver.
- Every error type has a stable error code (e.g., `E0042`) for machine consumption.
- Error messages follow the rustc convention: location, error, label, note, help.
- Error messages cite the spec section that the error implements (e.g., "§7.4: orthogonality violation").

### 3.5 Testing

- **Unit tests** live in the same file as the code (`#[cfg(test)] mod tests`). They test small invariants.
- **Integration tests** live in `tests/` at the workspace level. They test phase boundaries.
- **Conformance tests** live in `tests/<phase>/` and follow the layout in spec §10.
- **Property tests** (via `proptest`) for the GA engine and for parser fuzzing. Required for ortho.
- **Snapshot tests** (via `insta`) for AST shape, IR output, error messages. Approved snapshots are checked in.
- **Coverage:** track via `cargo-llvm-cov`. The orthogonality engine must be at 100% line and branch coverage. Other crates: 85% minimum.

---

## 4. The GA Orthogonality Engine — Special Standards

The `ortho` crate is the heart of Clifford. It is the part that, if wrong, produces silent miscompilation of safety-critical embedded code. We treat it accordingly.

### 4.1 Mandatory Practices for `ortho`

- **Every public function has a property test.** Not just example-based tests — properties.
- **Every algorithm cites its source.** Bitmask blade representation: cite Dorst & Mann if used; cite garust if vendored.
- **Every error message names the conflicting basis vectors by their original variable names**, not by index. The user never sees `e_5`; they see `rx_head`.
- **Every transformation preserves a documented invariant.** State the invariant in a comment, then test it.
- **No "optimization" without a benchmark.** The XOR-bitmask representation is already O(1); resist the urge to be clever.

### 4.2 Code Review Checklist for `ortho` PRs

- [ ] Does the change preserve `behavior(A) ∧ behavior(B) ≠ 0 ⇔ A and B disjoint`?
- [ ] Is there a property test that would have caught the bug being fixed?
- [ ] Does the error message tell a non-mathematician what to do?
- [ ] Is there a worked example in `tests/ortho/` exhibiting the new behavior?
- [ ] Does coverage remain at 100%?
- [ ] Has the spec been updated if semantics changed?

### 4.3 The Garust Question

**Decision deferred** — see spec §7.7. Until decided, the `ortho` crate has its own minimal in-tree blade arithmetic. Do not vendor garust on a whim. If a contributor proposes vendoring, file an ADR (`docs/adr/NNNN-vendor-garust.md`) and discuss.

---

## 5. Workflow

### 5.1 Branch Model

- `main` is always green and always releasable.
- Feature branches: `feat/<short-slug>`, e.g., `feat/lexer-numeric-literals`.
- Fix branches: `fix/<short-slug>`.
- Spec branches: `spec/<section>`, used for spec-only changes.
- No long-lived feature branches. If a branch lives longer than a week, it should be broken into smaller PRs.

### 5.2 Commits

- Atomic. One logical change per commit.
- Conventional Commits format:
  ```
  feat(lexer): add hex literal support

  Implements §1.2 of CLIFFORD_SPEC.md. Supports underscored separators
  (0xDEAD_BEEF) and explicit type suffixes.

  Tests: tests/lex/hex_literals.fe
  ```
- The commit body explains *why*. The diff explains *what*.
- Every commit passes CI in isolation. No "fix typo" commits — squash before merging.

### 5.3 Pull Requests

PR description template:

```markdown
## What this implements

§<section> of CLIFFORD_SPEC.md.

## How to verify

- `cargo test -p <crate>`
- New test: `tests/<phase>/<file>.fe`

## Spec changes

None. (Or: updated §X to clarify Y.)

## Open questions

(List, or "None.")
```

- Minimum one reviewer for non-trivial PRs.
- The `ortho` crate requires two reviewers. Both must verify the GA invariants.
- Squash-merge into `main`. Preserve the PR title as the merge commit subject.

### 5.4 Architecture Decision Records (ADRs)

Significant decisions are recorded in `docs/adr/` using the [Michael Nygard format](https://github.com/joelparkerhenderson/architecture-decision-record/blob/main/locales/en/templates/decision-record-template-by-michael-nygard/index.md):

```
docs/adr/0001-rust-as-implementation-language.md
docs/adr/0002-llvm-via-inkwell.md
docs/adr/0003-bitmask-blade-representation.md
```

ADRs are immutable once accepted. Superseded ADRs reference the new ADR.

### 5.5 Spec Changes

Changes to `CLIFFORD_SPEC.md` are PRs like any other, but:

- Tagged with `spec` label.
- Require sign-off from the project maintainer (currently: Goose).
- Bump the spec version in the document header.
- Add a `CHANGELOG.md` entry under "Spec Changes".
- May not be merged in the same PR as code changes that depend on them. Spec change goes first; code change references it.

---

## 6. Engineering Discipline by Phase

Each phase has its own quality emphasis. The bar everywhere is high; these are the *additional* concerns per phase.

### Phase 0 — Lexer / Parser
- Determinism is paramount. Same input → same AST, byte for byte.
- Error recovery: the parser produces a partial AST and reports all errors, not just the first.
- Span tracking: every AST node carries a source span (`(file, byte_start, byte_end)`).
- Property test: round-trip `source → AST → pretty-print → AST` is identity (modulo whitespace).

### Phase 1 — Type System
- Inference is principled. No ad-hoc rules; every rule corresponds to an HM judgment.
- Trait satisfaction is structural and decidable. No coherence games.
- Mutability check is a distinct pass; do not interleave it with inference.

### Phase 2 — Effect & FSM
- The state graph is a first-class data structure exposed in the typed AST.
- Every effect's mutation profile is verified against its body (read profile too).
- Reachability uses standard Tarjan / Kosaraju; cite the source.

### Phase 3 — GA Engine
- See §4 above. Treat as the highest-risk component.

### Phase 4 — Codegen
- IR is generated from the typed AST in a single pass.
- Optimizations are LLVM's job, not ours. We emit clean IR; LLVM optimizes.
- ABI compatibility verified by linking against hand-written C and round-tripping data.

### Phase 5 — Stdlib + Tooling
- Stdlib is written in Clifford, not Rust. It is the first dogfooding of the language.
- The CLI driver is thin. Real logic lives in the library crates.
- Diagnostics use `codespan-reporting` or `ariadne` for nice rendering.

---

## 7. Performance & Benchmarking

We do not optimize prematurely. We do measure, always.

- `criterion.rs` benchmarks live in `benches/`.
- Every phase has at least one bench: `bench_lex_large_file`, `bench_typecheck_generic_heavy`, `bench_ortho_50_automata`.
- CI runs benchmarks on every PR and posts a regression comment if any bench regresses by more than 5%.
- Performance regressions ≥ 20% block merge unless justified in the PR description.
- The compiler should compile a 10kLoC Clifford program in under 5 seconds in release mode on a 2024-era laptop. This is the baseline we hold.

---

## 8. Working with AI Agents

Many contributions to this project will come from AI agents. This is fine — but the standards do not relax. The following expectations apply:

### 8.1 Agent Operating Principles

1. **Read before writing.** Before any change, the agent reads:
   - This file (`CLAUDE.md`)
   - The relevant section of `CLIFFORD_SPEC.md`
   - The crate's `lib.rs` rustdoc
   - At least one existing similar function in the same crate
2. **Cite the spec.** Every PR from an agent must reference the spec section being implemented. "I implemented §3.2 of the spec" is good. "I added parsing" is not.
3. **Prefer small PRs.** An agent PR should touch one crate, implement one section, and add one set of tests. Resist the urge to "while I'm here, also fix..."
4. **Surface uncertainty.** If the spec has an `[OPEN]` marker on the relevant section, the agent stops and asks. It does not invent an answer.
5. **No silent dependencies.** Adding a new crate to `Cargo.toml` requires justification in the PR description and reviewer approval. Default answer is "no, write it yourself."

### 8.2 Things Agents Get Wrong (anti-patterns)

These are the specific failures we have seen and watch for:

- **Inventing syntax.** Clifford syntax is what the spec says. If something feels missing, file a spec issue, don't extend the parser unilaterally.
- **Stubbing instead of implementing.** A function with `todo!()` is not a contribution. Either implement it or don't open the PR.
- **Plausible but wrong error messages.** Error messages must be tested. An untested error message is a future user-facing bug.
- **Skipping property tests on `ortho`.** Non-negotiable. If the agent is touching `ortho` and cannot write a property test, it should not be touching `ortho`.
- **"Refactoring" while implementing a feature.** No. One PR, one purpose. File the refactor as a separate PR.
- **Ignoring `[OPEN]` markers.** These are explicit signals to stop and ask. Treat them as compile errors in the spec.

### 8.3 What Agents Should Do

- When given a task like "implement §3.2", the agent reads §3.2, the surrounding sections, the existing parser code, and the relevant tests, *then* writes a plan, *then* implements. The plan is shared in the PR description.
- The agent reports test coverage with every PR.
- The agent runs the full test suite locally (or in its sandbox) before opening the PR.
- The agent updates `CHANGELOG.md` in every user-visible PR.
- The agent does not mark a PR ready for review until CI is green.

### 8.4 Reviewing Agent PRs

The reviewer (human or another agent) verifies:

- [ ] Spec section is correctly cited and accurately implemented.
- [ ] Tests cover the happy path and at least one failure mode.
- [ ] No new `unsafe`, no new dependencies, no new `unwrap()` without justification.
- [ ] Error messages are clear to a non-implementer.
- [ ] Documentation is present and accurate.
- [ ] No drift from the project's existing style.

If any item fails, the PR is sent back. The reviewer does not fix it themselves — that breaks the feedback loop the agent needs.

---

## 9. Communication

### 9.1 Issues

- Bug: reproducible failure. Include the input, the expected output, the actual output.
- Feature: an enhancement aligned with the spec. If not in the spec, it is a spec issue first.
- Spec issue: a question, contradiction, or `[OPEN]` resolution.
- Discussion: open-ended. Use sparingly.

### 9.2 Status

The project's source of truth for "what is done" is the test suite, not a roadmap document. If the test exists and passes, the feature is done. Otherwise, it isn't.

### 9.3 Tone

Professional, direct, brief. Reviewers point out problems plainly. Authors fix them or push back with reasoning. We do not soften technical judgments to spare feelings, and we do not make technical judgments personal.

---

## 10. Releases

- Versioning: SemVer. Pre-1.0 means breaking changes between minor versions.
- v0.1: compiles Appendix A examples to a Cortex-M target (the firmware proving ground). Smoke-tested in QEMU. Also: a non-firmware example (e.g., a small CLI tool or a numerical kernel) to demonstrate the language is not embedded-only.
- v0.2: stdlib feature-complete (allocators, automaton-based event loops, IO primitives); supports a published reference firmware on real hardware (StarFive VisionFive 2 or STM32 Nucleo) *and* a non-firmware reference application demonstrating general-purpose use.
- v1.0: spec frozen, no breaking changes after this point without a v2 fork.

Each release ships with:
- A signed git tag (`v0.1.0`)
- A GitHub release with binaries for Linux/macOS/Windows hosts
- A blog post on `the-goose-factor.netlify.app` describing the release
- A `CHANGELOG.md` entry
- A bumped spec version if applicable

---

## 11. License & Attribution

- Code: dual-licensed MIT / Apache-2.0.
- Spec: CC-BY-SA 4.0 — anyone can use the spec to build their own implementation, with attribution.
- Contributions: by opening a PR, the contributor agrees to license under the project's terms. No CLA.
- Citations: when academic work cites Clifford, request the citation include the spec version and the relevant section number.

---

## 12. Final Word

We are building a language and a compiler that did not exist before. The Geometric Algebra orthogonality engine, in particular, is novel work — there is no prior art to copy from. This is hard, and that is the reason to do it well.

The discipline in this document is not bureaucracy. It is what allows a small team (or a small team plus agents) to produce a compiler that someone, somewhere, will use to ship a medical device or a satellite controller. That is the standard. Hold it.

When in doubt: **read the spec, write the test, then write the code.**

---

*Document version 0.1.0. Last revised alongside `CLIFFORD_SPEC.md` v0.1.0-draft.*
