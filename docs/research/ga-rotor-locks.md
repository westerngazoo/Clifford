# RESEARCH (deferred): Rotor-Based Plane-Confined Locks

> **Deferred from `DECISIONS.md` Decision #26 by the 2026-05 audit.**
> Preserved verbatim for the historical record. **Not** normative, **not**
> a v1.0 commitment. Refines the also-deferred `ga-shared-automata.md`
> (Decision #21). See `docs/research/README.md` and `docs/foundations.md`.
>
> Tracking ADR (immutable): `docs/adr/0005-rotor-plane-confined-locks.md`.

---

## Original text (Decision #26, locked 2026-05-05, deferred 2026-05-16)

**Refines:** Decision #21 (shared automata via mutator multivectors).

### Summary

Decision #21 / ADR 0002 already established that locks are multivectors `lock(L) = pri(L) + e_L` in a mixed-metric Clifford algebra, with rotors playing a *tiebreak* role for same-priority locks. Decision #26 reframed rotors from tiebreak machinery to the **acquisition primitive itself**.

A `#rotor_lock L` is conceptually a multivector cell `M`. Initially `M = 1` (scalar identity, "unlocked"). To acquire `L`, a thread `t` whose signature bivector is `B_t` rotates the cell: `M ← R_t · M` where `R_t = exp(-θ_t · B_t / 2)`.

Three properties were claimed to fall out of the algebra for free:

1. **Mutual exclusion.** Cross-plane acquire produces a non-rotor multivector (odd-grade components) → reject.
2. **Wrong-thread release detection.** `R̃_t' · R_t ≠ 1` for `t' ≠ t` → reject.
3. **Re-entrancy by the same thread.** `R_t · R_t = exp(-2θ_t · B_t / 2)` is still a rotor in plane `B_t`.

The static-analysis check is the same wedge-product the orthogonality engine already runs (`caller.thread_plane ∧ lock.plane`). Runtime cost is a normal CAS-based spinlock with an integer owner-ID — `exp` does not appear in generated code.

### Locked resolutions (per ADR 0005)

- **Q1 Thread-plane assignment:** Pool-based at link time for v0.7 (default `p = 16` shared basis vectors → 8 distinct planes). RTOS dynamic case deferred.
- **Q2 Re-entrancy:** Counted; lock owns owner-ID + depth counter at runtime.
- **Q3 Same-plane uniqueness:** Hard error `E0539 DuplicateThreadPlane`.
- **Q4 Who carries θ for release:** Lock owns its full state; thread checks "am I owner?".
- **Q5 Relation to #21's priority-ordering proof:** Rotor-as-acquisition supersedes; priority becomes the canonical strict total order on planes.

### Scaffolding (none was ever added)

Decision #26 stated `#rotor_lock`/`#thread_plane`/`#guarded_by` tokens would join Decision #21's lexer reservations "when the v0.7 milestone opens." They were never added to the lexer, so there is nothing to remove.

### Why it was deferred

`docs/decision-audit-2026-05.md` §5: the decision's own text admits "`exp` does not appear in generated code; runtime cost is a normal CAS-based spinlock with an integer owner-ID." That admission is the diagnosis — the rotor algebra is decoration over a CAS spinlock with an owner-ID. Rotor-as-acquisition is the exact novelty-bait pattern the post-pivot project rejects. The mutual-exclusion / wrong-thread-release / re-entrancy properties it sought are standard and well-served by ordinary lock implementations with owner-IDs.
