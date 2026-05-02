# Chapter 21: Decision #21 — Shared automata via mutator multivectors

> **Decision date:** 2026-05-01  
> **Spec section:** §7.0 (prologue), §7.9 (mixed-metric extension)  
> **Status:** ✓ LOCKED v0.6; implementation gated on v0.7  
> **ADR:** [`docs/adr/0002-shared-automata-mutator-multivectors.md`](https://github.com/westerngazoo/Clifford/blob/main/docs/adr/0002-shared-automata-mutator-multivectors.md)

This is the marquee chapter. The decision documented here is the one that makes Clifford genuinely novel — it is the bridge between the existing v0.1 disjoint-mutation orthogonality engine and the v0.7+ ability to model deliberately-shared kernel state. It is also the chapter most likely to become the basis of a published paper.

If you are reading this book in any other order than start-to-finish, this is the chapter to read second (after Chapter 1).

## The question

The current §7 of the spec proves a strong property: *two automata are concurrent-safe if and only if they touch literally disjoint state.* This is exactly the right property for embedded firmware that does not deliberately share mutable state — interrupt handlers, sensor read loops, peripheral drivers in the simple case. The bitmask check `a & b != 0 ⇒ wedge == 0` is the operational form, and it is decidable, fast, and has a beautiful geometric-algebraic foundation (Clifford algebra Cl(0,0,n) with all-null basis vectors).

But real kernels deliberately share mutable state. Run-queues. Capability spaces. Page allocators. IRQ binding tables. Caches. The `INV-23` "IRQ Routing Determinism" invariant in the Wari kernel is exactly such a shared resource — the table is *supposed* to be touched by every external IRQ trap. Forbidding shared mutation altogether would forbid every kernel ever written.

So the question Decision #21 answers is:

> *How do we extend the orthogonality engine to admit deliberately-shared mutable state without losing the static guarantees we already have for disjoint mutation?*

## The conventional answers

A short tour of how the field handles this problem.

### Lock annotations as documentation

The Linux kernel (and most C/C++ kernels) declares lock disciplines in module-level comments. Every shared variable has a comment saying which lock guards it. The compiler does not check this; the kernel team checks it socially through code review, and the Linux `lockdep` runtime checker checks the *acquisition order* discipline at runtime by tracking which locks are held when which other locks are acquired.

Cost: the discipline is real and the runtime checker works, but bugs ship before they manifest. Lockdep finds them in production logs years later.

### Type-system-tracked locks (Rust's `Mutex<T>`)

Rust pulls the locking discipline into the type system. `Mutex<T>` is a parametric type whose `lock()` method returns a `MutexGuard<T>`; the guard, on drop, releases the lock. The `T` inside the mutex is only accessible through the guard, which means at the type level "you cannot access `T` without holding the lock."

Cost: real and useful, but it conflates the lock and the data — every shared data structure has to be declared as `Mutex<...>`, the parametric type proliferates everywhere, and Rust's borrow checker plus mutex semantics produces a notoriously complicated experience for shared data structures that need different locking schemes (RwLock, RCU, hazard pointers, lock-free containers all require their own type wrappers and often `unsafe`). The lock-ordering problem (acquiring two mutexes in deadlock-prone order) is *not solved* by `Mutex<T>` alone — Rust has no built-in static deadlock checker; the standard library's deadlock detector is a runtime feature you have to opt into.

### Capability proofs (seL4)

seL4's capability system is the strongest existing answer. Every operation on every shared resource requires presenting a capability whose authority is proven in Isabelle/HOL. The verification effort took 11 person-years of full-time proof work and produced 200,000 lines of Isabelle for a 10,000-line kernel. The result is genuinely correct concurrent kernel code — but the verification scales poorly to evolving codebases, the Isabelle proofs are decoupled from the C implementation, and the verification community has not produced a path to a similar guarantee in a "normal" working language.

### Concurrent separation logic (Iris, RustBelt)

The Iris framework, developed at MPI-SWS and used by the RustBelt project, proves Rust's standard library and many of its concurrent abstractions correct in higher-order concurrent separation logic. The proofs are profound — the Iris team has shown that `RwLock<T>` is sound — but they are, again, in a separate proof language (Coq), and the soundness theorem they produce ("if Rust's type system accepts your code, you have these properties") is a meta-theoretical statement, not a per-program check.

### Branded references (GhostCell)

The GhostCell paper [Yanovski et al. 2021] is the closest cousin to what Decision #21 does. GhostCell uses *type-level brands* (a phantom type parameter representing "you have permission") to share data between threads safely. The brand is checked structurally at compile time; it has no runtime cost; the soundness proof is in Iris but the everyday user never sees it.

GhostCell proves that the *categorical pattern* — using a phantom-type-or-equivalent to track "permission to access" through the type system — is sound for concurrent shared-state safety. Decision #21 takes the same pattern and realises it in Clifford algebra rather than Rust phantom types. The intellectual debt is direct.

### Static-task-table OS architecture (Hubris)

Hubris (Oxide Computer's microkernel) sidesteps the problem by *eliminating* shared state at the OS level. Every task has private memory; tasks communicate by message-passing. The kernel itself has no dynamic shared state to protect — its state is static configuration declared at build time.

This is brilliant for embedded firmware and works extremely well for Hubris's design space. It rules out shared-cache designs, performant memory allocators that share free lists, anything where the fundamental performance argument requires shared state. It also doesn't address the question — it changes the topic.

## What Clifford does

Decision #21 extends the existing Cl(0,0,n) orthogonality engine to a **mixed-metric Cl(p,0,n) algebra** in which:

- **Private fields** (the v0.1 default; AST `FieldKind::Private`) contribute null basis vectors. Their wedge collapses on overlap. *Current race-detection behavior, unchanged.*
- **Shared fields** (the v0.7+ `#shared` qualifier; AST `FieldKind::Shared { lock }`) contribute non-null basis vectors. Their wedge does *not* collapse on overlap. Overlap on a shared basis vector is permitted; it generates a separate proof obligation: the lock guarding the shared resource is held by both concurrent contexts.

The locking discipline is itself algebraic, not procedural:

- Each lock `L` is a mixed-grade multivector `lock(L) = pri(L) + e_L`, where `pri(L)` is the lock's priority (an integer in the same priority space as `#interrupt #priority:` declarations) and `e_L` is the lock's identity basis vector.
- The lock-context multivector held by an executing automaton is the wedge of every held lock: `ctx = lock(L₁) ∧ lock(L₂) ∧ … ∧ lock(Lₙ)`.
- Acquisition validity falls out of the wedge product. Ascending-priority acquisition produces a canonical wedge; descending-priority is Koszul-flippable; equal-priority falls through to a *rotor* parameterised by a canonical structural attribute of each lock (MMIO `#address` for register-block locks; `#rotor:` clause / link-section position / source-location hash for software locks).
- **Theorem (sketched in §7.9 of the spec, ADR 0002 §5.5.4):** the lock-context multivector never collapses to zero ⟺ execution is deadlock-free.

Four safety properties unify under this single algebraic statement:

1. **Disjoint-mutation safety** — the existing v0.1 check, expressed as null-subspace wedge non-zero.
2. **Shared-state safety** — non-null subspace overlap discharges the lock-coverage proof obligation.
3. **Deadlock-freedom / lock-ordering safety** — the lock-context wedge non-zero (priority + rotor).
4. **Interrupt/lock unification** — an `#interrupt #priority: N { … }` is a priority-ordered acquisition under the §5.5 algebra; the engine handles both interrupt concurrency and explicit-lock concurrency with the same machinery.

## A worked example

The example from Chapter 1 made concrete:

```clifford
#lock plic_lock         #priority: 7;
#lock uart_lock         #priority: 7;
#lock notification_lock #priority: 5 #rotor: 0x0001;

#shared #automaton Plic {
  #address: 0x0c00_0000;     // <-- this IS the rotor parameter
  #lock:    plic_lock;
  #basis:   plic_basis;

  threshold: u32 #offset: 0x020_0000 #access: read_write;
  claim:     u32 #offset: 0x020_0004 #access: read_write;
}

#shared #automaton Uart {
  #address: 0x1000_0000;     // <-- different MMIO addr → different rotor angle
  #lock:    uart_lock;
  rbr_thr: u8 #offset: 0x00 #access: read_write;
}

#shared #automaton NotificationTable {
  #lock: notification_lock;
  irq_to_notif: [u32; 64];
}

#interrupt SupervisorExternal() #priority: 7
                                #mutates: [Plic, Uart, NotificationTable] {
  // Phase 1: drain hardware (priority-7 locks only)
  let irq: u32 = 0u32;
  let byte: u8 = 0u8;
  let pending: bool = false;
  
  #with_lock(plic_lock) {
    irq = #volatile_load<u32>(Plic.claim);
    if irq == 10u32 {
      #with_lock(uart_lock) {
        // Wedge: (7 + e_plic) ∧ (7 + e_uart)
        // pri match → rotor tiebreak: addr(plic) < addr(uart) → canonical ✓
        byte = #volatile_load<u8>(Uart.rbr_thr);
        pending = true;
      }
    }
    Plic.claim = irq;
  }
  // Priority-7 locks released here.
  
  // Phase 2: notify userspace (priority-5 lock; safe now that no high-pri held)
  if pending {
    #with_lock(notification_lock) {
      // Lock context starts fresh: (5 + e_notif). Empty before. ✓
      NotificationTable.irq_to_notif[irq] = byte;
    }
  }
}
```

The engine's static walk computes the lock-context multivector at each program point. **It never collapses to zero.** Theorem 5.5.4 says: this execution is deadlock-free. Compile-time guarantee.

The same code, written naively (acquiring `notification_lock` *inside* the priority-7 critical section), produces:

```
error[E0521]: deadlock-prone lock acquisition
   --> uart.cl:6:9
    |
  4 |   #with_lock(plic_lock) {
    |              --------- acquired here at priority 7
  6 |       #with_lock(uart_lock) {
    |                  --------- acquired here at priority 7
  7 |         #with_lock(notification_lock) {
    |         ^^^^^^^^^^^^^^^^^^^^^^^^^^^^^ priority-5 lock acquired
    |                                       while holding priority-7 locks
    |
    = note: lock-context wedge is anti-canonical at this point
    = note: another hart acquiring `notification_lock` and then waiting for
            `plic_lock` would deadlock against this handler
    = help: defer the notification work until after the priority-7 locks
            release; see the two-phase IRQ pattern (docs/concurrency.md)
```

## Why this works (the mathematical sketch)

The general Clifford algebra Cl(p,q,r) over a field of basis vectors {e_i} has each basis vector squaring to ±1 or 0 depending on which subspace it lives in. The current engine implicitly works in Cl(0,0,n) — every basis vector squares to zero. The proposal extends to Cl(p,0,n): some basis vectors are null (private fields), some are non-null (shared fields, with `e_~q² = +1`).

The wedge product in this mixed algebra has the property we want:

- Two private basis vectors `e_q ∧ e_q` → 0 (collapse: race detected)
- Two shared basis vectors `e_~q ∧ e_~q` → +1 (does not collapse: shared access permitted)

The collapse-vs-non-collapse distinction is the *type-level* signal of "is this overlap a race or a discharge obligation?". The discharge obligation — the lock-coverage proof — is then a separate static check whose machinery is itself another wedge (the lock-context wedge), with the same collapse-detection semantics applied to the *lock* basis vectors.

Two algebras, one wedge-detection mechanism, three safety properties checked. The algebra unifies what would otherwise be three disjoint static analyses.

The rotor for same-priority disambiguation is a deliberately conservative move. Same-priority locks could in principle be ordered by integer comparison on address (the simple answer), but the rotor formulation generalises — to multi-dimensional priority, to cyclic priority spaces, to translation-invariant relative-position checks — without changing the algebra. This is the "doors we keep open" argument from ADR 0002 made concrete: rotor today costs ~50 LoC and a docstring; rotor tomorrow buys us extensions that would otherwise require re-architecting the engine.

## Trade-offs

What we give up by adopting Decision #21:

**Spec complexity.** The §7 of the spec went from "Cl(0,0,n) and the bitmask check" (compact, decidable, easy to teach) to "mixed-metric Cl(p,0,n) with rotor tiebreaks" (compact in concept, dense in mathematical prerequisites). A reader who is comfortable with bitmask race-detection will need to internalise the wedge-product extension to understand v0.7+. The Chapter 24 (geometric-algebra primer) and Chapter 27 (mixed-metric extension) of this book exist to help.

**Implementation complexity.** v0.7's `crates/ortho` will need to handle the metric tag per basis vector (we've reserved space for it in the AST so this is a non-breaking addition, but it is still real engineering work). The lock-context computation is a new pass; the rotor evaluation is new code; the diagnostics need new error codes and new help text. Estimated v0.7 work: 2–4 person-months for the engine + ~1 month for the diagnostic surface.

**Conceptual surface area for users.** A user can write fully-private Clifford code and never need to know §7.0 of the spec exists. But the moment they declare a `#shared` field, they are in the new algebra, and they need to understand priority and rotor and the deadlock-freedom theorem, or at least enough of it to write code the engine accepts. We mitigate this with documentation and good diagnostics, but the cognitive cost is real.

## Doors we keep open by Phase-1 scaffolding

Per ADR 0002's *Doors we keep open* table, the v0.6 scaffolding (which has shipped in `main` as of commit `1321721`) does several things explicitly so that the v0.7 implementation does not need to refactor:

| Thing scaffolded | Why |
|---|---|
| `FieldKind` enum on `AutomatonField` | Adding `Shared { lock }` is a non-breaking AST change |
| `FieldKind` marked `#[non_exhaustive]` | Downstream pattern matches don't need revisiting |
| Tokens `#shared`, `#lock`, `#with_lock`, `#reads`, `#rotor` reserved in lexer | Source compatibility holds across the v0.7 enable |
| §7.0 prologue + §7.9 sketch in the spec | Implementers and users see the v0.7 plan from v0.6 onward |
| Decision #21 LOCKED in `DECISIONS.md` | Future decisions know this territory is taken |

Cost of v0.6 scaffolding: ~150 LoC + spec edits. Cost of *not* doing it: weeks of refactoring once v0.7 work begins.

## Why this is the right design

The honest case for Decision #21, as opposed to the alternatives (Option A through D in ADR 0002):

**Against Option A (per-automaton shared lock):** too coarse. An automaton with one shared field and ten private fields would have all ten "promoted" to shared, losing the disjoint-mutation guarantee on the other nine.

**Against Option B (Option A + verified lock-ordering as a separate procedural check):** the §5.5 rotor formulation absorbs lock-ordering into the algebra. Splitting them into two proof systems means two places to be wrong.

**Against Option D (don't extend; force kernel work into `#unsafe_shared` blocks):** preserves the orthogonality theorem unchanged, but forces every kernel-style program (cspace, sched, page allocator, IRQ binding) into audit-only mode. The strongest case Clifford could make — that kernel code is statically verifiable — would be foreclosed.

The chosen design (per-field shared/private, with rotor-resolved lock-ordering, in a unified mixed-metric algebra) has the highest expressivity for the smallest conceptual cost we found.

## What this enables practically

Beyond the headline safety property, Decision #21 enables a few concrete things that were not possible under v0.1:

- **Wari's `INV-23` "IRQ Routing Determinism" becomes a typecheck.** The IRQ binding table is `#shared` with an implicit lock at the IRQ source's PLIC priority. Any handler at a different priority trying to write the table fails the wedge-non-zero check. The English invariant in the module docstring is replaced by a static guarantee.
- **The two-phase IRQ pattern is enforced, not documented.** "Drain hardware, release high-priority locks, then touch software state" is a discipline every kernel writer learns through scar tissue. Under Decision #21 the engine catches the violation at compile time.
- **Cross-hart shared-state designs (run-queue, page allocator, RCU caches) are expressible.** Their soundness is checked rather than asserted.
- **The interrupt/lock unification eliminates §7.3's special-case handling** of interrupt concurrency, simplifying both the spec and the engine.

## Literature

The chapter draws on (cite-only to the bibliography for full entries):

- *Geometric algebra foundations:* [Dorst, Fontijne, Mann 2007], [Doran, Lasenby 2003], [Hestenes, Sobczyk 1984], [Lounesto 2001].
- *Mixed-metric Clifford algebras:* [Lounesto 2001] §17, [Vaz, da Rocha 2016].
- *Koszul signs / exterior algebras with poset structure:* [Stanley 1996], [Miller, Sturmfels 2005].
- *Concurrent separation logic / RustBelt:* [Jung et al. 2018], [Jung et al. 2015 Iris].
- *GhostCell as direct cousin:* [Yanovski et al. 2021].
- *seL4 capability proofs as proof-of-concept that this property is checkable:* [Klein et al. 2009].
- *Lock-ordering / deadlock theory:* the Linux lockdep documentation, [Chess et al. 2002] *On the Correctness of Lock-Free Algorithms*.
- *Hubris architecture (the not-shared alternative we considered):* the Oxide Computer engineering blog series.

## Cross-references

- **Spec:** §7.0 (algebra prologue), §7.9 (mixed-metric extension)
- **DECISIONS.md:** Decision #21 LOCKED entry
- **ADR:** [`docs/adr/0002-shared-automata-mutator-multivectors.md`](https://github.com/westerngazoo/Clifford/blob/main/docs/adr/0002-shared-automata-mutator-multivectors.md) — the full design exposition
- **Related chapters:**
  - Chapter 8 (Decision #5) for the automaton-as-category foundation
  - Chapter 14 (Decision #13) for body-scoped references (orthogonal to shared automata)
  - Chapter 24 (Geometric algebra primer) for the algebraic prerequisites
  - Chapter 27 (Mixed-metric extension) for the mathematical detail
  - Chapter 28 (Rotors) for the same-priority tiebreak machinery
  - Chapter 29 (Stanley–Reisner — the road not taken) for the alternative algebraic encoding we considered
  - Chapter 36 (GA orthogonality engine) for the v0.7 implementation when it lands
