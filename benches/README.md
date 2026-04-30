# Benchmarks

Per `docs/CLIFFORD_SPEC.md` §11 and CLAUDE.md §7: criterion.rs benchmarks for
each phase, with regression detection at ±5%.

## Required baseline benches (CLAUDE.md §7)

- `bench_lex_large_file` — lexer throughput on a synthetic 10kLoC `.cl` file.
- `bench_typecheck_generic_heavy` — type checker on a generics-rich workload.
- `bench_ortho_50_automata` — GA orthogonality engine on 50 concurrent
  automata. The wedge-product check should be O(1) per pair; this bench
  ensures the field-basis assignment doesn't degrade.

## Headline target

Compile a 10kLoC Clifford program in **under 5 seconds** in release mode on a
2024-era laptop. Phase 5 ships when this holds for the Appendix A examples
plus the v0.1 reference firmware and reference non-firmware application.

## CI integration

CI runs benchmarks on every PR and posts a regression comment if any bench
regresses by more than 5%. Performance regressions ≥ 20% block merge unless
justified in the PR description.
