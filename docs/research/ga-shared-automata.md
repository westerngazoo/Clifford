# RESEARCH (deferred): Shared Automata via Mutator Multivectors

> **Deferred from `DECISIONS.md` Decision #21 by the 2026-05 audit.**
> This is preserved verbatim for the historical record. It is **not**
> normative and **not** a v1.0 commitment. See `docs/research/README.md`
> for why, and `docs/foundations.md` §3–§4 for the replacement direction
> (Stack Resource Policy + Pony-`iso`-style `#owned`/`#sendable`).
>
> Tracking ADR (immutable): `docs/adr/0002-shared-automata-mutator-multivectors.md`.

---

## Original text (Decision #21, locked 2026-05-01, deferred 2026-05-16)

**Spec impact (as originally written):** §7 (Orthogonality Engine — adds §7.0 prologue and §7.9 mixed-metric extension), §2 (reserves new sigil-prefixed forms), §4 (Type System — `#shared` field qualifier).

### Summary

The current GA orthogonality engine works in a Clifford algebra Cl(0,0,n) — every basis vector squares to zero, which is why `a & b != 0 ⇒ wedge == 0` detects write-write races. This is mathematically clean for *disjoint-mutation* programs but cannot model *shared mutable state* that real kernels (Wari, seL4, Hubris, Linux) deliberately require — run-queues, capability tables, page allocators, IRQ binding tables.

Decision #21 extends the engine to a mixed-metric Cl(p,0,n) algebra where:

- **Private fields** (the v0.1 default) contribute *null* basis vectors. Their wedge collapses on overlap — current race-detection behavior, unchanged.
- **Shared fields** (declared `#shared`) contribute *non-null* basis vectors. Their wedge does *not* collapse on overlap; instead, overlap discharges a separate proof obligation: the lock guarding the shared resource must be held by both concurrent contexts.

The locking discipline is itself algebraic, not procedural:

- Each lock is a mixed-grade multivector `lock(L) = pri(L) + e_L` (scalar priority + identity basis vector).
- The lock-context multivector held by an executing automaton is the wedge of every held lock.
- Acquisition validity falls out of the wedge product:
  - Ascending priority → canonical wedge
  - Descending priority → Koszul-flippable
  - Equal priority → resolved by a GA *rotor* parameterised by a canonical structural attribute (MMIO `#address` for register-block locks; link-section position / source-location hash / explicit `#rotor:` clause for software locks).
- **Theorem (sketched):** the lock context never collapses to zero ⟺ execution is deadlock-free. Lock-ordering safety falls out of the algebra; no separate procedural checker.
- **Interrupts and locks unify:** an `#interrupt #priority: N { … }` is a priority-ordered acquisition under the algebra.

### What this Decision unified

1. **Disjoint-mutation safety** — null-subspace wedge non-zero (current §7.4 check).
2. **Shared-state safety** — non-null subspace overlap discharges the lock-coverage proof obligation.
3. **Deadlock-freedom** — lock-context multivector never collapses.
4. **Interrupt/lock unification** — interrupts are priority-ordered acquisitions; algebra handles both.

### Phase-1 scaffolding (now removed from the live tree)

The original decision landed the following scaffolding "to lock the design direction." All of it was **removed by the 2026-05 audit** when the decision was deferred:

- `crates/ast` `FieldKind` enum on `AutomatonField` (`#[non_exhaustive]`, one variant `Private`).
- `crates/lexer` reservations of `#shared`, `#lock`, `#with_lock`, `#reads`, `#rotor`.
- `CLIFFORD_SPEC.md` §7.0 / §7.9 prologue + extension sketch (handled by the §7 rewrite).

### Why it was deferred

`docs/decision-audit-2026-05.md` §5 and `docs/foundations.md` §3–§4: the mixed-metric algebra was unimplemented, gated to v0.7, and assumed a categorical correctness proof that did not exist in writing. The real problem — shared mutable resources — is better served by Stack Resource Policy (Baker 1991, as mechanized by RTIC, with 35 years of certification use) plus a minimal `#owned`/`#sendable` field qualifier inspired by Pony's `iso` reference capability. Both have published soundness results; neither needs an algebra. A fresh decision will cover SRP-based shared resources once a comparison artifact validates the need.
