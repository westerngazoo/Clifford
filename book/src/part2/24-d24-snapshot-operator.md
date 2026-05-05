# Chapter 24: Decision #24 — `@snapshot` boundary operator

> **Status:** DESIGN-IN-PROGRESS. ADR forthcoming:
> `docs/adr/0004-snapshot-boundary-operator.md`. This chapter is a
> placeholder; full content lands once the ADR concludes.

## 24.1 The one-line direction

Introduce `@snapshot Auto.field` as the **only** way to read mutable
automaton state into pure-side analysis. The boundary crossing
becomes syntactically visible: every place pure-side code observes
mutable state is grep-able.

## 24.2 Open questions for the ADR

- **Expression vs statement?** Is `@snapshot` an expression (`let v
  := @snapshot Counter.value;`) or a statement (`@snapshot v from
  Counter.value;`)? Expression is more composable; statement makes
  the timing more explicit.
- **Copy-by-value vs ref-to-snapshot?** A trivially-copyable field
  is straightforward; a 4 KiB cache is not. Do we permit only `Copy`
  fields, or do we introduce a separate `@snapshot_ref` for borrows?
- **Interaction with `#shared` (Decision #21).** A snapshot of a
  `#shared` field needs to acquire (and release) the relevant lock
  — does `@snapshot` do this implicitly, or does it require an
  explicit `@snapshot Auto.field with_lock(L)`?
- **Backward compatibility with existing snapshot-by-convention.**
  Book Ch. 39's SPSC pattern reads `Uart.head` and `Uart.tail`
  inside `@fn` analysis; how do those existing patterns migrate?

## 24.3 What lands when

- v0.2 once the ADR closes: `@snapshot` as the canonical
  boundary-crossing operator; existing implicit reads remain
  permitted with a deprecation warning.
- v0.4+: implicit reads removed; `@snapshot` becomes mandatory.

Full text lands once ADR 0004 closes. See `DECISIONS.md` Decision #24
for the locked direction.
