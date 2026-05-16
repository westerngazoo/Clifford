# RESEARCH BET: Synchronous Dataflow `#node` for Loop-Shaped Logic

> **Status:** active research bet, chartered 2026-05-16. **Not** a v1.0
> commitment, **not** normative. Lives in `docs/research/` per that
> directory's charter. This bet has an explicit kill protocol (§4) — if a
> stage gate fails, the bet is abandoned and this document records why.
>
> Origin: the logic/mutation boundary design-space survey (2026-05-16)
> identified synchronous dataflow as the strongest *under-explored* answer
> to how Clifford draws the boundary between pure logic and stateful
> mutation. See `docs/foundations.md` and the survey for context.

---

## 1. The bet

**Hypothesis.** For *periodic, loop-shaped* logic — control loops, sensor
fusion, debouncers, filters — the synchronous-dataflow model (Lustre /
Esterel / SCADE) is a better fit for Clifford than the
`#automaton`/`#effect`/`@snapshot` machinery, because it **dissolves the
logic/mutation boundary instead of making code cross it**.

In synchronous dataflow a program is a set of *equations* over streams.
State is not mutated — it is expressed by *delay operators* (`pre x` =
`x` one tick ago; `e1 -> e2` = `e1` on the first tick, `e2` after). There
are no assignment statements, no mutation, no boundary to snapshot
across: the pure equations *and* the state are the same syntactic
category, distinguished only by whether a delay operator appears. A
compiler statically schedules the equations and emits one allocation-free
`step()` function plus a fixed-size state record. No heap, no GC, no
runtime scheduler.

**Why it is plausibly a strong fit for Clifford specifically:**

- No GC, no runtime — Lustre/SCADE compile to exactly the constrained C
  that DO-178C avionics demands; that is the model's entire reason to
  exist.
- First-order by nature — nodes are not values; this composes with
  Decision #13's removal of closures rather than fighting it.
- The DO-178C-qualified industrial tool in this lineage is **SCADE**.
  Clifford's Decision #5 already cites Stateflow / Esterel / SCADE as its
  FSM lineage — but it borrowed only the state-machine *surface*, never
  the synchronous *model*. This bet tests whether borrowing the model is
  worth it.
- The `@snapshot` verbosity (N explicit boundary-crossing reads before
  any logic) vanishes by construction — there is no separate mutable
  store to read *from*.

**Why it is a bet and not a plan:** synchronous dataflow's natural domain
is periodic control. It is awkward for event-driven, irregular imperative
code (a UART driver's byte-at-a-time ingestion, a one-shot boot sequence,
an allocator) and has no natural expression for memory-mapped registers
(externally-mutated state, which the synchronous model assumes away). So
the bet is **not** "replace Clifford's imperative layer." It is: *can a
`#node` construct live alongside `#automaton` as a third construct kind,
for the loop-shaped subset, compiling through the same disjointness
engine and composing cleanly with the rest of the language?* If `#node`
cannot compose, or cannot lower to allocation-free static code, or does
not actually read cleaner — abandon.

## 2. Minimal `#node` surface (the experiment's language subset)

Lustre core only. No clocks / multi-rate (a later extension if the bet
wins), no Esterel-style imperative control.

```
#node thermostat(temp: f32, setpoint: f32, hyst: f32) -> (heater: bool) {
  var was_on: bool;
  was_on = false -> pre heater;
  heater = if was_on { temp < setpoint + hyst }
           else      { temp < setpoint - hyst };
}
```

- `#node name(inputs) -> (outputs) { var locals; equations }`.
- The body is an **unordered set of equations** `lhs = expr;` — one per
  local and output. Order in source is irrelevant; the compiler schedules.
- `pre e` — the value of stream `e` on the previous tick.
- `e1 -> e2` — `e1` on the first tick, `e2` thereafter.
- Expressions: the existing `@`-layer expression grammar (arithmetic,
  comparison, `if`/`else`, `@fn` calls) plus `pre` and `->`.
- Streams are typed with the existing primitive/composite types.

A `#node` is **pure**: it may call `@fn`s, may not call `#effect`s, has
no I/O. It is referentially transparent as a stream function — same input
streams, same output streams.

## 3. Staged experiment with kill gates

Each stage has an explicit gate. A failed gate = the bet is abandoned;
this document is updated with the failure and `docs/research/` keeps it
as a recorded negative result.

| Stage | Work | Kill gate — abandon if… |
|---|---|---|
| **1. Worked paper design** | Write real control programs as `#node`s; hand-lower one to LLVM IR; stress-test interop with `#automaton`/`#effect`/MMIO/`@fn`. | …the model does not compose with the existing language, OR does not lower to allocation-free static code, OR does not read cleaner than the `#automaton` version. |
| **2. Standalone `node-proto` prototype** | An isolated crate: mini lexer+parser for the §2 subset, causality analysis (dependency graph, reject `pre`-free cycles, topological schedule), LLVM-IR-text emit of `step()` + state struct. Not wired into the main pipeline. | …causality analysis or scheduling proves intractable or ugly, OR the emitted IR is not allocation-free / not target-clean. |
| **3. Judgement** | Compile + run the thermostat `#node` (QEMU, the slice-45 harness shape). Compare against the `#automaton` version on clarity, code size, and composition. | …it does not run, OR is materially worse than `#automaton` on size/clarity. |

If all three pass, the bet is promoted: a real `#node` construct enters
the language via the normal decision + spec + slice process. Until then
nothing touches the main compiler crates.

**Stage 1 is executed below.** Stages 2–3 are gated on its verdict.

---

## 4. Stage 1 — worked paper design

### 4.1 Three programs as `#node`s

**Thermostat** (the §2 example) — bang-bang control with hysteresis.
State: the previous heater output (`pre heater`). One delayed stream.

**Debouncer** — a noisy button input must be stable for 3 ticks before
the output changes:

```
#node debounce(raw: bool) -> (stable: bool) {
  var count: u8;
  var prev:  bool;
  prev   = false -> pre raw;
  count  = 0u8 -> if raw == prev { min(pre count + 1u8, 3u8) } else { 0u8 };
  stable = false -> if count >= 3u8 { raw } else { pre stable };
}
```

State: `pre raw`, `pre count`, `pre stable` — three delayed streams.
Causality: `prev ← {}`, `count ← {raw, prev}`, `stable ← {count, raw}`.
Schedule: `prev`, `count`, `stable`. No same-tick cycle.

**PID-ish integrator** — accumulate error, clamp:

```
#node integ(err: f32, limit: f32) -> (acc: f32) {
  acc = clamp(0.0f32 -> pre acc + err, -limit, limit);
}
```

`acc` depends on `pre acc` (previous tick — not a same-tick dep), `err`,
`limit`. Same-tick deps: none. Schedule: `acc`. The feedback loop is
*broken by `pre`* — this is the normal, intended shape.

All three are pure equations. No `@snapshot`, no `#mutates`, no
read-N-fields ceremony — because there is no separate store.

### 4.2 Causality analysis

Standard Lustre analysis, textbook:

1. Build a directed graph: the equation defining stream `x` has an edge
   to every stream referenced in its RHS **except references syntactically
   under `pre`** (a `pre y` reads the *previous* tick, so it is a
   dependency on state, not a same-tick dependency).
2. A cycle in this same-tick graph is a true algebraic loop → reject with
   a causality error (proposed `E0820 CausalityCycle`), naming the streams
   in the cycle.
3. Otherwise topologically sort → the evaluation order for `step()`.

`x = x + 1` (no `pre`) → rejected. `x = 0 -> pre x + 1` → accepted, the
cycle runs through `pre`. This is decidable, linear, and needs no SMT.

### 4.3 Hand-lowering the thermostat to LLVM IR

State struct — one slot per delayed stream, plus one `first`-tick flag
for the `->` operators (all `->`s in a node share one flag):

```llvm
%struct.thermostat_state = type { i1, i1 }   ; { first, heater_prev }
@thermostat.state = global %struct.thermostat_state { i1 1, i1 0 }

define i1 @thermostat_step(%struct.thermostat_state* %st,
                           float %temp, float %setpoint, float %hyst) {
entry:
  %first_p     = getelementptr %struct.thermostat_state, %struct.thermostat_state* %st, i32 0, i32 0
  %first       = load i1, i1* %first_p
  %heater_prev_p = getelementptr %struct.thermostat_state, %struct.thermostat_state* %st, i32 0, i32 1
  %heater_prev = load i1, i1* %heater_prev_p

  ; was_on = false -> pre heater
  %was_on = select i1 %first, i1 false, i1 %heater_prev

  ; heater = if was_on { temp < setpoint+hyst } else { temp < setpoint-hyst }
  %on_thr  = fadd float %setpoint, %hyst
  %off_thr = fsub float %setpoint, %hyst
  %lt_on   = fcmp olt float %temp, %on_thr
  %lt_off  = fcmp olt float %temp, %off_thr
  %heater  = select i1 %was_on, i1 %lt_on, i1 %lt_off

  ; commit state for next tick
  store i1 0, i1* %first_p
  store i1 %heater, i1* %heater_prev_p
  ret i1 %heater
}
```

Observations:
- **Allocation-free.** The state struct is a fixed-size global (or an
  automaton field — see 4.4); `step()` is straight-line, no `malloc`, no
  stack growth. Exactly what Clifford's no-GC/no-runtime codegen wants.
- The schedule (`was_on` before `heater`) is the topological order.
- `pre` reads load from the state struct at entry; the state commits at
  exit. The whole tick is a pure function of (state, inputs) → (state',
  outputs). This is the textbook synchronous lowering.
- No runtime scheduler — the *caller* drives the tick (its main loop,
  a timer ISR, etc.). Clifford emits the `step()`; the caller calls it.

The IR is the same shape and quality as the existing slice-1..9 codegen
output. No new lowering machinery beyond a state struct and a topological
walk. **The no-runtime / allocation-free claim holds.**

### 4.4 Interop stress test — the part that could break the bet

This is where Stage 1 genuinely tries to *kill* the bet.

**Q1 — Who owns a node's state, and who calls `step()`?**
A node *instance* needs its `%struct.<node>_state`. The clean answer:
**an `#automaton` owns a node instance as a field.** A `#node` becomes a
field type; the owning automaton's effects/transitions call
`node.step(...)`. The disjointness engine then sees the node's state as
part of the automaton's field footprint — no engine change. `step()` is
invoked from `#`-context (an effect, a transition, an interrupt). ✓
Composes.

**Q2 — Can a `#node` read MMIO / hardware registers?**
No — and it should not. The synchronous model takes inputs *as stream
arguments*. Hardware reads stay in the `#`-layer: an `#effect` reads the
ADC register and passes the value as a node input; the node's output
feeds an `#effect` that writes the actuator register. That is *exactly*
the functional-core / imperative-shell split — the `#node` is the
functional core, the `#effect` is the imperative shell doing I/O. ✓
Composes — and cleanly.

**Q3 — Can a `#node` call an `@fn`?** Yes. `@fn`s are pure; a node
equation may call one (`heater = decide(temp, sp, hyst)`). ✓

**Q4 — Can a `#node` call an `#effect`?** No — that would inject
imperative effects into pure dataflow and break the synchronous model.
Clean rule: `#node` ⊂ pure (may call `@fn`, may not call `#effect`). ✓
A simple, enforceable boundary.

**Q5 — How does it interact with the disjointness engine?**
A node instance's state is a footprint (the automaton field holding it).
Two effects calling `step()` on the *same* instance concurrently → a
write-write conflict on that footprint → the existing §7 check catches
it. Different instances → disjoint → fine. **No engine extension
needed.** ✓

**The one genuine tension Stage 1 surfaces.** A `#node` is *pure* (no
effects, no I/O, referentially transparent per stream) yet *stateful*
(the `pre` delay registers). Clifford's `@` layer is defined as
stateless; its `#` layer is defined by imperative mutation. A `#node` is
neither — it is a **third category: pure-but-stateful, where the state is
functional (delays), not mutated.** This is not a flaw; it is the precise
mechanism by which the bet dissolves the boundary. But it confirms the
survey's framing: `#node` is a genuinely new construct kind, not a
variant of `@fn` or `#automaton`. The sigil choice (`@node` vs `#node`)
is therefore a real question — it leans `@` (it is pure) but it has
state, so it cannot simply *be* an `@`-construct. Provisional choice:
`#node`, because instances are owned by `#automaton`s and `step()` is
called from `#`-context; revisit if Stage 3 says otherwise.

### 4.5 Does it read cleaner?

The thermostat, `#automaton` form (today, post-Option-B-or-not), needs an
automaton declaring `heater_state`, an `#effect` that reads inputs, calls
a pure `decide`, and writes `heater_state` back — with the boundary
crossing explicit. The `#node` form is the §2 five-line declaration: the
hysteresis state is `pre heater`, the logic is two equations, there is no
snapshot and no write-back because there is no separate store. For this
class of program the `#node` form is unambiguously clearer and shorter,
and the clarity gap *widens* as the control logic gains state (the
debouncer's three delayed streams would be three `@snapshot`s + three
write-backs in automaton form).

### 4.6 Stage 1 verdict — **PASS, proceed to Stage 2**

The bet survived the paper gate. Against the §3 Stage-1 kill criteria:

- Composes with the language? **Yes** — automaton owns the instance
  (Q1), effects form the imperative shell around it (Q2), calls `@fn`s
  (Q3), the `#node`⊄`#effect` rule is clean (Q4), the disjointness engine
  needs no change (Q5).
- Lowers to allocation-free static code? **Yes** — §4.3, a fixed state
  struct + a straight-line `step()`, same quality as existing codegen.
- Reads cleaner? **Yes** for the loop-shaped class, and more so as state
  grows (§4.5).

Confirmed costs (not kill criteria, but scope boundaries): `#node` is a
genuinely new third construct kind (§4.4), single-rate only in this
experiment (no clocks), and event-driven code + MMIO stay in the
`#automaton`/`#effect` layer by design.

## 5. Stage 2 scope (gated on §4.6 — now open)

A standalone crate, isolated from the main pipeline so the bet remains
abandonable:

- Location: `experiments/node-proto/` (a new top-level dir; not a
  workspace default member — it must not affect `cargo build` of the
  compiler).
- A mini lexer + parser for the §2 `#node` subset only — does **not**
  touch `crates/lexer` or `crates/parser`.
- Causality analysis per §4.2 — dependency graph, `E0820`-style cycle
  rejection, topological schedule.
- LLVM-IR-text emitter producing the §4.3 shape.
- Tests: the three §4.1 programs parse, schedule, and emit; a known
  algebraic loop is rejected.

Stage 2's kill gate (§3): abandon if causality/scheduling is intractable
or the IR is not allocation-free / target-clean.

---

*Document version 0.1.0. Stage 1 complete (PASS) 2026-05-16. Stages 2–3
pending.*
