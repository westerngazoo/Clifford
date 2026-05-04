# Chapter 22: Decision #22 — Kinds of imperative

> **Status:** Design locked 2026-05-03. Implementation slated for v0.2.
> This chapter is a placeholder; full content lands with the
> implementation PR.

## 22.1 The one-line summary

Extend the `$ [TraitList]` mechanism (Decision #2 — pure-side trait
markers) to also mark `#effect`, `#interrupt`, and `#transition`
declarations with predeclared imperative-side traits. The traits
classify *what kind of imperative work* the callable does, without
extending the orthogonality engine. They feed `cliffordc audit`,
codegen (memory-ordering decisions), and certification artefacts.

## 22.2 The trait list (locked)

| Trait               | Meaning                                                                                                            |
|---------------------|--------------------------------------------------------------------------------------------------------------------|
| `Hardware`          | Mutates memory-mapped registers (typically `#mutates` a register-block automaton per Decision #6)                  |
| `Realtime`          | Has a stated worst-case execution time bound; permitted in real-time scheduling decisions                          |
| `Acquire`           | Carries acquire memory ordering (per `std::sync::atomic::Ordering::Acquire` semantics)                             |
| `Release`           | Carries release memory ordering                                                                                    |
| `SeqCst`            | Carries sequential consistency (the strongest ordering)                                                            |
| `LockingDiscipline` | Manipulates a `#shared` field's lock per Decision #21 (v0.7+)                                                      |
| `PureState`         | Mutates only its own automaton's private state (no externally-visible side effects on other automata)              |
| `Encapsulated`      | Mutates only `#hidden`-marked fields per Decision #25 (effectively no externally-visible side effect on any state) |

## 22.3 Why this is locked, not ADR-required

The set of traits is *prescriptive*, not derived; the engine ignores
them; the cost is purely additive (one new clause grammar + storage on
existing AST nodes + audit-tool plumbing). No design uncertainty.

## 22.4 What lands when

- v0.1: lexer recognises `Hardware`, `Realtime`, etc. as predeclared
  trait names; parser accepts `$ [TraitList]` on `#effect` /
  `#interrupt` / `#transition`; AST stores the list.
- v0.2: `cliffordc audit --traits` and codegen consumers (memory
  ordering for `Acquire` / `Release` / `SeqCst`; `Realtime` consumers
  in worst-case-execution-time tooling).

Full text lands with the v0.2 implementation PR. See `DECISIONS.md`
Decision #22 for the locked design.
