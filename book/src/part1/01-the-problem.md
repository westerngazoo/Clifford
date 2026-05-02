# Chapter 1: The problem

> *"You can't have a system without a system of accounting."*  
> — anonymous, kernel-development hallway, repeated since at least the 1970s

## The thing every systems language has to answer

A systems language is one whose user takes responsibility for the things a higher-level language hides: stack frames, heap layout, register-mapped I/O, interrupt handlers, the bare metal. The language exists to constrain what the user can express, so that the things they *do* express compile into machine code that does not surprise them. The constraints are the language. The expressivity is, in some sense, a consequence.

Of all the things a systems language has to constrain, **concurrent shared-state mutation** is the one that consistently kills people.

It killed people in the Therac-25 in 1985 (six confirmed deaths, several more injuries) when a race between an operator's keystrokes and the machine's setup code allowed a 25 keV electron beam to fire without the X-ray attenuator in place. It came close to killing people in the Toyota electronic throttle control over the 2002–2009 model years (the litigation cost Toyota $1.2 B; the engineering verdict was tasks accessing global state without lock discipline). It contributed to the 2003 Northeast blackout (race condition in GE's XA/21 alarm system; 50 million people without power for two days). It is the central concern of every kernel post-mortem since SMP became standard, and it is what makes Linux's lockdep one of the largest single piece of validation machinery in the kernel.

This is not a fringe problem. **It is the central problem of writing systems software in 2026.** Every other thing the systems language constrains exists because, given a stack frame and a register file, a programmer can fairly reliably *read what they wrote*. Given a shared mutable variable and two threads, they cannot.

## Why the problem persists

Three things conspire to make concurrent shared-state mutation hard.

**The first is that it is invisible at the syntactic level.** A statement like `counter += 1` looks identical whether `counter` is a stack-local, a thread-local, a properly-locked global, or an unprotected shared variable accessed from an interrupt handler. The compiler sees the statement; the property the program needs to satisfy lives in the entire program's *behavior*, not in any single statement.

**The second is that it is invisible at the runtime level until it manifests.** A race condition that loses an update one time in 10,000,000 will not be caught by testing. It will be caught by a customer six months after release. Even tools that *can* catch it — Helgrind, ThreadSanitizer, Valgrind — catch it only along execution paths the test suite explores, which is a vanishing fraction of all paths. Hardware memory models (the actual rules a CPU follows when reordering loads and stores) make the problem worse: a program that races may not exhibit observable bad behavior on a single x86 desktop and yet *catastrophically fail* on the same code on an ARMv8 server with weaker memory ordering.

**The third is that solving it requires reasoning about the entire program at once.** A race is a property of *two* code locations interacting through shared state. You cannot prove "this function is race-free"; you can only prove "this function, in the context of every other function that might run concurrently with it, does not race." Local reasoning, the principle that lets you understand a function by reading only the function, fails. This is why concurrency bugs disproportionately occur where one engineer modifies code another engineer wrote — neither has the global picture.

## What the standard answers cost

A short and unkind summary of the field, expanded in Chapter 2:

- **C with locks** asks the programmer to maintain the discipline manually. They mostly fail. Linux locks are correct because they are checked at runtime by lockdep, not because they are correct by construction.
- **Java's monitors and Go's mutexes** put locking in the language but check nothing about which lock protects which data; nothing prevents you from accessing a field outside its lock.
- **Rust's borrow checker** prevents data races *between threads* using ownership semantics, but the cost is high — `&mut` is single-borrow, `Send`/`Sync` is hand-implemented per type, and a great deal of perfectly-safe shared-state patterns (RCU, hazard pointers, lock-free data structures) require `unsafe`. Rust trades expressivity for the guarantee.
- **Erlang's actor model** eliminates shared state entirely by giving each actor private state and message-passing. This works beautifully for distributed systems and badly for kernels and high-performance code, where the overhead of message-passing is unacceptable.
- **seL4's capability proofs** offer the strongest guarantee — every operation on every shared resource is mediated by a capability whose authority is proven in Isabelle/HOL — but at the cost of person-years of proof work per kernel, written in a separate proof language, with the proofs decoupled from the implementation language.
- **Iris and the RustBelt project** have proven Rust-style concurrent code correct in concurrent separation logic, including standard library primitives like `RwLock<T>`. The proofs are profound; they are also separate from the code they verify, written in Coq, and have not yet found a path to the working programmer.
- **Hubris's static task table** sidesteps shared state at the OS level — every task's data lives in private memory, communication is by message — at the cost of a particular kernel architecture choice that is excellent for embedded firmware and unworkable for shared-cache designs.

Each of these is correct. Each is incomplete. None of them say: *here is a property of your program that the type system can check at compile time, in the same vocabulary as everything else the type system checks, with diagnostics in source code line numbers, with no separate proof language.*

That is the gap Clifford targets.

## What Clifford claims

In one sentence:

> *Concurrent shared-state safety reduces to a wedge-product non-zero check in a mixed-metric Clifford algebra over priority-graded basis vectors.*

Unpacked, this means:

1. Every automaton's mutable state contributes a *basis vector* to a Clifford algebra. Two automata are concurrent-safe if their behavior multivectors have a non-zero wedge product.
2. Private fields contribute *null* basis vectors; their wedge collapses on overlap (the existing race-detection behavior, current §7 of the spec).
3. *Shared* fields contribute *non-null* basis vectors; their wedge does not collapse on overlap. Instead, overlap discharges a separate proof obligation: the lock guarding the shared resource must be held by both contexts.
4. Each lock is itself a mixed-grade multivector `lock(L) = pri(L) + e_L`. The lock-context multivector held by an executing automaton is the wedge of every held lock.
5. Acquisition validity falls out of the wedge product's behavior under priority. Ascending-priority acquisition is canonical wedge; descending is Koszul-flippable; same-priority falls through to a *rotor* parameterised by a structural attribute of the lock (MMIO address, link-section position, source-location hash).
6. The lock-context multivector never collapsing to zero is *equivalent* to the execution being deadlock-free. This is a theorem; the engine checks it statically.

Disjoint-mutation safety, lock-coupled shared-state safety, deadlock-freedom, and interrupt/lock unification become *one* algebraic statement, checkable in `crates/ortho` by the same code path that already checks the existing v0.1 disjoint property. There is no separate procedural lock-ordering pass. There is no separation-logic proof obligation written in a different language. There is no runtime check.

## What this buys, in concrete kernel terms

Take the example from the previous chapter (which is the canonical example we'll return to throughout the book):

```clifford
#interrupt SupervisorExternal() #priority: 7
                                #mutates: [Plic, Uart, NotificationTable] {
  #with_lock(plic_lock) {
    let irq: u32 = #volatile_load<u32>(Plic.claim);
    if irq == UART_IRQ_NUMBER {
      #with_lock(uart_lock) {
        let byte: u8 = #volatile_load<u8>(Uart.rbr_thr);
        #with_lock(notification_lock) {       // <-- bug: priority inversion
          NotificationTable.irq_to_notif[irq] = byte;
        }
      }
    }
    Plic.claim = irq;
  }
}
```

This is the kind of code a competent kernel engineer might write while exhausted at 2am. The PLIC lock is held throughout (priority 7). The UART lock is acquired inside it (also priority 7, distinguished by MMIO address, OK). The notification lock is acquired inside *that* (priority 5 — *priority inversion*, bug). On a single-hart system this might never visibly fail; on a multi-hart system it deadlocks the first time another hart holds the notification lock and then waits for the PLIC.

In Linux, this bug is caught — when it manifests in production — by lockdep printing a warning. By the time it manifests in production it has shipped. In Clifford, the engine's static walk computes:

```
ctx_before_inner = (7 + e_plic) ∧ (7 + e_uart)        max-priority-in-ctx: 7
new_acquisition  = (5 + e_notif)                       new-priority: 5
                                                       5 < 7 → priority inversion
                                                       wedge picks up Koszul-flip
                                                       acquisition is anti-canonical
```

…and emits:

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
```

The bug never ships. The author sees it the first time they run `cliffordc` on the source. The fix — the *two-phase IRQ pattern*, where you finish all hardware work before touching software state — becomes obvious from the diagnostic's `note:` and `help:` text.

This is the core proposition of Clifford. **The kind of bug that takes lockdep three years of production runtime to surface, the typechecker catches at compile time, with a diagnostic in source-code line numbers.** Not via a separate proof language. Not via runtime instrumentation. Not via a "we've verified this kernel" PDF. Via the same compile pass that checks your `let x: u32 = "hello"` — *because they are the same kind of check, in the same algebra, using the same machinery*.

## Why this has not been done before

Three communities have circled the problem from different sides:

The **geometric algebra community** (Hestenes, Dorst, Lasenby, et al.) has built a beautiful mathematical framework for reasoning about geometry and physics in any dimension, with applications spanning computer graphics, robotics, and quantum mechanics. They have not, to my knowledge, applied it to programming-language semantics.

The **programming-languages community** (Pierce, Harper, Plotkin, the Iris team, the RustBelt team) has built type theory, separation logic, and effect systems that can in principle prove anything about a program's behavior, including its concurrency properties. They have done so by inventing their own algebras (the BI logic of separation logic, the Iris monoid framework) rather than by adopting an existing geometric algebra.

The **systems community** (the Linux kernel team, the seL4 team, the Tock team, the Hubris team) has built kernels whose lock disciplines are *specified in English* in module documentation and *enforced in C or Rust*, with verification via separate efforts that scale poorly to evolving codebases.

The combination — a real systems language whose static checker uses Clifford algebra — has not been published. The reason is not that anyone tried it and decided it would not work. The reason is that the people who knew the geometric algebra mostly did not know they should look at programming languages, and the people who knew the programming languages mostly used the algebras their own community had already invented.

That is the gap. The book in your hands is, in part, the case that the gap is real and that closing it produces something useful. Subsequent chapters make the case in detail.

---

**Cross-references:**
- The full mathematical statement is Chapter 26 (the orthogonality theorem) and Chapter 27 (the mixed-metric extension).
- The five-line worked example above is expanded in Chapter 21 (Decision #21).
- The conventional answers are surveyed in Chapter 2.
- The geometric-algebra angle is introduced in Chapter 3 and developed in Chapters 24, 26, 27, 28.

**Further reading from the bibliography:**
- *Therac-25*: [Leveson & Turner 1993] *An Investigation of the Therac-25 Accidents*. IEEE Computer 26(7).
- *Toyota Unintended Acceleration*: [NASA / NHTSA 2011] *Technical Assessment of Toyota Electronic Throttle Control (ETC) Systems*. NASA Engineering and Safety Center.
- *2003 Blackout*: [U.S.–Canada Power System Outage Task Force 2004] *Final Report on the August 14, 2003 Blackout in the United States and Canada*. Section on the GE XA/21 alarm system race.
- *RustBelt*: [Jung et al. 2018] *RustBelt: Securing the Foundations of the Rust Programming Language*. POPL.
- *seL4*: [Klein et al. 2009] *seL4: Formal Verification of an OS Kernel*. SOSP.
