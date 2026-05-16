# `docs/research/` — research bets, not v1.0 commitments

This directory holds design work that is **deliberately not on the path to
v1.0**. Content here is preserved verbatim for the historical record and as
a starting point if the project ever revisits these ideas — but it is *not*
normative, *not* a locked commitment, and *not* scaffolded in the live
compiler.

## Why this directory exists

The 2026-05 decision audit (`docs/decision-audit-2026-05.md`), itself a
consequence of the post-GA-narrative pivot (`docs/foundations.md`), graded
three locked decisions **DEFER-TO-RESEARCH**. They addressed real problems
but did so via Geometric Algebra machinery that was unimplemented,
speculative, and — per the audit and `foundations.md` — better served by
established literature (Stack Resource Policy, Pony reference capabilities,
optimistic-concurrency-control). Rather than delete the work, it was
relocated here.

A decision in this directory:

- has its `DECISIONS.md` entry replaced by a short deferral stub that
  points here;
- has had its compiler scaffolding (lexer token reservations, AST enum
  placeholders) **removed from the live tree** — if the idea is ever
  revived, the scaffolding is re-added then, against the design as it
  stands then;
- is not referenced by the normative spec as a forthcoming feature.

## Contents

| File | Origin | What it was |
|---|---|---|
| `ga-shared-automata.md` | Decision #21 | Shared mutable state via a mixed-metric Cl(p,0,n) algebra; locks as multivectors. |
| `ga-rotor-locks.md` | Decision #26 | Rotor-as-acquisition-primitive for plane-confined locks (refines #21). |
| `ga-across-scales.md` | Decision #27 | The same wedge primitive lifted to distributed runtime race detection. |

## The real problems, and where they are actually addressed

The deferral does **not** abandon the problems these decisions named — it
abandons the GA mechanism. The replacement direction, per
`docs/foundations.md`:

- **Shared mutable resources** (run-queues, allocators, capability tables):
  Stack Resource Policy (Baker 1991, as mechanized by RTIC) for the
  priority + shared-resource story, plus a minimal `#owned` / `#sendable`
  field qualifier inspired by Pony's `iso` reference capability for
  send-once shared mutable state. A fresh decision will cover this once
  the comparison artifact validates that the embedded patterns Clifford
  targets actually need it.
- **Distributed race detection**: if ever built, grounded in the
  optimistic-concurrency-control literature (Sinfonia, Calvin) under its
  own decision — not as a "GA across scales" claim.

The original ADRs (`docs/adr/0002`, `0005`, `0006`) are immutable per
CLAUDE.md §5.4 and remain in place as the historical record of the
locked-then-deferred designs.
