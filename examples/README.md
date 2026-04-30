# Clifford Examples

Worked example programs in `.cl` source. Each demonstrates a slice of the
language and is referenced by name from `docs/CLIFFORD_SPEC.md` §10
conformance tests.

## v0.1 examples (target list per CLAUDE.md §10)

### Firmware (Cortex-M / RISC-V proving ground)

- `blinky/` — toggle PA5 every 500 ms; log "blink N\n" over USART1. Smallest
  meaningful firmware. Demonstrates: register-block automata (Decision #6),
  named transitions (Refinement #5b), monoid automata (Decision #5 Rule 4),
  sigma loops over slices (Decision #14 + Refinement #14a), pure helpers,
  `:=` short binding (Decision #8).
- `uart_echo/` — line-buffered UART echo with FSM and ISR. Demonstrates:
  multi-state automaton, ISR concurrency, named transitions, state inspection
  (`Auto@state`, `Auto::State` per Refinement #5d), `cmd_is_help` pattern for
  pure helpers operating on borrowed slices.
- `temp_monitor/` — read ADC every 100 ms, send Celsius reading over UART.
  Demonstrates: multiple peripherals, periodic timing, ADC + UART register
  blocks, formatting helpers.

### Non-firmware (demonstrates general-purpose use per CLAUDE.md §10)

- `cli_word_count/` — reads stdin, counts words. Demonstrates: stdlib I/O,
  sigma loops over input, the language without any `#interrupt`s.
- `kernel_scheduler/` — pure scheduler decision over a snapshot, imperative
  apply. Demonstrates: the FCIS pattern under compiler enforcement
  (`@fn pick_next` is `$ [Pure]` and cannot reach into `#`-context).

## Format

Each example is a directory with:

- One or more `.cl` source files.
- A `README.md` explaining what it demonstrates and which spec sections it
  exercises.
- A `Cargo.toml` (when buildable as a standalone binary) or a target config
  file (when compiling for a Cortex-M target).
- An `expected/` subdirectory with golden outputs (IR snapshot, runtime
  output, etc.) per the `insta` snapshot convention.

## Phase 5 will populate these

v0.1 scaffolding includes only this README. The example sources are written
during Phase 5 alongside stdlib bootstrap.
