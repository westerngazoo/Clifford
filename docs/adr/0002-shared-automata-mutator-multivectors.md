# ADR 0002: Shared Automata via Mutator Multivectors (Mixed-Metric GA)

**Status:** Draft — proposed
**Date:** 2026-05-01
**Deciders:** Goose (architect), TBD reviewers
**Spec impact:** §7 (Orthogonality Engine), §2 (Grammar), §4 (Type System)
**DECISIONS.md:** Proposes Decision #21
**Branch:** `spec/decision-21-shared-automata`

---

## TL;DR

The current GA orthogonality engine works in a Clifford algebra Cl(0,0,n) — every
basis vector squares to zero, which is exactly why the bitmask implementation
detects a write-write race as `a & b != 0 ⇒ wedge == 0`. This is mathematically
clean for *disjoint-mutation* programs but cannot model *shared mutable state*
that real kernels (Wari, seL4, Hubris, Linux) deliberately require — run-queues,
capability tables, page allocators, IRQ binding tables.

This ADR proposes **Decision #21: Shared Automata via Mutator Multivectors**.
A new `#shared` field qualifier (and supporting `#lock:` automaton clause)
puts shared state in a *different metric subspace* of the GA — non-null basis
vectors that don't collapse the wedge product. The orthogonality theorem
extends from "wedge must be non-zero" to "wedge's null-subspace projection
must be non-zero, AND any non-null subspace overlap discharges a separate
lock-coverage proof obligation."

This unifies disjoint-mutation safety (current engine) and lock-coupled
shared-state safety (new) under one algebraic framework.

**Recommendation:** Lock the design direction in DECISIONS.md as Decision #21,
update §7 of the spec to declare the current algebra as the *restricted form*
(Cl(0,0,n)) and reserve the mixed-metric extension for v0.7.0-draft.
**Implement only minimal AST scaffolding now** (a `FieldKind` enum on
`AutomatonField` with one variant today, `Private`) so that adding
`FieldKind::Shared { lock: Ident }` later is a non-breaking AST change.
Defer engine work (the `crates/ortho` mixed-metric algebra) and surface
syntax (`#shared` parser support) to v0.7.

The user-facing reason this matters: **without locking the design direction
now, we risk hard-coding decisions in §7 and the `ortho` crate's data
structures that would foreclose this extension later.** The ADR's "doors to
keep open" section enumerates exactly which.

---

## Context

### What the current engine proves

Per §7.4 of `CLIFFORD_SPEC.md`, the orthogonality check is:

```rust
fn outer_product(a: u64, b: u64) -> Option<u64> {
    if a & b != 0 { None }              // shared basis → wedge is zero
    else          { Some(a | b) }       // disjoint → bitwise union
}
```

The mathematical statement is: *two automata are concurrent-safe iff their
behavior multivectors wedge to a non-zero element of the algebra*. The bitmask
encoding works because we are implicitly in Cl(0,0,n) — every basis vector
squares to zero, so `e_q ∧ e_q = 0`, so any shared basis vector collapses the
wedge to the zero element.

This is the strongest possible isolation property: **two automata can run
concurrently iff they touch literally disjoint state.** It's also exactly the
property an embedded firmware system wants — interrupt handlers and main code
should not share writable fields.

### What the current engine cannot prove

Real kernels deliberately have shared state. Looking at `wari/kernel/src/`:

| Subsystem | Wari file | Shared-state shape |
|---|---|---|
| Run-queue | `sched/process.rs` | One per hart; readable from scheduler IRQ; written by `enqueue`/`dequeue` paths from many syscalls |
| Capability space | `cap/cspace.rs` | Per-process; read by lookup paths; written by `mint`/`copy`/`revoke`. Today single-hart; multi-hart in Phase 2 |
| Page allocator | `mem/page_alloc.rs` | Global free-list; many writers (page faults from any hart) and many readers (alloc/free) |
| IRQ binding table | `mmio/plic.rs` | `IRQ_NOTIFICATION_BINDINGS`; written at boot, read by every external-IRQ trap. Wari documents this as `INV-23` |
| `kvm.rs` page table | `mem/kvm.rs` | Read by every TLB miss; written by mapping syscalls |

For each of these, "two automata both write `RunQueue.tasks`" is *correct*, not
a bug — provided they hold the run-queue lock. The current GA engine has no
vocabulary for "this is shared, the proof obligation is different."

Wari handles this today by:
1. Curating an `INV-N` invariant catalog (`docs/invariants.md`).
2. Writing English-language safety arguments in module docs (see the `INV-3` /
   `INV-23` citations in `mmio/plic.rs`).
3. Putting `static mut` behind discipline rather than type-level proof.

This is the standard kernel approach — and it's the part Clifford could most
uniquely improve, *if* the algebra extends.

### What "mutator multivectors" means algebraically

User's framing in the originating discussion. In a general Clifford algebra
Cl(p,q,r) over basis vectors {e_i}, we have:

- **p basis vectors with `e_i² = +1`** (positive-definite)
- **q basis vectors with `e_i² = -1`** (negative-definite)
- **r basis vectors with `e_i² = 0`** (null / degenerate)

The current engine works in Cl(0,0,n): all basis vectors null. The proposal
is to extend to Cl(p,0,n): some basis vectors null (private fields), some
non-null (shared fields). The algebra now has:

- `e_q ∧ e_q = 0` for q ≤ n (private — current behavior; collapses wedge)
- `e_~q ∧ e_~q = 1 ≠ 0` for q ≤ p (shared — does *not* collapse wedge)

The wedge product of two behavior multivectors is then a mixed-grade element
whose **null-subspace projection** signals whether private state is shared
(the existing race condition) and whose **non-null-subspace projection**
signals which shared resources are touched (a new proof obligation).

The orthogonality theorem extends to:

> **Concurrency-safe(A, B) iff:**
> 1. `(behavior(A) ∧ behavior(B)) | null_subspace ≠ 0`
>    *(no private races — current condition)*
> 2. **AND** for every shared basis vector `e_~q` appearing in both behaviors:
>    A and B both hold the lock declared on that shared resource at the time
>    they touch it.
>    *(lock-coverage discharge — new condition)*

Condition 1 is *automatic* — the existing engine plus the metric tag on each
basis vector. Condition 2 is a separate static analysis pass over the AST,
similar in spirit to Rust's borrow checker but simpler (lock identity, not
lifetime).

This is mathematically elegant and *additive* to the current engine. The
existing implementation continues to work for purely-private programs (the
common embedded case). The extension activates only when a `#shared` field
appears in the AST.

---

## Design space — four options

### Option A — `#shared` automaton + named lock

**Surface:**
```clifford
#shared #automaton RunQueue {
  #lock: rq_lock;
  #basis: rq_basis;
  tasks: [TaskId; MAX_TASKS];
  head:  usize;
  tail:  usize;
}

#effect enqueue(t: TaskId) #mutates: [RunQueue] {
  #with_lock(rq_lock) {
    RunQueue.tasks[RunQueue.tail] = t;
    RunQueue.tail += 1;
  }
}
```

**Algebra:** The automaton's basis vector becomes non-null. Every field within
becomes part of the same non-null subspace.

**Pros:**
- Simplest surface change.
- Maps directly to spinlock patterns Wari already uses.
- One conceptual unit (the lock) protects one conceptual unit (the automaton).

**Cons:**
- Coarse-grained: an automaton with one shared field and ten private fields
  has all ten "promoted" to shared.
- No room for read/write asymmetry within an automaton.
- Doesn't model lock-ordering deadlocks.

### Option B — Option A + verified lock-ordering

**Surface:** Adds a top-level attribute:
```clifford
@lock_order(rq_lock < pa_lock < cspace_lock);
```

**Static check:** A `#with_lock(b) { ... }` body cannot call any `#effect`
that internally takes a lock earlier in the order than `b`. This rules out
the entire deadlock class.

**Pros:**
- Closes a real bug class. Linux's lockdep is famously the most complex piece
  of the kernel that still finds bugs years after merging.
- Genuinely publishable: "GA-mechanized deadlock-free locking discipline."

**Cons:**
- Lock ordering is a real expressibility constraint. Cases where lock A and
  lock B can be taken in either order (depending on which is contended)
  require either trylock-with-rollback (an algebra extension on its own) or
  a redesign of the protocol.
- The static check is a separate proof system; not just an algebra extension.
  More work, more places to be wrong.

### Option C — Per-field shared/private with read/write asymmetry

**Surface:**
```clifford
#automaton CSpace {
  #lock: cspace_lock;
  #basis: cspace_basis;
  caps: [Capability; MAX_CAPS] #shared;        // RCU-shape allowed
  generation: u64               #shared;        // monotonic counter
  cookie: u32;                                  // private, not lock-protected
}

#effect lookup(idx: usize) -> Capability #reads: [CSpace] {
  return CSpace.caps[idx];                      // no lock for read
}

#effect insert(c: Capability) #mutates: [CSpace] {
  #with_lock(cspace_lock) {
    CSpace.caps[next_slot()] = c;
    CSpace.generation += 1;
  }
}
```

**Algebra:** Each field gets its own metric tag. Reads contribute to a
*read-multivector*; writes contribute to the existing write-multivector. The
orthogonality check distinguishes:
- write ∧ write on shared field → discharge lock obligation (current ADR scope)
- read ∧ write on shared field → discharge a different obligation (RCU epoch
  reclamation, or also the same lock — design choice per field)
- read ∧ read → always safe (matches §7.2 v0.2 work hint)

**Pros:**
- Fine-grained: a `cookie` field that nobody synchronises stays private and
  benefits from full disjointness checking.
- Read/write asymmetry is how real kernels actually work (cspaces are RCU,
  page tables are RCU, route caches are RCU).
- Subsumes Option A: an automaton where every field is `#shared` is the
  Option-A case.

**Cons:**
- More surface area; more parser work; more AST machinery.
- The read/write asymmetry hint in §7.2 ("v0.2 work") needs to be
  reconciled with this — they should be one extension, not two.

### Option D — Don't add `#shared`. Keep the language pure.

Shared state in a kernel goes in `#unsafe_shared { ... }` blocks. The audit
log lists every shared-state access. Pure embedded code stays verified;
kernel bringup goes through audit.

**Pros:**
- Preserves the orthogonality theorem unmodified.
- Doesn't pretend to prove things we don't.
- Maps to the "pure functional core, narrow imperative shell" pattern that
  works for OCaml/Haskell systems work.

**Cons:**
- Wari's most interesting subsystems (cspace, sched, page alloc) would need
  audit-only blocks.
- Forecloses the most novel research result Clifford could plausibly produce
  (mechanizing pieces of `cap/proofs.rs`).
- The stated goal of "embedded firmware as canonical first target, but the
  same constructs work for servers, robotics, scientific computing"
  (`CLAUDE.md §0`) is harder to defend if kernels are out of scope.

---

## Prior art

| System | Mechanism | What we learn |
|---|---|---|
| **seL4** | Capability-soundness in Isabelle/HOL; manual proofs | The proof obligation we'd mechanize is exactly seL4's. Person-years of Isabelle; if we could make it a type-check, that's the contribution. |
| **Hubris (Oxide)** | Statically-configured task table; no shared state at all in the kernel | The Option-D shape, taken seriously. Works for some kernel architectures but rules out shared-cache designs. |
| **Tock** | `TakeCell` — single-owner moveable cell; capsules don't share | Capsule isolation by construction. Doesn't help with deliberate sharing. |
| **GhostCell (Yanovski et al, ICFP 2021)** | Type-level brand to share state without runtime check | Same algebra family — phantom basis vectors that distinguish "branded" from "unbranded" access. Direct intellectual ancestor of this proposal. |
| **RustBelt's `RwLock`** | Higher-order separation logic proof of `RwLock<T>` soundness in Iris | The proof obligation discharge format we'd want to match. Iris is overkill for our setting but the *shape* is right. |
| **Linux lockdep** | Runtime lock-order tracking | Demonstrates the pain Option B is paying down. |

The closest formal-verification cousin is **GhostCell**. The mathematical
flavor (separate metric subspaces for branded basis vectors) is direct.

---

## Recommendation

**Adopt Option C — per-field shared/private with read/write asymmetry — but
ship it in two phases.**

### Phase 1 (this ADR): lock the design direction. Implement minimal AST
### scaffolding to keep the door open.

Concretely, in this PR:

1. **`docs/DECISIONS.md` Decision #21** — state the design direction with a
   rationale block. Mark as ✓ LOCKED. Include the algebraic statement and
   the §7 extension sketch.

2. **`docs/CLIFFORD_SPEC.md` §7.0 (new prologue):** declare the current
   algebra as Cl(0,0,n) (the *restricted form*) with a note that the
   mixed-metric extension is reserved for v0.7.0-draft and tracked by
   Decision #21.

3. **`docs/CLIFFORD_SPEC.md` §7.9 (new):** sketch the mixed-metric extension
   (~half page). State the extended orthogonality theorem. Note that the
   current `outer_product` bitmask remains correct for the null-subspace
   projection; the non-null subspace requires a parallel data structure
   (TBD in v0.7).

4. **`crates/ast/src/lib.rs`:** introduce a single-variant enum:
   ```rust
   pub enum FieldKind {
       /// Field is private state — contributes a null basis vector to the
       /// behavior multivector. Race detection: any sharing is a hard error
       /// (the current §7 behavior).
       Private,
       // FUTURE (Decision #21, v0.7+): Shared { lock: Ident }, ReadShared, etc.
       // Adding variants here is non-breaking because we mark the enum
       // `#[non_exhaustive]`.
   }
   ```
   `AutomatonField` gains a `kind: FieldKind` field defaulted to `Private`.
   The parser sets it unconditionally to `Private` for now.

5. **`crates/ortho/src/lib.rs`** (when it lands — currently a stub): the
   orthogonality engine's primary data structure for behavior multivectors
   will be designed with a metric tag per basis vector from day one. Even
   if v0.1 only uses null tags, the type carries the dimension.

That's it for code. **No `#shared` parser, no engine extension, no DECISIONS.md
change beyond locking the direction.** The total diff is < 200 lines.

### Phase 2 (v0.7.0-draft, after Phase 0–4 close): implement the extension.

1. Surface syntax in the lexer + parser: `#shared` field qualifier, `#lock:`
   automaton clause, `#with_lock(name) { … }` statement, `#reads:` clause
   on effects, optional `@lock_order(…)` attribute.
2. AST extension: add `FieldKind::Shared { lock: Ident }`, `LockClause` on
   `AutomatonDecl`, `WithLock` statement, `reads` field on `EffectDecl`.
3. `crates/ortho` mixed-metric algebra implementation. The bitmask needs to
   become two parallel bitmasks (null subspace, non-null subspace) or a
   single 128-bit representation with the metric tag in the high bits.
4. `crates/check` lock-coverage analysis: for every effect that touches a
   shared field, verify the enclosing `#with_lock` covers it.
5. (Stretch, possibly v0.8) Lock-ordering check (Option B addition).

---

## Doors we keep open by Phase-1 scaffolding

These are the **concrete things that would foreclose Decision #21 if we
didn't do the Phase-1 scaffolding now:**

| If we don't… | …we'd later need to |
|---|---|
| Mark `FieldKind` `#[non_exhaustive]` from day one | Break every downstream pattern match in `clifford-types` / `clifford-check` to add a new variant |
| Carry a metric tag on basis vectors in `crates/ortho` from day one | Refactor every behavior-multivector data path; risk silent miscompilation in the part of Clifford with the smallest tolerance for it |
| Reserve `#shared`, `#lock`, `#with_lock`, `#reads`, `@lock_order` in the lexer's `#`-form catalog | Either rename them later (breaking source compat) or live with whatever ad-hoc names accidentally don't collide |
| Document the algebra as Cl(0,0,n) in §7.0 with reserved §7.9 for the extension | Have to retrofit a non-trivial mathematical extension into an already-published spec. Spec versioning makes this expensive |
| Write the Decision #21 LOCKED entry now | Risk Decision #21 being assigned to something else that contradicts the design direction |

**Cost of Phase-1 scaffolding:** ~200 LoC, one ADR, one DECISIONS entry,
one §7.0 + §7.9 spec edit. Maybe a half-day of work.

**Cost of *not* doing Phase-1 scaffolding:** weeks-to-months of refactoring
when v0.7 work begins, plus the ever-present risk of an accidentally
foreclosing decision in the meantime.

---

## Risks and open questions

### Risk: the algebra doesn't simplify as cleanly as I think

The metric extension *should* be additive, but the actual proof of "the
extended orthogonality theorem coincides with the existing one when no shared
fields are present" needs to be written down (§7.9) and reviewed. If it
turns out the extension has a footgun — e.g. the non-null wedge product
behaves badly in the presence of bivectors generated from the existing
trait basis vectors — the design changes.

**Mitigation:** Phase 1 scaffolding does *not* commit us to the algebra
specifics. It commits us to "*some* extension lives in this metric direction"
and reserves spec section §7.9 for it. We can revise the algebra in v0.7
work without invalidating the scaffolding.

### Risk: lock-coverage analysis is unexpectedly hard

Discharging "did A and B both hold the lock when they touched the shared
basis vector?" requires a flow-sensitive check across the call graph.
`#with_lock(L) { ... }` blocks must dominate every read/write of a
`#shared` field protected by L. This is doable (the existing
`clifford-check` walk has the right shape) but we don't have a working
implementation to compare against. If it turns out to be NP-hard or
exponentially slow on real programs, we have a problem.

**Mitigation:** This risk lives entirely in v0.7. The Phase-1 scaffolding
doesn't depend on it being tractable. Worst case, we walk back to Option D
in v0.7 having only spent the scaffolding cost.

### Open: does `#shared` interact with `#staged` (Decision #12, deferred to v0.2)?

Decision #12 introduces *staged* automata for deferred mutation — writes
accumulate in a staging area and commit atomically at a sync point.
Conceptually, a staged write is "private until commit, then visible to
everyone." A `#shared #staged` automaton would have weird semantics:
locks for the visible state but lock-free staging area? Worth thinking
through before v0.2 lands Decision #12.

**Resolution:** Note the interaction in DECISIONS.md Decision #21. Do not
attempt to resolve until both #12 and #21 implementations begin.

### Open: does this affect `#interrupt` declarations?

Wari's PLIC `IRQ_NOTIFICATION_BINDINGS` is read at every external-IRQ trap
and written only at boot — a textbook RCU-ish pattern. Would the `#interrupt`
form need a `#reads:` clause too? Almost certainly yes, but the design fits
naturally: `#interrupt USART1_IRQHandler() #reads: [IrqBindings] #mutates:
[Counter] #priority: HIGH { … }`.

**Resolution:** Confirmed in scope of Decision #21; no separate decision
needed.

---

## Compliance

This decision is compatible with:

- `CLAUDE.md §1.1` — "A boring algorithm with a written proof beats a clever
  one with a hunch." This ADR *is* the written proof's outline; the §7.9
  spec text is the proof itself.
- `CLAUDE.md §4.1` — "Every algorithm cites its source." GhostCell and
  RustBelt cited; algebraic foundation cited (general Cl(p,q,r) Clifford
  algebra).
- `CLAUDE.md §8.1` — "Surface uncertainty. If the spec has an `[OPEN]`
  marker, the agent stops and asks." This ADR opens the discussion before
  any implementation; it is the explicit pause-and-ask before extending §7.

This decision is consistent with:

- The "general-purpose systems language" framing in `CLAUDE.md §0` (kernels
  are in scope).
- Decision #5's automaton-as-category foundation (shared automata are still
  small categories; the metric extension is an enrichment, not a replacement).
- Decision #13 body-scoped references (orthogonal — references and shared
  fields are separate axes).

---

## Decision

**RECOMMENDED — pending Goose's review and any reviewer pushback:**

1. Add Decision #21 to `docs/DECISIONS.md` with the "lock direction, defer
   implementation" framing in this ADR's Recommendation section.
2. Update `CLIFFORD_SPEC.md` §7.0 prologue declaring Cl(0,0,n) as the v0.1–v0.6
   restricted algebra; add §7.9 sketching the v0.7 mixed-metric extension.
3. Add `FieldKind` enum to `crates/ast`, defaulting all fields to
   `FieldKind::Private`, marked `#[non_exhaustive]`.
4. Reserve `#shared`, `#lock`, `#with_lock`, `#reads`, `@lock_order` in
   the lexer's `#`-form catalog (no parser support, just the tokens).
5. **Do not** implement the engine extension; that is v0.7 work tracked by
   this ADR and Decision #21.

If reviewers reject the design direction, no rollback is required —
the Phase-1 scaffolding is small, the spec sections are non-normative
prologue text, and Decision #21 can be marked DEFERRED rather than LOCKED.

---

## Revisit when

- v0.7.0-draft work begins.
- A user-reported issue surfaces a kernel-shape program that the current
  engine cannot model and the unsafe-block escape hatch (Option D) is
  unworkable for.
- A new research result (e.g., a more refined algebra than mixed-metric
  Clifford, perhaps Geometric Algebra over a more exotic structure)
  emerges in the literature that supersedes this approach.
- Decision #12 (`#staged` automata) advances from DEFERRED, at which point
  the interaction noted above must be resolved.
