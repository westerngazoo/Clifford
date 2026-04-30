---
name: Bug
about: Reproducible failure of the compiler or a related tool.
title: "[bug] "
labels: bug
---

<!--
Per CLAUDE.md §9.1, bugs include the input, expected output, and actual output.
-->

## Input

```clifford
// Minimal Clifford source that triggers the issue.
```

## Expected behavior

(What should `cliffordc` have done?)

## Actual behavior

(What did it do? Paste error messages, IR output, or runtime symptoms.)

## Spec citation

Which section of `docs/CLIFFORD_SPEC.md` (or `docs/DECISIONS.md`) describes the
expected behavior?

## Environment

- `cliffordc` version / commit:
- `rustc --version`:
- Host OS:
- Target triple:
