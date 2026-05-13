// crates/stdlib/cl/audit_shadow_sanitizer.cl
//
// `ShadowSanitizer` — default `PointerAuditor` implementation
// for the v0.2 `#audit` runtime-auditing chain (Decision #18).
//
// **Status (post-slice-39):** the permissive default — all
// four methods are no-ops, `validate_*` always returns
// `true`. Firmware that wants real allocation tracking
// provides its own `#impl PointerAuditor for
// MyTrackingSanitizer` (referencing an automaton with
// allocation-table state). The wrap-emitting codegen pass
// (slice 41) dispatches against whichever Sanitizer the
// firmware's startup code wires up.
//
// The permissive default exists so v0.2 firmware that adds
// `#audit` to an automaton compiles + links + boots cleanly
// even without a configured Sanitizer — the markers fire,
// the calls resolve to no-ops, no validation actually
// happens but no spurious crashes either. Once the
// firmware author wires up a real Sanitizer (via Decision
// #16's `#impl` mechanism), validation kicks in.


// ─── Placeholder Sanitizer automaton ──────────────────────────────
//
// A real ShadowSanitizer would have allocation-table state
// — `shadow_marks: [u8; N]` or similar — and a base
// address against which to compute shadow indices. For
// v0.2 we ship the field-less variant; real shadow state
// requires allocator / paging infrastructure that lives
// later in the stdlib runway.

#automaton ShadowSanitizer { }


// ─── #impl PointerAuditor for ShadowSanitizer ─────────────────────
//
// Decision #16's `#impl Interface for Automaton { … }`
// registers `ShadowSanitizer` as a `PointerAuditor`. The
// resolver consumes this registration; the wrap-emitting
// codegen pass (slice 41) dispatches against the
// `PointerAuditor` interface — which monomorphizes to
// these implementations when this Sanitizer is the
// configured default.
//
// All four methods are no-ops for the v0.2 permissive
// default. `validate_load` / `validate_store` return
// `true` so wrapped operations always proceed.

#impl PointerAuditor for ShadowSanitizer {
  effect record_alloc(ptr: access<u8>, size: u32) {
    // No shadow state to update; the permissive default
    // doesn't track allocations. A real Sanitizer would
    // mark the shadow range `[ptr, ptr+size)` as valid.
    return;
  }

  effect record_free(ptr: access<u8>) {
    // No shadow state to invalidate. A real Sanitizer
    // would mark the recorded allocation's shadow range
    // as invalid.
    return;
  }

  effect validate_load(ptr: access<u8>, size: u32) -> bool {
    // Permissive default: every load is allowed. A real
    // Sanitizer would check `[ptr, ptr+size)` against
    // its shadow allocation table.
    return true;
  }

  effect validate_store(ptr: access<u8>, size: u32) -> bool {
    // Permissive default: every store is allowed. Same
    // shape as `validate_load`.
    return true;
  }
}
