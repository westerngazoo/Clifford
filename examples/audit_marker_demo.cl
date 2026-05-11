// audit_marker_demo.cl — Decision #18 codegen marker (slice 21)
//
// Demonstrates that an `#audit #automaton`'s transition body
// emits `; audit-wrap site for <Owner> (<primitive>)` IR
// comments at every unsafe-primitive call site. The comments
// give a future debug-build instrumentation pass a stable
// place to inject `PointerAuditor` calls without touching the
// emitter again. Release builds elide the comments
// trivially (they're just IR comments — LLVM strips them
// during parsing).
//
// What this exercises:
//   - `#audit` automaton modifier                         (slice 20)
//   - `#audit` codegen markers at unsafe primitives       (slice 21)
//   - Marker categorisation per primitive                 (slice 21)
//   - No marker leaks into non-audit transitions          (slice 21)
//
// Compile:  cliffordc compile examples/audit_marker_demo.cl
// Output:   examples/audit_marker_demo.ll


// ─── Audit-marked: every unsafe op gets a wrap-site comment ─────────

#audit #automaton AuditedRing {
  // Transition that pokes a hand-rolled MMIO pointer. Each
  // primitive (`#unchecked_cast`, `#unchecked_offset`,
  // `#unchecked_store`, `#unchecked_load`) emits its own
  // categorised marker.
  #transition poke {
    let base: &u32 = #unchecked_cast<u64, &u32>("device base", 0x4000_4000u64);
    let next: &u32 = #unchecked_offset<u32>(base, 1i32);
    let cur: u32 = #unchecked_load<u32>(next);
    #unchecked_store<u32>(next, cur);
  }
}


// ─── Non-audit sibling: byte-identical slice-20 IR ──────────────────
//
// Same primitives, no `#audit` modifier — the IR for this
// transition contains zero `; audit-wrap site` comments.

#automaton PlainRing {
  #transition poke {
    let base: &u32 = #unchecked_cast<u64, &u32>("device base", 0x4000_5000u64);
    let next: &u32 = #unchecked_offset<u32>(base, 1i32);
    let cur: u32 = #unchecked_load<u32>(next);
    #unchecked_store<u32>(next, cur);
  }
}


// ─── Audit + staged composes (slices 18 + 21 are orthogonal) ────────
//
// The `#staged` shadow-write redirection (slice 18) and the
// `#audit` instrumentation marker (slice 21) live on the same
// automaton without interfering. Writes route through
// `@AuditedStaged.shadow`; primitives in the body still get
// the `; audit-wrap site` markers; a `#flush` (omitted here
// to keep the sample focused) would commit the shadow into
// live state.

#audit #staged #automaton AuditedStaged {
  v: u32;
  #transition init {
    let base: &u32 = #unchecked_cast<u64, &u32>("device base", 0x4000_6000u64);
    #unchecked_store<u32>(base, 0u32);
  }
}


// ─── Slice 22: markers extend to effects + interrupts ───────────────
//
// An `#effect` whose `#mutates: [...]` clause names an audited
// automaton picks up the same wrap-site markers as the
// audited automaton's own transitions. Same for `#interrupt`.
//
// `poke_audited_via_effect` writes to AuditedRing through an
// MMIO pointer and gets markers because `AuditedRing` is in
// the `#mutates` list. `poke_plain_via_effect` writes to
// PlainRing only — no markers, byte-identical to slice-21
// non-audit IR.

#effect poke_audited_via_effect() #mutates: [AuditedRing] {
  let base: &u32 = #unchecked_cast<u64, &u32>("aux", 0x4000_4040u64);
  #volatile_store<u32>(base, 0xCAFE_F00Du32);
  return;
}

#effect poke_plain_via_effect() #mutates: [PlainRing] {
  let base: &u32 = #unchecked_cast<u64, &u32>("aux", 0x4000_5040u64);
  #volatile_store<u32>(base, 0xDEAD_BEEFu32);
  return;
}

// An ISR that handles AuditedRing — markers fire on every
// unsafe primitive in the handler body.
#interrupt SysTick() #mutates: [AuditedRing] #priority: HIGH {
  let base: &u32 = #unchecked_cast<u64, &u32>("aux", 0x4000_4044u64);
  #volatile_store<u32>(base, 1u32);
  return;
}
