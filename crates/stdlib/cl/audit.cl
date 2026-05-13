// crates/stdlib/cl/audit.cl
//
// `clifford::audit` — the PointerAuditor interface (Decision #18,
// slice 37) and supporting types for the v0.2 runtime-auditing
// chain.
//
// **Status:** interface declaration is canonical for v0.2.
// Default `ShadowSanitizer` implementation + the codegen pass
// that turns `; audit-wrap site for <Owner>` IR markers into
// real calls land in slices 38+.
//
// Decision #18 specifies the surface: a user opts into
// runtime auditing by adding `#audit` to an `#automaton`
// declaration (slice 20). The compiler emits markers
// (slices 21–23, 26) at every unsafe-primitive emission
// site inside an `#audit`-marked automaton. In debug
// builds, a planned codegen pass rewrites those markers
// into calls through this interface, dispatching against
// a Sanitizer implementation that maintains shadow state
// for allocation tracking and pointer validation.
//
// The interface is intentionally minimal — four methods,
// no allocation, no formatting. A Sanitizer impl is free
// to be more elaborate (sourcemap reporting, stack-trace
// capture, etc.) but must satisfy the four-method contract.
//
// **Why `access<u8>` instead of `*mut u8`?**
//
// Clifford uses `access<T>` as its narrow-unsafe pointer
// type per Decision #19. `access<u8>` is the byte-pointer
// shape — it carries the same address but with Clifford's
// audit-trail discipline (every cast into it requires
// `#unchecked_cast<S, access<u8>>("<reason>", v)`).


// ─── PointerAuditor interface ───────────────────────────────────────
//
// A Sanitizer implements all four methods. The compiler-
// inserted wrappers around `#unchecked_*` / `#volatile_*` /
// `#unchecked_cast` dispatch through a Sanitizer instance
// (provided by the firmware's startup code or by the
// default `ShadowSanitizer`).
//
// `record_alloc` / `record_free` are called by the
// allocator (or by user code at object lifecycle
// boundaries) to update the shadow allocation table.
// `validate_load` / `validate_store` are called BEFORE
// every unsafe load / store of `size` bytes at `ptr`;
// returning `false` aborts the operation (the wrapper
// surfaces a runtime error with the source location).

#interface PointerAuditor {
  // Allocator hook: record that `size` bytes starting at
  // `ptr` are now valid for `validate_load` /
  // `validate_store`. Called from `#> allocate` paths
  // (or any user code that constructs valid pointers).
  effect record_alloc(ptr: access<u8>, size: u32);

  // Allocator hook: invalidate the region starting at
  // `ptr`. After this call, `validate_load` /
  // `validate_store` for any sub-range must return
  // false. Called from `#> free` paths.
  effect record_free(ptr: access<u8>);

  // Validation hook: called before every audited load.
  // Returns `true` if `[ptr, ptr+size)` is currently
  // marked valid (i.e. inside an alloc with no
  // intervening free). The wrapper-emitting pass
  // (slice 38+) inserts a call to this method
  // immediately before each `#unchecked_load` /
  // `#volatile_load` / `#unchecked_offset` site
  // inside an `#audit` automaton.
  effect validate_load(ptr: access<u8>, size: u32) -> bool;

  // Symmetric to `validate_load` for the write side.
  // Same semantics; called before `#unchecked_store` /
  // `#volatile_store` / `#unchecked_cast`-followed-by-
  // dereference patterns.
  effect validate_store(ptr: access<u8>, size: u32) -> bool;
}
