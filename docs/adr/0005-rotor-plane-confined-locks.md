# ADR 0005: Rotor-Based Plane-Confined Locks

**Status:** Proposed (2026-05-04)
**Date:** 2026-05-04
**Deciders:** Goose (architect)
**Spec impact:** §7 (Orthogonality Engine — extends Decision #21's mixed-metric machinery), §2 (Grammar — `#rotor_lock` declaration form, `#thread_plane` clause), §4 (Type System — plane-typed lock identifiers)
**DECISIONS.md:** Refines Decision #21. Targeted as a v0.7+ addition.
**Predecessor ADRs:** ADR 0002 (mixed-metric GA, Decision #21).
**Branch:** `adr/0005-rotor-plane-confined-locks`.

---

## TL;DR

Decision #21 / ADR 0002 established that locks for shared automaton state
are multivectors `lock(L) = pri(L) + e_L` in a mixed-metric Clifford
algebra, with rotors playing a *tiebreak* role for same-priority locks.
This ADR proposes a stronger interpretation: **rotors are the
acquisition primitive, and lock state lives in the algebra**.

A `#rotor_lock L` is conceptually a multivector cell `M`. Initially `M =
1` (scalar identity, "unlocked"). To acquire `L`, a thread `t` whose
signature bivector is `B_t` rotates the cell:

```
M ← R_t · M   where R_t = exp(-θ_t · B_t / 2)   // rotor in plane B_t
```

Three properties fall out *for free*:

1. **Mutual exclusion.** A second thread `t'` with `B_t' ≠ B_t` cannot
   acquire while `M = R_t`, because `R_t' · R_t` is not a pure rotor —
   it has odd-grade components, the algebra rejects it.
2. **Wrong-thread release detection.** Releasing means computing `R̃_x ·
   M`. Only `R̃_t · R_t = 1`. Any other thread's release leaves a
   non-scalar.
3. **Re-entrancy by the same thread.** `R_t · R_t = exp(-2θ_t · B_t / 2)`
   is still a rotor in plane `B_t`. Re-entry is structurally a no-op
   from the algebra's perspective — the holder stays the holder.

The static-analysis benefit is that **the orthogonality engine's
existing wedge-product primitive is the lock check**. Compile-time:
`caller.thread_plane ∧ lock.plane = 0` ⇔ acquire is impossible (planes
orthogonal); `caller.thread_plane = lock.plane` ⇔ acquire is provably
allowed. Runtime: lock is implemented as a normal owner-tracking
spinlock with an integer owner ID; the GA is the *modeling formalism*
and *static-analysis lens*, **not** the runtime data structure.

**Crucial clarification on `exp()` cost:** the runtime never computes
multivector exponentials. `exp` shows up in the formalism so the
`R · R̃ = 1` algebra is precise; the lowered code is a normal CAS-based
spinlock with an owner-ID field. This is the same pattern Decision #21
established: GA is the proof system, not the runtime.

**Recommendation:** Mark Decision #21 as superseding the "rotor-as-
tiebreak" reading with the "rotor-as-acquisition" reading proposed
here, *if and only if* the five open questions in §6 resolve cleanly.
Implementation gated to v0.7+ alongside the rest of Decision #21's
shared-state machinery. **Do not lock yet** — this ADR is **Proposed**,
not **Accepted**, until §6 closes.

---

## Context

### Where ADR 0002 left rotors

ADR 0002 (Decision #21) made rotors a *deterministic tiebreak* for
same-priority locks:

> Same-priority ties resolve deterministically through GA *rotors*
> parameterised by the lock's structural attributes (MMIO address for
> register blocks; declaration-site ordinal for software locks).

The rotor was *bookkeeping* — a way to choose between equally-prioritised
candidates. The *acquisition* mechanism itself was conventional: lock as
priority + ID, acquire via "raise current priority floor to lock's
priority."

### What the user observed

Working with the algebra, the user noticed that the same primitives could
*be* the acquisition mechanism, not just the tiebreak:

> "Something that rotates in a multidimension on different planes and
> aligns with the one that has the lock and it can only rotate in that
> plane until released — otherwise if some other plane (thread) tries
> to rotate it, won't be in its plane, in that way only the code
> executing is possibly using it."

This is a sharper statement than what ADR 0002 captured. Lock state
itself becomes algebraic: the lock *is* a rotor when held, and the
"who holds it" question is decided by which plane the rotor lives in.
The algebra encodes the holder.

### Why this is interesting

Three properties make this worth an ADR:

1. **Reuses the orthogonality engine's wedge primitive.** No new
   algebra — `caller.plane ∧ lock.plane` is already what §7.4 computes.
   The lock check is one more wedge product per acquisition.
2. **Compile-time decidability.** For embedded firmware where thread
   topology is static (interrupt vectors known at link time, main loop
   plane fixed), the *entire* lock-correctness analysis collapses to
   plane assignment + wedge products. No runtime check needed for the
   well-formed case; runtime check is only for the dynamic-thread case.
3. **Encapsulation symmetry with Decision #25.** Decision #25 said:
   "encapsulation is the bit isn't there for outsiders to refer to."
   This ADR says: "mutual exclusion is the plane isn't there for
   outsiders to rotate into." Both are *algebraic-trivial*
   correctness properties — no separate algebra layer, just absence
   of the relevant basis component.

---

## Proposal

### 1. The acquisition algebra

Let the algebra be `Cl(p, 0, n)` per Decision #21:
- `n` private (null) basis vectors `f_1, …, f_n` for private fields.
- `p` shared (non-null) basis vectors `e_1, …, e_p` for `#shared` fields,
  with `e_i² = +1` (Euclidean) — chosen so `exp` is well-defined and
  produces standard rotations.

Each *thread* (conceptually: each independent execution context — an
`#interrupt`, the main loop, an RTOS task) has a unique **signature
bivector**:

```
B_t = e_{i_t} ∧ e_{j_t}   where (i_t, j_t) is unique per thread t
```

With `p ≥ 2k` shared basis vectors we can support up to `k` distinct
threads with non-overlapping bivectors. (Embedded targets: `p = 16`
gives 8 threads, comfortably more than typical Cortex-M IRQ counts.)

A **rotor in plane `B_t`** is:

```
R_t(θ) = exp(-θ · B_t / 2) = cos(θ/2) - sin(θ/2) · B_t
```

Properties (Euclidean bivector, `B_t² = -1`):
- `R_t(θ) · R̃_t(θ) = 1` (rotor inverse exists and is the reverse).
- `R_t(θ_1) · R_t(θ_2) = R_t(θ_1 + θ_2)` (same-plane rotors compose
  in plane).
- `R_t · R_{t'}` for `B_t ≠ B_{t'}` is *not a rotor* — it's a general
  even-grade multivector with components in both `B_t`, `B_{t'}`, and
  `B_t ∧ B_{t'}` (a 4-blade if the planes are independent).

### 2. Lock state and operations

A `#rotor_lock L` has an algebraic state `M_L`, a multivector cell.

| Operation                  | Algebra                                    | Runtime equivalent                |
|----------------------------|--------------------------------------------|-----------------------------------|
| `init L`                   | `M_L ← 1` (scalar)                        | atomic owner-ID = NONE            |
| `acquire L from t`         | `M_L ← R_t · M_L` (must keep grade-even)   | CAS owner-ID NONE → t             |
| `release L from t`         | `M_L ← R̃_t · M_L`                          | CAS owner-ID t → NONE             |
| `try_acquire L from t'`    | check `R_{t'} · M_L` is grade-even         | check owner-ID == NONE atomically |

The static check (compile time, well-formed case) is:

```
∀ acquire(L) site in callable C:
    C.thread_plane = L.plane    // statically declared, must match
∀ release(L) site in callable C:
    C.thread_plane = L.plane    // ditto
∀ access of #shared field f #guarded_by L from callable C:
    C is inside an acquire(L)…release(L) block
    AND C.thread_plane = L.plane
```

The runtime check (dynamic-thread case) is the standard CAS — no
multivector arithmetic required, because the only thing being compared
is *plane identity*, which is a fixed integer per thread.

### 3. Surface syntax

```clifford
// Lock declaration: lock identity = its plane.
#rotor_lock UartTx #plane: e_5 ∧ e_6 #priority: HIGH;

// Shared state guarded by the lock.
#shared
#automaton Uart {
  tx_buffer: [u8; 64] #shared #guarded_by: UartTx;
  tx_head:   usize    #shared #guarded_by: UartTx;
}

// Each thread declares its signature plane.
#interrupt UART_TX_DMA() #priority: HIGH #thread_plane: e_5 ∧ e_6 {
  #with_lock(UartTx) {                           // ← static check passes:
    Uart.tx_buffer[Uart.tx_head] = 0u8;          //   thread.plane = lock.plane
    Uart.tx_head = Uart.tx_head + 1usize;
  }
}

#interrupt SYSTICK() #priority: MEDIUM #thread_plane: e_3 ∧ e_4 {
  #with_lock(UartTx) {                           // ← static check fails:
    Uart.tx_head = 0usize;                       //   E0535 plane mismatch
  }                                              //   thread.plane = e_3∧e_4
}                                                //   lock.plane   = e_5∧e_6
                                                 //   wedge ≠ 0 of grade 4
                                                 //   → planes are independent
                                                 //   → cannot acquire

@fn current_head() -> usize $ [Pure, PureState] {
  return @snapshot Uart.tx_head;                 // ← Decision #24's @snapshot
}                                                //   does NOT need the lock
                                                 //   (Snapshot path is read-only,
                                                 //   sequentially consistent under
                                                 //   the holder's release)
```

### 4. Diagnostic shape (E0535 family)

Per the convention in §10 of the spec:

- **E0535 PlaneeMismatch.** "Lock `UartTx` requires `thread_plane: e_5 ∧
  e_6`; caller `SYSTICK` declares `thread_plane: e_3 ∧ e_4`.
  Wedge-product of plane signatures = `e_3 ∧ e_4 ∧ e_5 ∧ e_6` (4-blade,
  non-zero) → planes are independent → acquire is impossible."
- **E0536 NoThreadPlane.** "Caller `foo` references lock `UartTx` but
  declares no `#thread_plane`. Add `#thread_plane: …` to the
  declaration."
- **E0537 SharedFieldOutsideLock.** "Field `Uart.tx_buffer` is
  `#guarded_by: UartTx` but referenced outside an enclosing
  `#with_lock(UartTx)` block."

All three are statically detectable; none require runtime instrumentation.

---

## Cost analysis: is the algebraic framing worth it vs. a normal lock?

This is the user's explicit question: **"check if exp is worth the
check or we shall stick to normal lock."** Direct answer:

### At runtime: NO. Normal lock semantics, no GA arithmetic.

The lowered code is a standard CAS-based spinlock with an integer
owner-ID field:

```
struct Lock {
    owner: AtomicU32,  // 0 = unowned; otherwise the holder's thread-plane index
}

acquire(L, t):  while CAS(L.owner, 0, t) != 0 { spin }
release(L, t):  CAS(L.owner, t, 0)  // panics on holder mismatch
```

No `exp`, no rotor multiplication, no multivector data structures.
Each thread's signature plane is a small integer index (the chosen
basis-vector pair, encoded as `(i_t, j_t)` packed into a u32).
"`R_t · M_L`" is "atomic compare-exchange owner field." Runtime cost
is identical to a Rust `Mutex` or a Linux spinlock.

### At compile time: YES. The algebra earns its keep.

The `exp` formulation is what makes the *static check* trivial:

- "Is acquire possible?" ⇔ "Is `caller.plane = lock.plane`?" — equality
  of two stored integers.
- "Is the acquire algebraically valid?" ⇔ "Is `caller.plane ∧
  lock.plane = 0` interpreted as planes-equal?" — the wedge product
  the engine *already* computes.
- "Is the release pairing correct?" ⇔ "Did the same caller acquire it?"
  — single-pass scope check.
- "Are two acquires of *different* locks composable?" ⇔ "Is `B_t1 ∧
  B_t2 = 0` (disjoint planes) ⇒ disjoint locks; otherwise structured
  ordering by priority-then-rotor (Decision #21 §5.5)."

All four collapse to wedge products. The orthogonality engine's
hot path is unchanged.

### Compared to a "normal" lock + post-hoc verification

A conventional approach would be:
1. Declare `Lock<T>` with runtime owner tracking.
2. Run a separate static analyzer to check lock pairing and ordering.

The proposed approach **unifies the analyzer with the orthogonality
engine** — there's no separate "lock-correctness pass," it's the same
wedge-product machinery already running. That's the engineering win:
one pass, one algebra, two safety properties (disjoint mutation +
mutual exclusion).

### Compared to a "normal" lock + no static verification

A pure-runtime approach (just normal locks, no static analysis) is
strictly weaker: it catches deadlocks at runtime (or never, depending
on the hardware), it can't prove plane-confinement at compile time,
and it can't detect cross-plane re-entry attempts before they execute.
For embedded firmware where bugs ship with the device, the
compile-time check is the entire point of the language.

### Conclusion

**The `exp` formulation is worth it as a modeling lens, free at
runtime.** Stick to "normal lock" semantics for codegen; use the GA
formalism for the static check. This is the same trade Decision #21
made for shared automata, applied recursively to the lock primitive
itself.

---

## Open questions (must close before Accepting)

These five questions are why this ADR is **Proposed**, not **Accepted**.

### Q1. How are thread-planes assigned?

**Embedded (static) case.** Each `#interrupt` and the main loop get a
unique bivector at compile time. A "plane assignment" pass (analogous
to §7.1's basis assignment) walks the program, enumerates execution
contexts, and assigns each one a distinct `e_i ∧ e_j` pair from a
reserved range. Bounded by `p = 16` shared basis vectors → 8 threads,
matching realistic Cortex-M / RISC-V IRQ counts.

**Dynamic (RTOS) case.** Tasks created at runtime need plane
allocation. Options:
- (a) Reserve a "task plane pool" at link time; tasks pull from the
  pool. Bounded by pool size.
- (b) Treat all tasks as living in a single shared "task plane" and
  use conventional priority-based scheduling within that plane. The
  rotor model only distinguishes interrupts from tasks, not tasks
  from each other.
- (c) Out-of-scope for v0.1; defer to a v0.8+ task scheduler ADR.

**Proposed resolution.** (a) for v0.7 with a fixed pool; (c) re-opens
the question if it proves too restrictive. v0.1 only sees the
embedded static case.

### Q2. Re-entrancy semantics

The algebra says `R_t · R_t = R_t(2θ)` is still a valid same-plane
rotor. So same-thread re-entry is *structurally* a no-op (the lock
remains "in the holder's plane"). Three options:

- (a) **Free re-entry (algebra default).** Re-acquire by the same
  thread succeeds with no counter; release-once releases the lock.
- (b) **Counted re-entry.** Track depth via accumulated `θ`; release
  decrements; lock truly releases at depth 0.
- (c) **No re-entry.** Re-acquire by the same thread is `E0538`
  (analogous to a deadlock detection).

(a) matches the math but surprises users coming from POSIX (which is
counted). (b) matches POSIX but adds a depth counter and breaks the
"M is purely a rotor" property. (c) is the most defensive but rejects
legitimate recursive structured code.

**Proposed resolution.** (b) — counted re-entry. The depth counter
lives alongside the owner-ID at runtime; the algebra still describes
the held-state, just with `M_L = R_t(nθ)` for re-entry depth `n`.
Trade is small: one extra word per lock.

### Q3. Same-plane parallel access (uniqueness of thread planes)

If two distinct threads were assigned the same plane, the algebra
treats them as the same thread — both "have the lock" simultaneously,
which is wrong for mutual exclusion. The plane-assignment pass MUST
guarantee uniqueness.

**Proposed resolution.** Plane-assignment pass enforces uniqueness;
duplicates are `E0539 DuplicateThreadPlane`. Easy to enforce; no
algebraic surprise.

### Q4. Release symmetry — who carries `θ`?

The release operation `M_L ← R̃_t · M_L` requires knowing `θ_t`. Two
storage options:

- (a) The lock stores `(B_t, θ_t)` (or just the rotor `R_t`).
  Release is "lock, find your own inverse, apply."
- (b) The thread carries `θ_t` per held lock. Release is
  "thread, compute your inverse, apply to lock."

(a) is what the runtime owner-ID field already does (the owner ID
encodes both `B_t` and the implicit `θ_t = lock-acquisition step`).
(b) requires per-thread bookkeeping.

**Proposed resolution.** (a). The lock owns its full state. Threads
just check "am I the owner?" on release. This matches the normal-
lock runtime model and keeps `θ_t` out of thread-local data.

### Q5. Relation to Decision #21's lock-priority + rotor-tiebreak

Decision #21 already has:
- `lock(L) = pri(L) + e_L` (priority scalar + identity bivector).
- Same-priority ties broken by rotors parameterised by structural
  attributes.

This ADR proposes an *operational* extension: the rotor isn't just a
tiebreak label, it's the acquisition action. Two readings:

- (a) **Rotor-as-acquisition supersedes rotor-as-tiebreak.** Decision
  #21's `pri + e_L` framing is recovered: `pri` is still the priority
  scalar; `e_L` is the lock's identity component (which thread can
  hold it parameterises *which rotor planes are admissible*).
- (b) **Both coexist.** Priority-based ordering still governs lock
  ordering (deadlock-freedom proof in §5.5 of ADR 0002); rotor
  formulation governs ownership semantics.

(a) is cleaner; (b) is more conservative.

**Proposed resolution.** (a) supersedes; the priority-ordering proof
in ADR 0002 §5.5 is re-derived in terms of plane-acquisition order
rather than priority-bump order. The two formulations turn out to
be equivalent (priority is just the canonical strict total order on
planes); the rotor framing is the more general one.

---

## Consequences

### If accepted as proposed

- v0.7+ implementation work (Decision #21 milestone) gains a lock
  acquisition mechanism with full compile-time plane-confinement
  analysis.
- ADR 0002 §5.5 is *refined* (not retracted): the rotor formulation
  becomes the acquisition primitive; priority-ordering becomes a
  derived total order on planes.
- Spec §7 gains a new subsection on plane-confined locks (cross-
  reference to §7.9 mixed-metric).
- Book Ch. 21 (Decision #21) gains a worked example: a UART driver
  with `#rotor_lock`-protected TX buffer.

### Doors kept open

- `#thread_plane` clause shape is forward-compatible with the lexer's
  reservation of `#shared` / `#lock` / `#with_lock` / `#reads` /
  `#rotor` tokens (Decision #21).
- The runtime owner-ID field is the same shape Decision #21 anticipated;
  this ADR doesn't change the lowered-code layout.

### Doors potentially closed

- If the wedge-product test for plane-disjointness turns out to need a
  different metric than the `e_i² = +1` Euclidean choice (e.g., for
  hyperbolic-plane locks in some exotic priority structure), this
  ADR's `Cl(p, 0, n)` choice would need revisiting. **Mitigation:**
  v0.7 implementation can ship with `e_i² = +1` and revisit if a
  concrete need surfaces.

### If rejected

- Decision #21's rotor-as-tiebreak framing remains canonical.
- v0.7 implementation falls back to a conventional `Lock<T>` runtime
  primitive plus a separate static lock-pairing analyzer (which is
  what 95% of language implementations do — no shame in this
  fallback).
- The "encapsulation symmetry with Decision #25" elegance argument is
  lost, but no correctness property is.

---

## Decision

**Status: Proposed.** Lock after the five open questions in §6 close
in conversation with the architect. Targeted close: by 2026-06-15.
Implementation gated to v0.7+ alongside the rest of Decision #21.

**Action items if accepted:**
1. Add `#rotor_lock`, `#thread_plane`, `#guarded_by`, `#with_lock`
   tokens to the lexer (most are already reserved per Decision #21).
2. Extend the AST with `RotorLockDecl` and `ThreadPlane` clauses.
3. Implement the plane-assignment pass (mirror of §7.1 basis assignment).
4. Implement the static lock-confinement check (E0535–E0539 family).
5. Lower to standard CAS spinlocks in codegen.
6. Book chapter (Part II: Decision #21 § supplement, or a separate
   chapter in Part III adjacent to Ch. 28 "Rotors and same-priority
   tiebreaks").

---

## Cross-references

- **ADR 0002 / Decision #21** — the mixed-metric machinery this ADR
  refines.
- **Decision #25** — the algebraic-trivial-encapsulation pattern this
  ADR mirrors at the lock primitive (encapsulation by bit-absence;
  mutual exclusion by plane-mismatch).
- **Spec §7.9 (mixed-metric extension)** — where the algebra
  formalism lives.
- **Book Ch. 28 (Rotors and same-priority tiebreaks)** — the existing
  rotor coverage that this ADR generalises from "tiebreak" to
  "acquisition."

---

*This ADR is Proposed. The rotor formulation is the user's framing;
this document is its formalisation. Locking requires resolving §6.*
