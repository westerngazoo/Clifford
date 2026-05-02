# Chapter 2: The conventional answers

> **Status:** Stub. Full chapter pending.

A survey of how mainstream systems languages handle concurrent shared-state safety.

**Coming in book v0.2.** Will cover:

- C with locks (the discipline-as-documentation approach)
- Java/Go monitors and mutexes
- Rust's borrow checker + `Mutex<T>` / `RwLock<T>`
- Erlang's actor model
- seL4's capability proofs (Isabelle/HOL)
- Iris and the RustBelt project (Coq proofs of Rust soundness)
- Hubris's static-task-table approach
- GhostCell's brand-types

Each gets a fair treatment of what it gets right and where it leaves gaps.

**Cross-references:** Bibliography §2, §5, §8, §9, §12.

