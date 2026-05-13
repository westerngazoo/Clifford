// crates/stdlib/cl/audit_shadow_sanitizer.cl
//
// `ShadowSanitizer` — placeholder default `PointerAuditor`
// implementation for the v0.2 `#audit` runtime-auditing chain
// (Decision #18, slice 38).
//
// **Status:** scaffolding only. The interface registration
// parses through to resolve but the methods are not yet
// implemented because the parser's `#impl` body grammar
// currently accepts an empty `{ }` only (no method bodies).
// Method-body support is slice 39's task; once that lands,
// this Sanitizer becomes a "permissive default" that
// returns `true` from every `validate_*` call (no shadow
// state, no real allocation tracking).
//
// **Design intent (post-slice-39):** the default
// `ShadowSanitizer` is the no-op variant. Firmware that
// wants real allocation tracking provides its own
// `#impl PointerAuditor for MyTrackingSanitizer` (referencing
// an automaton with allocation-table state). The wrap-
// emitting codegen pass (slice 40) dispatches against
// whichever Sanitizer the firmware's startup code wires up.
// Until then, builds compile cleanly and the markers in
// the IR are the only artefact.
//
// The empty-body limitation here is non-blocking for the
// rest of the chain: slice 40 can rewrite markers into
// calls against the *interface* (`PointerAuditor`), not
// against a specific impl. Wiring the default Sanitizer
// is a separate startup-code concern that lands when the
// stdlib has bootstrap support.


// ─── Placeholder Sanitizer automaton ──────────────────────────────
//
// A real ShadowSanitizer would have allocation-table state
// — `shadow_marks: [u8; N]` or similar — and a base
// address against which to compute shadow indices. For the
// slice-38 scaffold, we declare it with no fields so it
// parses and resolves cleanly while documenting the
// design intent.

#automaton ShadowSanitizer { }


// ─── Empty #impl registration ──────────────────────────────────────
//
// Decision #16's `#impl Interface for Automaton { … }`
// registers `ShadowSanitizer` as a `PointerAuditor`. The
// resolver consumes this registration; codegen consults it
// (in slice 40) to know which Sanitizer's effects to call
// when wrapping unsafe primitives in `#audit` automatons.
//
// The body is empty for v0.2-α (parser limitation). Once
// slice 39 lands `#impl` method bodies, this becomes:
//
//   #impl PointerAuditor for ShadowSanitizer {
//     effect record_alloc(ptr: access<u8>, size: u32) { return; }
//     effect record_free(ptr: access<u8>) { return; }
//     effect validate_load(ptr: access<u8>, size: u32) -> bool {
//       return true;
//     }
//     effect validate_store(ptr: access<u8>, size: u32) -> bool {
//       return true;
//     }
//   }

#impl PointerAuditor for ShadowSanitizer { }
