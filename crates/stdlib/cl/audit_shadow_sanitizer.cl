// crates/stdlib/cl/audit_shadow_sanitizer.cl
//
// `ShadowSanitizer` — default `PointerAuditor` implementation
// for the v0.2 `#audit` runtime-auditing chain (Decision #18).
//
// **Status (post-slice-42):** the **call-counting** default.
// Every call to `validate_load` / `validate_store` /
// `record_alloc` / `record_free` increments the matching
// per-kind counter; the validation methods always return
// `true` (so wrapped operations always proceed). The
// counters are observable from user code via
// `@snapshot ShadowSanitizer.<field>` for debugging,
// smoke-testing, and profiling:
//
//   "did my ISR's audit-wrapped store actually execute?"
//   → check `@snapshot ShadowSanitizer.stores` before/after
//
//   "how many loads happened during this region?"
//   → diff the counter at the boundaries
//
// **Why counting (vs full shadow-memory tracking)?**
//
// A KASAN-style ShadowSanitizer would maintain an actual
// shadow allocation table — a `[u8; SHADOW_SIZE]` array
// mapping byte addresses → valid/invalid — to reject
// dereferences of freed or unallocated pointers. That
// requires:
//   1. Allocator scaffolding (Clifford v0.2 has none yet).
//   2. Real ptr+size arguments at every wrap site (slice 41
//      passes placeholders pending the next plumbing
//      slice).
// Both are gated on infrastructure that lives later in the
// stdlib runway. Counting is the useful middle ground that
// works *today* with the existing slice-41 placeholder
// argument shape.
//
// **Concurrency caveat (v0.2):** the counter increments are
// load-modify-store with no synchronization. Multi-core
// firmware or interrupt-preemption can race the counters,
// producing under-counts. v0.2 is single-core focussed;
// firmware that needs accurate counts in the presence of
// interrupts should wrap the audited region in
// `#atomic: interrupt_critical;` (v0.2-δ/ε) — that's
// already the recommended pattern for any deferred-
// mutation work. The counters are best-effort
// observability, not a guaranteed-accurate audit log.


// ─── Sanitizer state ──────────────────────────────────────────────
//
// Four `u32` counters, one per `PointerAuditor` method.
// `@snapshot ShadowSanitizer.<field>` reads them from any
// `@fn` / `#effect` / `#interrupt`; the underlying field
// access is a single i32 load, which is single-instruction
// atomic on every supported target.

#automaton ShadowSanitizer {
  // Total `record_alloc` calls since boot. Incremented
  // each time the allocator (or user code) registers a
  // new pointer range as valid.
  allocs: u32;

  // Total `record_free` calls since boot.
  frees: u32;

  // Total `validate_load` calls since boot — equivalently,
  // the number of audited `#unchecked_load` / `#volatile_load`
  // sites that executed.
  loads: u32;

  // Total `validate_store` calls since boot — equivalently,
  // the number of audited `#unchecked_store` / `#volatile_store`
  // sites that executed.
  stores: u32;
}


// ─── #impl PointerAuditor for ShadowSanitizer ─────────────────────
//
// Each method increments its matching counter, then either
// returns (alloc/free) or returns `true` (validate_*). The
// `true` return preserves the v0.2 permissive-default
// semantics: every wrapped operation proceeds — counting
// is observation only, not enforcement.

#impl PointerAuditor for ShadowSanitizer {
  effect record_alloc(ptr: access<u8>, size: u32) {
    ShadowSanitizer.allocs += 1u32;
    return;
  }

  effect record_free(ptr: access<u8>) {
    ShadowSanitizer.frees += 1u32;
    return;
  }

  effect validate_load(ptr: access<u8>, size: u32) -> bool {
    ShadowSanitizer.loads += 1u32;
    return true;
  }

  effect validate_store(ptr: access<u8>, size: u32) -> bool {
    ShadowSanitizer.stores += 1u32;
    return true;
  }
}
