# Conformance Tests

Per `docs/CLIFFORD_SPEC.md` §10. Tests are organised by phase. Each test is a
`.cl` source file plus an expected outcome file.

```
tests/
├── lex/        — token stream tests, one file per token category
├── parse/      — AST shape tests, JSON-encoded expected ASTs
├── typecheck/
│   ├── pass/   — files that should type-check
│   └── fail/   — files that should fail with specific error codes
├── effect/     — FSM extraction tests, expected state graphs as DOT
├── ortho/
│   ├── pass/   — orthogonal automata, should compile
│   └── fail/   — non-orthogonal, expected error messages with E0520
├── borrow/     — body-scoped reference / provenance tests (Decision #13)
├── sigma/      — sigma-loop bounds-tracking tests (Decision #14)
├── interface/  — #interface + #impl + monomorphization tests (Decision #16)
├── unsafe/     — narrow unsafe primitive tests (Decision #17)
├── access/     — nominal access type tests (Decision #19)
├── bitfield/   — first-class bitfield tests (Decision #20)
├── codegen/    — IR snapshot tests (LLVM IR golden files via insta)
└── runtime/    — actual execution on QEMU for embedded targets
```

## Critical test cases

See `docs/CLIFFORD_SPEC.md` §10 "Critical test cases" for the full list. Each
phase ships only when its critical tests pass.

## Naming conventions

- Pass tests: `<short_description>.cl`
- Fail tests: `<error_code>_<description>.cl`
  (e.g., `E0710_uart_to_spi_pointer.cl`)
- Each test file has a sibling `.expected` file describing the expected
  outcome (token stream, AST JSON, error code, IR snapshot, etc.).

## Running

```sh
cargo test --workspace                 # all unit + integration tests
cargo test -p clifford-ortho           # one crate
cargo test -- --nocapture              # see println output
```

`cliffordc test` (Phase 5) runs `#test` blocks discovered in user `.cl` source.
