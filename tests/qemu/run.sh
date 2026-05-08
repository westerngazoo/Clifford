#!/usr/bin/env bash
# run.sh — drive the full QEMU firmware smoke test.
#
# Pipeline:
#   1. cliffordc compile firmware_smoke.cl   -> firmware_smoke.ll
#   2. clang --target=thumbv7m-none-eabi -c   -> firmware_smoke.o
#   3. clang --target=thumbv7m-none-eabi      -> startup.o, harness.o
#   4. clang --target=thumbv7m-none-eabi -T link.ld   -> app.elf
#   5. qemu-system-arm -M lm3s6965evb         -> run, capture exit code
#
# Required tooling (all available on Ubuntu via apt):
#   - cargo (for cliffordc)
#   - clang with the LLVM ARM backend
#   - qemu-system-arm
#
# Exit:
#   0 if the harness exits with code 0 (all functional checks passed)
#   non-zero if any toolchain step fails or the harness reports a check failure

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "${SCRIPT_DIR}/../.." && pwd)"
BUILD_DIR="${SCRIPT_DIR}/build"

mkdir -p "${BUILD_DIR}"
cd "${BUILD_DIR}"

# ─── 1. Compile the Clifford source ───────────────────────────────────
echo "==> cliffordc compile firmware_smoke.cl"
(cd "${REPO_ROOT}" && cargo run --quiet -p clifford-cli -- \
    compile "${SCRIPT_DIR}/firmware_smoke.cl" \
    -o "${BUILD_DIR}/firmware_smoke.ll" \
    --module-name firmware_smoke)

# ─── 2-3. Cross-compile to Cortex-M3 ──────────────────────────────────
TARGET=thumbv7m-none-eabi
CFLAGS=(
    --target="${TARGET}"
    -mcpu=cortex-m3
    -mthumb
    -ffreestanding
    -nostdlib
    -O0
    -g
)

echo "==> clang ${TARGET} on .ll, .c sources"
clang "${CFLAGS[@]}" -c "${BUILD_DIR}/firmware_smoke.ll" -o "${BUILD_DIR}/firmware_smoke.o"
clang "${CFLAGS[@]}" -c "${SCRIPT_DIR}/startup.c"        -o "${BUILD_DIR}/startup.o"
clang "${CFLAGS[@]}" -c "${SCRIPT_DIR}/harness.c"        -o "${BUILD_DIR}/harness.o"

# ─── 4. Link with the Cortex-M layout ─────────────────────────────────
echo "==> link app.elf"
clang "${CFLAGS[@]}" \
    -T "${SCRIPT_DIR}/link.ld" \
    -Wl,--gc-sections \
    "${BUILD_DIR}/firmware_smoke.o" \
    "${BUILD_DIR}/startup.o" \
    "${BUILD_DIR}/harness.o" \
    -o "${BUILD_DIR}/app.elf"

# ─── 5. Run on QEMU ───────────────────────────────────────────────────
echo "==> qemu-system-arm -M lm3s6965evb"
set +e
qemu-system-arm \
    -M lm3s6965evb \
    -nographic \
    -no-reboot \
    -semihosting-config enable=on,target=native,arg=app \
    -kernel "${BUILD_DIR}/app.elf"
QEMU_RC=$?
set -e

if [[ ${QEMU_RC} -eq 0 ]]; then
    echo "==> PASS (all 8 functional checks succeeded)"
    exit 0
else
    echo "==> FAIL (harness reported check #${QEMU_RC} failed; see harness.c)"
    exit ${QEMU_RC}
fi
