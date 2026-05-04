# Chapter 23: Decision #23 — Tighten `@fn` toward Haskell-clean

> **Status:** DESIGN-IN-PROGRESS. ADR forthcoming:
> `docs/adr/0003-haskell-clean-fn-discipline.md`. This chapter is a
> placeholder; full content lands once the ADR concludes.

## 23.1 The direction

Make `@fn` as close to a Haskell-clean function as practical:

- **Total by default.** `@fn` bodies must terminate; non-termination
  is a typing error rather than a runtime concern. The escape valve
  for unbounded loops is `@partial @fn` (TBD).
- **Effect rows in signatures.** Bring effect tracking into the
  `@fn` signature à la Koka, so the pure-side type tells you not only
  "what does this return" but "what does it touch."
- **Refinement types in argument positions.** Liquid-Haskell-style
  refinements on parameter types (e.g., `n: u32 { n > 0 }`) for
  pre-condition encoding.
- **Local-stack mutation per Refinement #1a remains permitted.** The
  ST-monad-equivalent: a `@fn` may freely allocate a local stack
  variable and mutate it; what it cannot do is touch automaton state.

## 23.2 Why the ADR

The design decisions here are not mechanical. The ADR will survey:

- Idris totality checking and how it integrates with HM inference.
- Liquid Haskell refinement types and the SMT backend question
  (do we want one in v0.2? v0.7? never?).
- Koka effect rows and how they interact with `$ [TraitList]`
  pure-side traits (Decision #2).
- The local-mutation discipline already locked in Refinement #1a.

## 23.3 What lands when

- v0.2 (likely): the effect-row pieces; `$ [TraitList]` extension.
- v0.4+: totality checking; refinement types if SMT backend is
  available.

Full text lands once ADR 0003 closes. See `DECISIONS.md` Decision #23
for the locked direction.
