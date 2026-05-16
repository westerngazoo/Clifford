# RESEARCH (deferred): GA Across Scales — Distributed Runtime Race Detection

> **Deferred from `DECISIONS.md` Decision #27 by the 2026-05 audit.**
> Preserved verbatim for the historical record. **Not** normative, **not**
> a v1.0 commitment. Layered on the also-deferred `ga-shared-automata.md`
> (#21) and `ga-rotor-locks.md` (#26). See `docs/research/README.md`.
>
> Tracking ADR (immutable): `docs/adr/0006-runtime-distributed-multivector-engine.md`.

---

## Original text (Decision #27, locked 2026-05-05, deferred 2026-05-16)

### Summary — the unifying claim

Decisions #21 and #26 already established that the GA wedge product proves race-freedom for in-process state — first compile-time, then in-process locking. Decision #27 committed to extending the *same wedge primitive* to **runtime distributed race detection**, scoped to plugin / debug mode.

This was the unifying architectural pattern across #21, #26, and #27:

> **GA is the unifying algebra; standard primitives (CAS spinlocks, flags, RPCs, atomics) are the implementation.**

The same `outer_product` operation was claimed to run at three scales:

| Scale | When | What carries the algebra | What carries the runtime |
|---|---|---|---|
| Compile-time, single-process | `cliffordc` invocation | Static `actual_writes` per callable | (none — pure proof) |
| In-process runtime (#21/#26) | Lock acquire/release | `lock(L) = pri(L) + e_L` multivector cell | Normal CAS spinlock with owner-ID + depth counter |
| Distributed runtime (#27) | Mutation phase publish/retract | `Behaviour { (resource, slice) bits }` | RPC publish + central coordinator + RPC retract; `&` op on coordinator |

### Locked resolutions (per ADR 0006)

- **Q1** Coordinator topology: central for v0.4-α; gossip pluggable for v0.5+.
- **Q2** Publication scope: per-transaction (`#effect` body or `@dist_phase("name") { … }` block).
- **Q3** Race response: configurable per `#rotor_lock` via `#on_dist_race: Log | Abort | Quarantine`; default `Log`.
- **Q4** Resource basis assignment: pre-agreed schema at link time.
- **Q5** Interaction with #21/#26: opt-in per resource via `#dist_shared` field qualifier.

### Scaffolding (none was ever added)

Decision #27 stated `#dist_shared`/`#dist_phase`/`#on_dist_race` reservations "may land alongside the #21/#26 reservations or independently in v0.4-α." They were never added to the lexer, so there is nothing to remove.

### Why it was deferred

`docs/decision-audit-2026-05.md` §5: the decision's own table describes the distributed mechanism as "RPC publish + central coordinator + RPC retract; `&` op on coordinator" — which is optimistic-concurrency-control: publish a read/write set, intersect it at a coordinator, retract. Sinfonia (2007), Calvin (2012), and every modern OCC system already do exactly this. The "GA is the unifying algebra" framing adds nothing operational; it was the marketing thesis the post-pivot project exists to retire. If distributed race checking is ever built, it should be grounded in the OCC literature under its own decision.
