# Glossary

> **Status:** Stub. Full chapter pending.

**Coming in book v0.2.** A definitions-only reference for Clifford-specific terminology.

Terms to cover (alphabetical, partial list):

- **Automaton** — a `#automaton` declaration; a small category in the control-flow sense (Decision #5) with state-tags as objects and `#transition`s as morphisms.
- **Basis vector** — a single `e_q` contributing one dimension to the GA orthogonality engine''s behavior multivector. Auto-assigned per field unless `#basis: name` overrides (Decision #4).
- **Behavior multivector** — the wedge of all basis vectors an effect/automaton writes; the input to the §7.4 orthogonality check.
- **CallContext** — Resolver tag on `#> proc()` calls per Refinement #5b: Identity / Transition / Generic.
- **Decision number** — entry number in `docs/DECISIONS.md`; references like *Decision #21* are stable across spec versions.
- **Functional layer** — the `@`-prefixed half of the language (Decision #1): `@fn`, `@type`, `@trait`, etc.
- **Imperative layer** — the `#`-prefixed half: `#automaton`, `#effect`, `#interrupt`, `#transition`, etc.
- **Lock-context multivector** — the wedge of every held lock at a program point (Decision #21, §5.5 of ADR 0002).
- **Mutator multivector** — alternate name for an effect/transition viewed as a Kleisli arrow over State (Ch. 25 framing).
- **Orthogonality check** — the §7.4 wedge-product non-zero test for concurrent-safety.
- **Rotor** — a unit GA element used for same-priority lock disambiguation (Decision #21).
- **Sigil** — `@` or `#`, the layer-marker prefix on identifiers (Decision #1).
- **Span** — `(byte_start, byte_end)` source-position range carried on every AST node.

(Full glossary forthcoming.)

