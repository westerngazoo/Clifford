# Chapter 26: Decision #26 — Rotor-based plane-confined locks

> **Status:** ✓ LOCKED 2026-05-05; refines Decision #21. Tracking ADR:
> `docs/adr/0005-rotor-plane-confined-locks.md` (Accepted 2026-05-05).
> Implementation gated to **v0.7+** alongside the rest of Decision
> #21's mixed-metric machinery. This chapter is a placeholder; full
> text lands with the v0.7-α implementation PR.

## 26.1 The one-line summary

Decision #21 / ADR 0002 established that locks for shared automaton
state are multivectors `lock(L) = pri(L) + e_L` in a mixed-metric
Clifford algebra, with rotors playing a *tiebreak* role for
same-priority locks. Decision #26 reframes rotors from tiebreak
machinery to the **acquisition primitive itself**.

A `#rotor_lock L` is conceptually a multivector cell `M`. To acquire
`L`, a thread `t` whose signature bivector is `B_t` rotates the cell:

```
M ← R_t · M    where R_t = exp(-θ_t · B_t / 2)
```

Three properties fall out of the algebra for free:

1. **Mutual exclusion.** Cross-plane acquire produces a non-rotor
   multivector → reject.
2. **Wrong-thread release detection.** `R̃_t' · R_t ≠ 1` for `t' ≠ t`
   → reject.
3. **Re-entrancy by the same thread.** `R_t · R_t = R_t(2θ)` is
   still a rotor in plane `B_t`.

## 26.2 Crucial: `exp` cost is zero at runtime

The lowered code is a standard CAS-based spinlock with an integer
owner-ID + depth counter. The GA formulation lives entirely in the
*static analyzer*. Same pattern Decision #21 established: GA is the
proof system, not the runtime.

## 26.3 Locked resolutions (per ADR 0005)

| # | Question | Locked answer |
|---|---|---|
| Q1 | Thread-plane assignment | Pool-based at link time for v0.7 (default `p = 16` shared basis vectors → 8 distinct planes). RTOS dynamic case deferred to v0.8+. |
| Q2 | Re-entrancy | Counted (matches POSIX expectations). |
| Q3 | Same-plane uniqueness | Hard error `E0539 DuplicateThreadPlane`. |
| Q4 | Who carries `θ` for release | Lock owns its full state. |
| Q5 | Relation to Decision #21's priority-ordering | Rotor-as-acquisition supersedes; priority becomes derived total order on planes. |

## 26.4 Diagnostic family

`E0535 PlaneeMismatch`, `E0536 NoThreadPlane`, `E0537
SharedFieldOutsideLock`, `E0538 ReEntryViolation`, `E0539
DuplicateThreadPlane`.

## 26.5 What lands when

- v0.1–v0.6: lexer reservation only (`#rotor_lock`, `#thread_plane`,
  `#guarded_by` join the existing `#shared` / `#lock` / `#with_lock`
  / `#reads` / `#rotor` reservations from Decision #21).
- v0.7+: parser + AST + plane-assignment pass + static
  lock-confinement check (E0535–E0539) + codegen lowering to CAS
  spinlocks with depth counter.

Full chapter — including the worked UART-driver example with full
algebraic trace through every phase, the implementation guide, and
the cross-references — lands with the v0.7-α implementation PR.

See also `DECISIONS.md` Decision #26 and ADR 0005 for the full
design rationale.
