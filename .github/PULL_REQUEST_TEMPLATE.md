<!--
Per CLAUDE.md §5.3, every PR description states which spec section it
implements and which test demonstrates it. Fill in below.
-->

## What this implements

§<section> of `docs/CLIFFORD_SPEC.md`. (Or: §X of `docs/DECISIONS.md` Decision #N.)

## How to verify

- `cargo test -p <crate>`
- New tests: `tests/<phase>/<file>.cl`

## Spec changes

None. (Or: updated §X to clarify Y. Note: per CLAUDE.md §5.5, spec changes are
their own PR; do not bundle with code that depends on them.)

## Open questions

(List, or "None.")

## Reviewer checklist

<!-- The reviewer fills this in. CLAUDE.md §8.4 lists the criteria. -->

- [ ] Spec section is correctly cited and accurately implemented.
- [ ] Tests cover the happy path and at least one failure mode.
- [ ] No new `unsafe`, no new dependencies, no new `unwrap()` without justification.
- [ ] Error messages are clear to a non-implementer.
- [ ] Documentation is present and accurate.
- [ ] No drift from the project's existing style.
- [ ] (For `ortho` crate only) coverage remains at 100%; property test added.
