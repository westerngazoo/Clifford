# ADR 0006: Runtime Distributed Race & Deadlock Detection via Dynamic Multivector Check

**Status:** Proposed (2026-05-05)
**Date:** 2026-05-05
**Deciders:** Goose (architect)
**Spec impact:** None on the core language. New plugin crate (`crates/dist-check`) and a runtime instrumentation hook in `crates/codegen`. Spec §10 gains a new error-code range (E07xx — runtime diagnostics) reserved for plugin consumers.
**DECISIONS.md:** No new Decision number; this is plugin-layer infrastructure that consumes the existing GA primitives.
**Predecessor ADRs:** ADR 0002 (mixed-metric GA, Decision #21), ADR 0005 (rotor-plane locks, Decision #26).
**Branch:** `adr/0006-runtime-distributed-multivector-engine`.

---

## TL;DR

The compile-time orthogonality engine (§7) proves disjoint mutation
for **statically-known callables in a single process**. It cannot
reason about:

- **Distributed peers** the compiler never saw (a Rust service
  talking to your Clifford service over RPC).
- **Dynamic resource sharding** (which node owns which key in a
  hash-partitioned table is a runtime decision).
- **Cross-process coordination** (two nodes both think they hold "the
  leader lock" because of a stale lease).

This ADR proposes a **runtime check using the same wedge-product
primitive**, scoped to debug / plugin mode. Each distributed node
publishes its current "behaviour multivector" to a coordinator (or
gossip layer); the coordinator computes pairwise wedge products on
every join / mutation; any collapse (`b_i ∧ b_j = 0` over a shared
basis vector) is a *race detected at runtime* with a diagnostic in
source-level identifiers.

Same algebra. Same `&` instruction. Same diagnostic shape ("nodes
N₁ and N₂ both wrote `Resource.slice_42`"). The only thing that
changed is *when* the check runs. The compile-time engine remains
unchanged; this is purely additive.

**Key design constraint:** zero impact on release builds. The
runtime check is opt-in via a `#[cliffordc::dist_check]` attribute
or `cliffordc test --dist-check` flag; release builds elide the
publish/check/retract instrumentation entirely. The compile-time
engine never depends on the runtime check existing.

**Recommendation:** Lock as Phase 5+ work (v0.4 / v0.5 alongside
`clifford::core::sync` and any networking stdlib). Prerequisites:
Decisions #21 / #26 implementation (v0.7+) for the in-process lock
ground truth that the distributed check sits on top of.

This ADR is **Proposed**, not **Accepted**. Five open questions in
§6 must close before locking.

---

## Context

### What the compile-time engine does and doesn't do

§7 / Decision #4 / Emergent Rule 1 give us:

```
behavior(callable) = { (automaton, field) bits the callable writes }
∀ concurrent pair (A, B):
    behavior(A) ∧ behavior(B) ≠ 0  ⟺  no race
```

Computed at compile time over the **static call graph**, with
`actual_writes` extracted by §6.2.

This is sufficient — and provably sound — for **single-process
in-memory** Clifford programs. Decision #21 / ADR 0002 extends it
to in-process *shared* state via `#shared` fields and the
mixed-metric algebra. Decision #26 / ADR 0005 extends it again to
in-process locking via rotor-plane confinement.

What it does not extend to:

- **Multi-process.** Two Clifford processes on the same machine
  sharing memory via `mmap` or talking via Unix sockets — the
  compiler sees one program, not two.
- **Multi-machine.** Distributed services coordinating via gRPC,
  Kafka, etcd, or anything network-mediated.
- **Mixed-language.** A Rust frontend writing to a database that a
  Clifford backend also writes — Clifford's engine has no view of
  the Rust write.

For these cases the compile-time engine is *strictly silent*. It
neither accepts nor rejects; it simply does not see the
cross-boundary writes.

### What the user observed

> "i was thinking also again in the distributed engine check maybe
> it could not garantee anything on shared resources or data races
> at compile time but it could have a plugin or debug mode check.
> aint could we use the multivector properties it gives us
> orthogonality and parallel multivector"

This ADR formalises that intuition. Three terms to map:

- **"Could not guarantee anything at compile time"** — correct;
  the network introduces non-determinism the compiler cannot
  observe.
- **"Plugin or debug mode check"** — exactly the right scope.
  Runtime instrumentation, opt-in, off in release builds.
- **"Multivector properties... orthogonality and parallel
  multivector"** — the wedge primitive (orthogonality) and the
  product-category structure (parallel composition from Appendix B).
  Both already exist in the engine. Lift them from "static check"
  to "runtime predicate" — same math, different lifecycle.

### Why this is interesting (vs just "use a normal distributed tracer")

Three properties:

1. **Algebraic continuity.** The runtime check uses the *same*
   wedge primitive the compile-time engine already runs. No new
   algebra. A user who understands `behaviour(A) ∧ behaviour(B) =
   0 ⇒ race` at compile time understands it at runtime.
2. **Source-level diagnostics.** When the runtime check fires, the
   diagnostic is `(LeaderLock, slot_3)` — the same source-level
   identifier the compile-time E0520 would name — not some opaque
   `violated invariant 0x7f`.
3. **Encapsulation symmetry.** Decision #25 said "encapsulation is
   the bit isn't there for outsiders to refer to." Decision #26
   said "mutual exclusion is the plane isn't there for outsiders
   to rotate into." This ADR adds: "distributed safety is the bit
   isn't claimed by outsiders at the same time." All three are
   *algebraic-trivial* properties of the GA core.

---

## Proposal

### 1. The runtime data flow

Each distributed node `N` carries a runtime structure:

```
behaviour(N) = bitmask of (resource, slice) pairs N is currently mutating
state(N) = { Idle | Acquiring | Mutating | Releasing }
```

A coordinator (or gossip layer) maintains:

```
active = { (N_i, behaviour(N_i)) : N_i.state ∈ { Mutating, Acquiring } }
```

Lifecycle of a mutation phase:

```
1. N.publish(intended_bits):
   propose: active' = active ∪ { (N, intended_bits) }
   ∀ (N', b') ∈ active where N' ≠ N:
       if intended_bits & b' ≠ 0:                   ← wedge collapse
           reject; emit E07xx race diagnostic
   commit: active ← active'

2. N performs the mutation.

3. N.retract():
   active ← active \ { (N, behaviour(N)) }
```

The wedge check is the same `&` operation as §7.4; the only
difference is `active` is a runtime set rather than the static
basis-assignment table.

### 2. Coordinator topology (Q1 in §6)

Three options:

- **Central coordinator.** One process maintains `active`; all
  nodes publish/retract via RPC. Simplest; single point of
  failure; lowest latency under no contention; doesn't scale past
  ~10⁴ nodes.
- **Gossip / CRDT.** `active` is a join-semilattice; nodes
  exchange deltas. Eventual consistency; tolerates partitions; no
  single point of failure; race detection is *eventually*
  correct, not *immediately*.
- **2PC / consensus (Raft / Paxos).** `active` is replicated by
  consensus. Strong consistency; high latency; complex.

For **debug mode in CI / staging**, the central coordinator is the
right default: simple, low-latency, fail-stop. For **production
distributed audit**, gossip is more realistic.

Proposed (Q1 resolution): central coordinator for v0.4-α; gossip
backend pluggable via trait in v0.5+.

### 3. Diagnostic shape

When `intended_bits & b' ≠ 0` at publish time:

```
E0701 DistributedRace at byte X in <source>:
  Node `service-a@host1.cluster:7474` (active behaviour
  Behaviour { LeaderLock.slot_3, WriteCache.key_5 })
  conflicts with previously-publishing node
  `service-b@host2.cluster:7474` (active behaviour
  Behaviour { LeaderLock.slot_3 }) on shared bit `LeaderLock.slot_3`.

  Both nodes are mutating the same logical resource concurrently.

  See `cliffordc::dist::Behaviour::wedge` for the algebraic
  derivation, or run `cliffordc audit --dist-trace` for the full
  publish-retract timeline of both nodes.
```

The diagnostic names the conflicting resource by its source
identifier (`LeaderLock.slot_3`), names both offending nodes, and
points the user at audit tooling.

### 4. What this ADR is NOT

- **Not a replacement for compile-time `#shared` + locks.** The
  compile-time machinery from Decisions #21 / #26 still runs and
  catches in-process races at compile time. This ADR is for
  *cross-process* races where no compiler can see both sides.
- **Not a recovery mechanism.** Detection ≠ prevention. The race
  has already happened by the time the publish RPC returns. The
  app must decide to retry, roll back, or kill the conflicting
  node — the framework just provides the detection.
- **Not coupled to a specific runtime.** No assumption about
  threading model, async runtime, or transport (RPC / Kafka /
  shared memory). The instrumentation hook is an interface; the
  coordinator backend implements it.
- **Not on by default.** Requires explicit opt-in via attribute
  or flag. Release builds elide instrumentation entirely.

---

## Cost analysis

### At runtime, off (release build)

**Zero cost.** No instrumentation emitted; codegen elides the
publish/check/retract calls entirely (analogous to how Rust elides
`#[cfg(debug_assertions)]` blocks in release builds).

### At runtime, on (debug / `cliffordc test --dist-check`)

Per mutation phase:

- 1× publish RPC to coordinator (network round-trip; ~100 µs LAN,
  ~10 ms WAN)
- 1× wedge check at coordinator (O(|active|) bitwise AND ops;
  microseconds)
- 1× retract RPC (same as publish)
- ~10× to ~100× the bare write cost on the hot path

This is debug-mode cost; production should run release builds
which elide all of it. For CI / staging environments, this is
acceptable for catching the bug class the static engine can't.

### Coordinator load

Central coordinator handles `2 × N × R` RPCs per second where `N`
is node count and `R` is per-node mutation rate. For typical CI
loads (10s of nodes, 10s of mutations/sec each), this is ~kRPS —
trivial for a single coordinator. Beyond ~10⁴ nodes or ~10⁵ RPS,
gossip backend recommended.

### Compile-time cost

**Zero.** The compile-time engine never reads the dist-check
infrastructure; the plugin lives in its own crate (`crates/dist-
check`) and registers a codegen hook that activates only when the
opt-in is set. No impact on `cargo build` of a Clifford program
that doesn't use it.

---

## Open questions (must close before Accepting)

### Q1. Coordinator topology

Central vs gossip vs consensus. **Proposed:** central for v0.4-α;
gossip backend pluggable via trait in v0.5+.

### Q2. Behaviour publication scope

Per-mutation (every write announces) vs per-transaction (mutation
phases batched) vs per-session (long-lived behaviours)?

**Proposed:** per-transaction for v0.4-α. A "mutation phase" is a
contiguous set of writes inside an `#effect` body or an explicit
`@dist_phase("name") { … }` block. Per-mutation is too noisy for
realistic apps; per-session loses precision.

### Q3. Race response

Log-only (post-mortem) / abort the offending mutation
(consistency) / quarantine the node (CAP partition)?

**Proposed:** Configurable per `#rotor_lock` declaration via a
`#on_dist_race: Log | Abort | Quarantine` annotation. Default
**Log** (least-disruptive for debug; surfaces in `cliffordc audit
--dist-trace` output).

### Q4. Resource basis assignment in distributed

Compile-time the basis assignment is `(automaton, field) → bit`,
known statically. Distributed: how do nodes agree on which bit is
`(LeaderLock, slot_3)` vs `(WriteCache, key_5)`?

**Proposed:** Pre-agreed schema at link time. A
`clifford::dist::Schema` artefact is generated from each
participating program's `#shared` declarations and exchanged at
deployment. Schema mismatches are `E0702 SchemaIncompatible`. (See
also: similar question for cap'n proto schema versioning.)

Future work: runtime schema registration for dynamic-resource
cases; deferred to v0.6+.

### Q5. Interaction with `#shared` (Decision #21) + ADR 0005 / #26

Two readings:

- (a) **Stack on top.** In-process: rotor-plane lock from #26 is
  the local truth; cross-node: dist-check publishes the rotor-
  hold to the coordinator so other nodes know the lock is taken
  globally.
- (b) **Replace.** Only do dist-check for resources explicitly
  marked as cross-node (e.g., `#dist_shared` field qualifier).
  In-process resources use Decision #26 only; cross-node
  resources use this ADR only.

(a) is the more general; (b) is more conservative and avoids
double bookkeeping for in-process locks that don't need
distributed visibility.

**Proposed:** (b). Marking a `#shared` field as `#dist_shared`
opts it into the cross-node check; otherwise Decision #26's
in-process rotor-plane machinery is sufficient. This keeps the
runtime cost paid only by the resources that actually need it.

---

## Consequences

### If accepted as proposed

- v0.4+ implementation work: new `crates/dist-check` crate; new
  `#dist_shared` field qualifier (parser / AST / lexer
  reservation); codegen hook for publish/retract instrumentation;
  central coordinator reference implementation.
- Spec §10 gains an `E07xx` error-code range reserved for runtime
  diagnostics from this plugin.
- Book chapter (Part V — Practice) on distributed Clifford
  patterns, alongside the existing Ch. 44 (Wari kernel patterns).
- **The compile-time engine is unchanged.** This ADR's value is
  precisely that it consumes existing GA primitives; no §7
  extension required.

### Doors kept open

- The plugin architecture means alternative coordinator backends
  (etcd, Consul, pure-gossip) can land later without re-spec'ing.
- The `Schema` artefact format is forward-compatible with v0.6+
  dynamic-resource registration.
- No change to release-build semantics — code that doesn't opt in
  pays nothing.

### Doors potentially closed

- Pre-agreed schema (Q4) means programs that share resources must
  agree on basis assignment at deployment. This is similar to RPC
  schema versioning and is the same pain point. Mitigation: the
  schema can be auto-derived from `#dist_shared` declarations and
  shipped as a build artefact.
- The `Log | Abort | Quarantine` race-response choice (Q3) is
  config, not algebra. Users who want a different policy (e.g.,
  custom recovery) need a hook. Mitigation: the response policy
  can be a closure passed to the coordinator constructor.

### If rejected

- The compile-time engine (Decisions #21 / #26) remains the only
  Clifford-native race-detection mechanism. Cross-process /
  cross-machine race detection falls back to standard tools
  (Linux lockdep equivalents, distributed tracing, custom
  audit). Nothing lost in the static side; a categorical
  "everything that fits in one process" boundary remains.
- The "distributed Clifford" use case becomes a downstream
  ecosystem concern, not a language-level feature. Acceptable;
  the language's core claim is single-process correctness.

---

## Implementation milestones

| Phase     | Deliverable                                              | Crate                              |
|-----------|----------------------------------------------------------|------------------------------------|
| v0.4-α    | `crates/dist-check` skeleton: trait `Coordinator`, `Behaviour` type, central-coordinator backend | new                                |
| v0.4-β    | `#dist_shared` field qualifier; lexer reservation; parser/AST | lexer, ast, parser                 |
| v0.4-γ    | Codegen hook: publish/retract instrumentation behind `#[cliffordc::dist_check]` | codegen, dist-check                |
| v0.4      | E0701, E0702 in spec §10; reference implementation passes integration tests | spec, dist-check                   |
| v0.5+     | Gossip backend (CRDT-based)                              | dist-check                         |
| v0.6+     | Dynamic schema registration                              | dist-check, ast                    |
| v0.7+     | Integration with Decision #26 rotor-plane locks (cross-node visibility of in-process holders) | ortho, dist-check                  |

---

## Decision

**Status: Proposed.** Lock after the five open questions in §"Open
questions" close in conversation with the architect. Targeted
close: by 2026-07-01.

Implementation is **Phase 5+ work** (per CLAUDE.md §10's release
roadmap) — no v0.1 / v0.2 dependency. The compile-time engine
ships unaware of this ADR's existence.

---

## Cross-references

- **ADR 0002 / Decision #21** — the mixed-metric machinery whose
  algebra this ADR lifts to runtime.
- **ADR 0005 / Decision #26** — the rotor-plane lock formulation
  that this ADR layers cross-node visibility onto (Q5).
- **Decision #25** — the algebraic-trivial-encapsulation pattern
  this ADR mirrors at the distributed layer (mutual exclusion by
  bit-absence in published behaviour).
- **Spec §7** — the wedge-product primitive that does both
  compile-time and runtime checks.
- **Spec §10** — the error-code table; this ADR reserves the
  E07xx range for runtime diagnostics.
- **Book Ch. 44 (formerly Ch. 40, Kernel patterns / Wari)** — the
  in-process kernel-patterns chapter that this ADR's distributed
  variant complements.

---

*This ADR is Proposed. The intuition is the user's framing
("plugin or debug mode check using the multivector properties");
this document is its formalisation. Locking requires resolving §6.*
