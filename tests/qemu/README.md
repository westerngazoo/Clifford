# QEMU firmware smoke test

End-to-end proof that `cliffordc`-generated LLVM IR compiles to a
working Cortex-M3 binary and runs correctly under QEMU. This is
the v0.1 release-blocker test: every codegen slice contributes a
function to `firmware_smoke.cl`, and the C harness verifies each
returns the expected value. A regression anywhere in the pipeline
fails this test.

## What it tests

| Function | Slice exercised | Verified by |
|---|---|---|
| `answer()` | slice 1 — basic integer fn | returns 42 |
| `clamp(x)` | slice 13 — `if` + comparison | clamp(50)=50, clamp(200)=100 |
| `sum_to(n)` | slice 11 + 12 — sigma loop + `let mut` accumulator | sum_to(10)=45 |
| `bit_count(x)` | slice 13 — `if` + sigma + bitwise + shift | bit_count(0xFF)=8 |

Eight functional checks total. The harness exits with code 0 on
success or with `1..=8` indicating which check failed; QEMU's
`-semihosting-config arg=app` propagates that back as the process
exit code.

## CI

Runs on every PR via `.github/workflows/qemu-firmware.yml` on
Ubuntu. The workflow installs `clang`, `llvm`, and
`qemu-system-arm` via apt, then runs `tests/qemu/run.sh`.

## Manual run (Linux / macOS / WSL)

You'll need:

- Rust toolchain (already pinned in `rust-toolchain.toml`)
- `clang` with the LLVM ARM backend (`apt install clang llvm` /
  `brew install llvm`)
- `qemu-system-arm` with `lm3s6965evb` board support
  (`apt install qemu-system-arm` / `brew install qemu`)

Then:

```bash
bash tests/qemu/run.sh
```

The script writes intermediate artefacts (`*.ll`, `*.o`, `app.elf`)
to `tests/qemu/build/` (gitignored).

## Files

- `firmware_smoke.cl` — the Clifford program with four functions
  exercising slices 1–13.
- `harness.c` — calls each function and checks its return value;
  exits via semihosting.
- `startup.c` — minimal Cortex-M3 boot: vector table, reset
  handler that initializes `.bss` / `.data` and calls `main()`,
  semihosting `SYS_EXIT_EXTENDED` primitive.
- `link.ld` — Cortex-M3 linker script matching the LM3S6965
  memory map (256 KB flash at 0x0, 64 KB SRAM at 0x20000000).
- `run.sh` — end-to-end driver script.

## Why bare-metal Cortex-M3 (not Linux user-mode ARM)

The v0.1 target IS firmware. A user-mode `qemu-arm` test would
prove the codegen produces valid ARM machine code, but it
wouldn't exercise:

- The vector table layout (Decision #10 `#interrupt`s land here).
- The fixed memory map (Decision #6 register-block `#address`).
- The lack of a runtime — Clifford has no `libc`, no allocator,
  no `_start` shim. Every line of the boot path is in
  `startup.c`.
- The semihosting exit primitive — the only way bare-metal code
  can talk back to the host without UART hardware.

The `lm3s6965evb` board is the embedded community's standard
QEMU smoke target precisely because it's minimal and supports
semihosting out of the box.

## Adding a new slice's smoke check

1. Add a function to `firmware_smoke.cl` that exercises the
   slice's surface.
2. Add a corresponding `extern` declaration and `if (...) { semihost_exit(N); }`
   check to `harness.c`.
3. Run `bash tests/qemu/run.sh` locally (or push and let CI
   verify).

That's it. No CMake, no Cargo target glue, no embedded HAL
dependency. The harness stays small on purpose so a regression
points at exactly one slice.
