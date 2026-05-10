//! # clifford-codegen
//!
//! LLVM IR code generation for the Clifford compiler. Implements §8 of
//! `docs/CLIFFORD_SPEC.md`.
//!
//! ## Targets
//!
//! Per §8.1: `thumbv6m-none-eabi`, `thumbv7em-none-eabihf`,
//! `riscv32imac-unknown-none-elf`, `riscv64gc-unknown-none-elf`,
//! `x86_64-unknown-linux-gnu` (for testing).
//!
//! ## Slice 1 (this PR) — text-form LLVM IR for the v0.1 minimum surface
//!
//! The decision recorded in `Cargo.toml` is to emit **text-form `.ll`** in
//! v0.1, deferring the inkwell / llvm-sys binding choice until a later
//! slice needs native LLVM linkage. The IR is the standard textual form
//! a user can pipe to `llc` / `clang` for object generation.
//!
//! What lowers in this slice:
//!
//! - **Primitive types** (§4.1): `u8` … `u64`, `usize`, `i8` … `i64`,
//!   `isize`, `f32`, `f64`, `bool`. Each maps to its LLVM equivalent
//!   (`i8`, `i16`, `i32`, `i64`, `float`, `double`, `i1`).
//!   Pointer-sized integers (`usize` / `isize`) lower to `i64` for the
//!   v0.1 default 64-bit target; future target-aware slice will adapt.
//! - **`@fn` declarations** with primitive params + return + bodies that
//!   contain a single `return <expr>;` or a sequence of `let` bindings
//!   followed by `return <expr>;`.
//! - **Integer / boolean literals**, **path expressions** (single-segment
//!   names resolving to local bindings or function parameters), **basic
//!   binary arithmetic** (`+ - * / %` over integers), **direct function
//!   calls** (single-segment Path callee).
//!
//! What v0.1 codegen-slice-1 deliberately defers (subsequent slices):
//!
//! - **§8.4 Automaton/transition/effect lowering** — the big chunk:
//!   state struct per non-register-block automaton; state-tag field for
//!   multi-state automata; one LLVM function per effect, transition,
//!   hardware mutator, and per `(generic_effect, interface_arg)`
//!   specialisation (Decision #16); transition-atomicity wrapping per
//!   Refinement #5e (cli/sti or LDREX/STREX based on R(A) and target);
//!   register-block field reads/writes as volatile loads/stores at
//!   `address + offset` (Decision #6); bit-field RMW with target-atomic
//!   when concurrent writer exists (Decision #20).
//! - **§8.5 Interrupt handler emission** — `#interrupt NAME` produces an
//!   LLVM function with linker symbol `NAME`, target-specific calling
//!   convention, `.interrupts` section (Decision #10).
//! - **§8.3 Composite types** — references (`T*` with `noalias` for
//!   `&mut`), arrays (LLVM `[N x T]`), slices (`{T*, i64}`), tuples
//!   (LLVM struct).
//! - **ADT lowering** — tagged-union representation (variant tag + max-
//!   sized payload).
//! - **Sigma loops** — counted loop with bounds-check elision (§5.8).
//! - **Decision #22 codegen consumers** — `Acquire` / `Release` / `SeqCst`
//!   memory-ordering fences (consumed by the v0.4-α slice when the
//!   imperative-callable lowering lands).
//! - **Optimisation passes** — none in v0.1; LLVM's own passes do the
//!   heavy lifting downstream.
//!
//! ## `unsafe`
//!
//! This crate is one of the two allowed `unsafe` sites (per CLAUDE.md
//! §3.1, the other being `clifford-stdlib`). Slice 1 does not need
//! `unsafe` — text emission is pure-safe Rust. When the native LLVM
//! binding lands, every `unsafe` block must justify itself with a
//! `// SAFETY:` comment.

#![warn(missing_docs)]
// `unsafe` is allowed in this crate per CLAUDE.md §3.1; do NOT add
// `#![forbid(unsafe_code)]` here. Specific unsafe blocks must each
// justify themselves with a `// SAFETY:` comment.

use std::collections::HashMap;
use std::fmt::Write;

use clifford_ast::{
    AssignOp, AutomatonDecl, BinaryOp, Block, EffectDecl, Expr, ExprKind, FieldAssign, FnDecl,
    InterruptDecl, Item, Param, PrimitiveType, Program, Stmt, StmtKind, TransitionDecl, TypeExpr,
    TypeKind, UnaryOp,
};
use clifford_resolve::Resolution;
use clifford_types::{Type, Typing};
use thiserror::Error;

/// Errors produced during code generation.
///
/// Reserves the `E08xx` range alongside the `E08xx` block in §10
/// conformance tests. Codegen errors are typically internal — user
/// errors are caught by upstream phases (lexer, parser, resolver,
/// types, check, effect, ortho); a codegen error usually indicates a
/// missing lowering for an AST shape this slice doesn't yet handle.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum CodegenError {
    /// An AST shape isn't yet handled by this codegen slice.
    ///
    /// Carries a short human-readable description of *what* couldn't
    /// be lowered (e.g. `"automaton declaration"`, `"sigma loop"`,
    /// `"reference type"`) so error messages point at the missing
    /// feature rather than failing silently.
    #[error("E0810: codegen not yet implemented for {what}")]
    NotYetImplemented {
        /// Short description of the unhandled construct.
        what: &'static str,
    },

    /// A name appeared in expression position that didn't resolve to
    /// a local binding, parameter, or top-level callable. This should
    /// have been caught upstream by `clifford-resolve`; if codegen
    /// sees it, the program is malformed.
    #[error("E0811: codegen could not resolve name `{name}` (likely an upstream resolver bug)")]
    UnresolvedName {
        /// The unresolved name.
        name: String,
    },

    /// An integer literal carried a type suffix codegen couldn't parse
    /// (e.g. unrecognised suffix or out-of-range value). This should
    /// have been caught by upstream typing; here it's an internal
    /// safety net.
    #[error("E0812: codegen could not interpret literal `{literal}`: {reason}")]
    BadLiteral {
        /// The literal as written in source.
        literal: String,
        /// Why it couldn't be lowered.
        reason: &'static str,
    },
}

/// Lower a fully-typechecked [`Program`] to text-form LLVM IR.
///
/// The returned string is the contents of a `.ll` file the user can
/// pipe to `llc` / `clang` for object code. Slice 1+ emits a module
/// containing one LLVM function per `@fn` declaration; other top-level
/// items (`#automaton`, `#effect`, `#interrupt`, `@type`, `@trait`,
/// `#interface`, `#impl`, `#test`, `@sequential`) are silently skipped
/// — they lower in later slices.
///
/// `resolution` and `typing` come from the upstream `clifford-resolve`
/// and `clifford-types` phases; the emitter consults them for
/// authoritative type info on every expression (path lookups, call
/// return types, signed-vs-unsigned op selection). Slice 1 ran with a
/// syntactic-guess fallback; slice 2 replaces that with `Typing`-driven
/// lookup.
///
/// `module_name` ends up in the `; ModuleID = '<name>'` header and the
/// `source_filename` line; pick something deterministic per source file
/// (e.g. the source path's stem) so reproducible builds work.
///
/// # Errors
///
/// Returns `Err(Vec<CodegenError>)` when one or more `@fn` bodies
/// contain expression / statement shapes this slice can't lower. The
/// vector accumulates errors across the whole program in source order
/// so a single pass surfaces every unhandled construct.
///
/// # Examples
///
/// ```
/// use clifford_codegen::lower;
/// use clifford_ast::Program;
/// use clifford_resolve::Resolution;
/// use clifford_types::Typing;
/// let p = Program::default();
/// let r = Resolution::default();
/// let t = Typing::default();
/// let ir = lower(&p, &r, &t, "empty").expect("empty program lowers cleanly");
/// assert!(ir.contains("ModuleID = 'empty'"));
/// ```
pub fn lower(
    program: &Program,
    resolution: &Resolution,
    typing: &Typing,
    module_name: &str,
) -> Result<String, Vec<CodegenError>> {
    let mut emitter = Emitter::new(module_name, resolution, typing);
    emitter.emit_module_header();

    // Pass 1: build the automaton registry so effects and transitions
    // can resolve field offsets without re-walking the AST.
    emitter.collect_automatons(program);

    // Pass 2: emit one `%struct.<Name>` type definition + global
    // state instance per non-register-block automaton (slice 3 scope).
    // Register-block automatons (`#address: 0x…`) defer to slice 4 —
    // their lowering uses volatile loads/stores at fixed addresses
    // rather than a global state variable.
    emitter.emit_automaton_state_structs(program);

    // Slice 18 (Decision #12): if any automaton is `#staged`, the
    // `#flush` lowering will need `@llvm.memcpy.p0.p0.i64`. Emit
    // the declaration once at module scope so every flush site is
    // a plain `call` with no per-site declaration.
    emitter.emit_staged_intrinsics_if_needed(program);

    // Pass 3: emit one LLVM function per @fn / #effect / #interrupt /
    // #transition. Order is preserved from source so callers see
    // callees declared first when source orders them naturally; LLVM
    // doesn't actually require forward declarations for module-level
    // functions but the predictability helps tooling.
    for item in &program.items {
        match item {
            Item::Fn(decl) => emitter.emit_fn(decl),
            Item::Effect(decl) => emitter.emit_effect(decl),
            Item::Interrupt(decl) => emitter.emit_interrupt(decl),
            Item::Automaton(decl) => emitter.emit_automaton_transitions(decl),
            // Other items (`@type`, `@trait`, `#interface`, `#impl`,
            // `#test`, `@sequential`) defer to subsequent slices.
            // Skipping (vs erroring) means partial programs still
            // produce usable IR for the supported portion.
            _ => {}
        }
    }
    if emitter.errors.is_empty() {
        Ok(emitter.out)
    } else {
        Err(emitter.errors)
    }
}

// ─── Internal emitter ──────────────────────────────────────────────────────

/// LLVM IR text emitter for one module.
struct Emitter<'a> {
    /// Module name (goes in the IR header).
    module_name: String,
    /// Accumulating output.
    out: String,
    /// SSA value-ID counter; reset per function.
    next_value_id: u32,
    /// Local binding map (per current function). Entries hold the
    /// source-name, the SSA value reference (e.g. `"%n"` / `"%tmp.3"`),
    /// and the recorded IR type so path-position references know what
    /// type to emit alongside the value. Reset per function. For
    /// function parameters, the value ref is `%<name>`; for `let`
    /// bindings, an SSA temp like `%tmp.7`.
    locals: Vec<LocalBinding>,
    /// Resolution from `clifford-resolve` — used to look up bindings
    /// when the typing path needs cross-referencing.
    #[allow(dead_code)] // forward-compat for slice 3+ (cross-fn sig lookup)
    resolution: &'a Resolution,
    /// Typing from `clifford-types` — authoritative source for every
    /// expression's type. Used by [`Self::expr_ir_type`] to pick the
    /// right LLVM IR type without falling back to syntactic guesses.
    typing: &'a Typing,
    /// Slice 3: registry of every `#automaton` in the program. Maps
    /// name → field-offset table so `#mutate` and `Auto.field`
    /// lowering can pick the right `getelementptr` index without
    /// re-walking the AST. Slice 4 extends this with per-field MMIO
    /// offsets for register-block automatons.
    automatons: HashMap<String, AutomatonInfo>,
    /// Slice 4: maps transition-name → owner-automaton-name. Used by
    /// the proc-call lowering path when the call is to a transition
    /// from *outside* a transition body (e.g. an `#effect` calling a
    /// transition, or an `#interrupt` dispatching one). The
    /// `enclosing_owner` field handles the in-transition case
    /// (slice 3); this map closes the cross-callable case.
    /// Built at pass-1 time alongside the automaton registry.
    transition_owners: HashMap<String, String>,
    /// Slice 3: current owning automaton when emitting a
    /// `#transition` body. `None` outside of transitions. Used to
    /// resolve `Self.field` to the correct automaton.
    enclosing_owner: Option<String>,
    /// Slice 21 (Decision #18): name of the audited automaton
    /// whose transition body is currently being emitted, if any.
    /// `Some(name)` ⇒ every unsafe-primitive emission site
    /// (`#unchecked_load` / `#unchecked_store` /
    /// `#unchecked_offset` / `#unchecked_cast` /
    /// `#volatile_load` / `#volatile_store`) prepends a
    /// `; audit-wrap site for <name>` IR comment indicating
    /// where a debug-build `PointerAuditor` call would be
    /// injected. `None` ⇒ no marker emitted; the unsafe op
    /// lowers as before, byte-identical to slice-20 output.
    ///
    /// The marker is established by [`Self::emit_audited_owner_if_needed`]
    /// which is called at the top of `emit_automaton_transitions`'
    /// per-transition loop (after the per-function reset, before
    /// any body statements). Cleared by `reset_per_function_state`
    /// so effects/interrupts (which run AFTER the transitions in
    /// `lower`'s pass-3 loop) start with a fresh `None`.
    current_audited_owner: Option<String>,
    /// Decision #22 codegen consumer: when the current callable is
    /// marked `Release` / `SeqCst`, this carries the LLVM fence
    /// ordering that must be emitted **before each `ret`**. `None`
    /// means no exit fence. Set per-callable in
    /// [`Self::emit_effect`] / [`Self::emit_interrupt`] /
    /// [`Self::emit_transition`].
    pending_exit_fence: Option<&'static str>,
    /// Slice 9: when emitting a `#transition name -> Dest { … }`
    /// inside a multi-state automaton, this carries
    /// `(automaton_name, dest_tag)` so a `store i32 <tag>, i32* …`
    /// is emitted before every `ret` exit (alongside any
    /// Decision #22 release / SeqCst fence). `None` for monoid
    /// transitions or transitions without a `-> Dest` target.
    pending_transition_tag_write: Option<(String, u32)>,
    /// Slice 11: name of the basic block currently being emitted
    /// into. Updated whenever a new label is written. Used by phi
    /// nodes (sigma loops) to identify their predecessor blocks.
    /// Reset to `"entry"` at the start of every function.
    current_block: String,
    /// Slice 11: monotonically incrementing per-function counter
    /// for fresh sigma-loop label IDs (`sigma.header.<n>`,
    /// `sigma.body.<n>`, `sigma.exit.<n>`). Reset to 0 at the
    /// start of every function.
    next_label_id: u32,
    /// Slice 11: `true` when the current basic block has been
    /// terminated by a `ret`. The sigma-loop emitter consults this
    /// before emitting the back-edge — emitting a second terminator
    /// would produce invalid LLVM IR. Reset to `false` whenever a
    /// new label opens a fresh basic block.
    current_block_terminated: bool,
    /// v0.2-ε: `true` when an `#atomic: interrupt_critical` body
    /// is open and needs an exit `cpsie i` (or target-equivalent
    /// unmask) before every `ret`. Set by `emit_atomic_entry_mask`
    /// at the start of each `#atomic` callable; consumed by
    /// `emit_atomic_exit_unmask_if_pending` at every exit site;
    /// cleared at end-of-callable.
    pending_atomic_exit_unmask: bool,
    /// Slice 17: stack of currently-open `sigma` loops.
    /// Each entry holds the loop's `(back_edge_label,
    /// exit_label)`. `break;` emits `br label %<exit>`;
    /// `continue;` emits `br label %<back_edge>` so the
    /// increment runs (going straight to the header would
    /// re-enter the loop with the SAME iteration index — an
    /// infinite loop). The back-edge label is a synthetic
    /// `sigma.continue.<id>` block that performs the
    /// increment and jumps to the header. Reset in
    /// `reset_per_function_state`; pushed/popped by
    /// `emit_sigma`.
    sigma_loop_stack: Vec<(String, String)>,
    /// Errors collected across the whole program.
    errors: Vec<CodegenError>,
}

/// Per-automaton info captured in pass 1 (slice 3+). The `fields` vec
/// preserves declaration order so `getelementptr` indices match LLVM
/// struct layout (for non-register-block automatons) or `(name, IR
/// type, optional offset)` for register-block fields (slice 4).
struct AutomatonInfo {
    /// Source name (matches `AutomatonDecl.name`).
    #[allow(dead_code)] // recorded for diagnostics; the map key is the canonical lookup
    name: String,
    /// `(field_name, ir_type, optional_offset)` triples in declaration
    /// order. For non-register-block automatons, `optional_offset`
    /// is always `None` and the index in this vec is the LLVM struct
    /// field index used by `getelementptr`. For register-block
    /// automatons, every field has `Some(offset_value)` (parsed from
    /// the `#offset: 0xHEX` clause; `clifford-check` enforces that
    /// every register-block field has an `#offset`).
    fields: Vec<(String, String, Option<u64>)>,
    /// `true` if this automaton is a register block (`#address: 0x…`
    /// clause present). Register-block automatons skip state-struct
    /// emission; field accesses lower to volatile loads/stores at
    /// `address + offset` (Decision #6).
    is_register_block: bool,
    /// Slice 4: parsed base address for register-block automatons,
    /// `0` for non-register-block. Sum of `address` + each field's
    /// `offset` is the absolute MMIO address of that field.
    base_address: u64,
    /// Slice 9: state-tag table for multi-state automatons. Each
    /// `(name, tag)` pair maps a `#states: [...]` entry to the
    /// integer encoding used in the state struct's tag field. Empty
    /// for monoid automatons (no `#states` clause). The first
    /// declared state always has tag 0 — that matches the global's
    /// `zeroinitializer` so the initial state needs no special
    /// emission.
    state_tags: Vec<(String, u32)>,
    /// Slice 20 (Decision #18): `true` if the source declared this
    /// automaton with the `#audit` modifier. When emitting any
    /// transition body of an audit automaton, [`Emitter`] sets
    /// `current_audited_owner = Some(name)`; every unsafe-primitive
    /// emission site (`#unchecked_load` / `#unchecked_store` /
    /// `#unchecked_offset` / `#unchecked_cast` /
    /// `#volatile_load` / `#volatile_store`) consults that field
    /// and prepends a `; audit-wrap site for <Name>` IR comment.
    ///
    /// **Slice 21 scope is the marker only.** The actual
    /// `PointerAuditor` dispatch (and the `ShadowSanitizer`
    /// stdlib impl) lands in subsequent slices once the stdlib
    /// has the runtime helpers in place. The marker establishes
    /// the wiring point and gives downstream consumers (and
    /// debug-build instrumentation passes) a stable place to
    /// inject the wrap.
    ///
    /// Effects and interrupts that target audited automatons via
    /// `#mutates: [...]` are **not** marked in slice 21 — only
    /// transition bodies of the audited automaton itself. The
    /// effects/interrupts case requires looking up every
    /// mutated automaton's audit flag at every primitive site,
    /// which is straightforward but defers to a follow-up slice
    /// once the transition-body case is exercised in real
    /// firmware.
    is_audited: bool,
    /// Slice 18 (Decision #12): `true` if the source declared this
    /// automaton with the `#staged` modifier. Two consequences:
    ///
    /// 1. State emission produces **two** globals:
    ///    `@<Name>.state` (live) and `@<Name>.shadow` (pending).
    /// 2. `#mutate` and mutation-sugar writes target the shadow
    ///    global instead of the live one. Reads (`Name.field`,
    ///    `@snapshot Name.field`) continue to come from live
    ///    state — the shadow's purpose is to buffer pending
    ///    writes until an explicit `#flush Name;` commits them
    ///    by `memcpy`'ing shadow → live.
    ///
    /// Register-block automatons cannot be `#staged` (no shadow
    /// makes sense for MMIO); the parser does not currently
    /// reject the combination, so the emitter falls back to
    /// direct-MMIO behaviour for register-block + `#staged`
    /// (a future slice can lift this to a parse-time error if
    /// firmware patterns prove the combination is always wrong).
    is_staged: bool,
}

impl AutomatonInfo {
    /// Slice 9: `true` iff this automaton has a `#states: [...]`
    /// clause (i.e. is multi-state). Monoid automatons return
    /// `false` and skip the state-tag field in the state struct.
    fn is_multi_state(&self) -> bool {
        !self.state_tags.is_empty()
    }

    /// Slice 9: return the LLVM struct-field index for the user-
    /// declared field at user-visible index `user_idx`. For
    /// multi-state automatons the state-tag occupies index 0, so
    /// user fields shift up by one. For monoid automatons the
    /// user index IS the LLVM index.
    fn llvm_field_index(&self, user_idx: usize) -> usize {
        if self.is_multi_state() {
            user_idx + 1
        } else {
            user_idx
        }
    }

    /// Slice 9: look up the integer tag for a state name.
    /// Returns `None` if `state_name` isn't in the `#states`
    /// list (or the automaton is monoid).
    fn state_tag(&self, state_name: &str) -> Option<u32> {
        self.state_tags
            .iter()
            .find_map(|(n, tag)| if n == state_name { Some(*tag) } else { None })
    }

    /// Slice 18 (Decision #12): the LLVM global symbol that
    /// receives **writes** to this automaton's fields. For
    /// `#staged` automata this is `@<Name>.shadow`; for the
    /// default direct-write case (and for register-block
    /// automata, which never have a shadow because they are
    /// MMIO) this is `@<Name>.state`.
    ///
    /// Used by `emit_field_store`, `emit_indexed_field_store`,
    /// and the transition tag-write sites — every code path
    /// that emits a `store` against the live state struct must
    /// route through this helper so that staged-automaton
    /// writes are buffered.
    fn write_global(&self) -> String {
        if self.is_staged && !self.is_register_block {
            format!("@{}.shadow", self.name)
        } else {
            format!("@{}.state", self.name)
        }
    }
}

/// One per-function local binding: source name + SSA-value ref + IR
/// type. Slice 2 tracks the IR type alongside the value so path-
/// position lookups don't need to re-walk the typing map.
///
/// Slice 12 added the `storage` field to discriminate between
/// SSA-direct bindings (the slice-1 path: `value` is the SSA name
/// holding the value) and stack-slot bindings (the new path for
/// `let mut`: `value` is an `alloca`-produced pointer; reads load
/// through it; writes store through it). Parameters and immutable
/// `let` keep the SSA-direct shape; only `let mut` uses the stack.
struct LocalBinding {
    name: String,
    value: String,
    ir_type: String,
    /// Slice 12: discriminator for read/write lowering.
    storage: LocalStorage,
}

/// Slice 12: how a local's value is held in IR.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum LocalStorage {
    /// `LocalBinding::value` is the SSA name that holds the value.
    /// Reads emit nothing; the SSA name is the value. Used for
    /// parameters, immutable `let`, `let :=`, and `sigma`-loop
    /// variables.
    Ssa,
    /// `LocalBinding::value` is an SSA name that holds an
    /// `alloca`-produced pointer (`<ir_type>*`). Reads emit
    /// `load <ir_type>, <ir_type>* %ptr`; writes emit
    /// `store <ir_type> %v, <ir_type>* %ptr`. Used for `let mut`.
    Stack,
}

impl<'a> Emitter<'a> {
    fn new(module_name: &str, resolution: &'a Resolution, typing: &'a Typing) -> Self {
        Self {
            module_name: module_name.to_owned(),
            out: String::new(),
            next_value_id: 0,
            locals: Vec::new(),
            resolution,
            typing,
            automatons: HashMap::new(),
            transition_owners: HashMap::new(),
            enclosing_owner: None,
            current_audited_owner: None,
            pending_exit_fence: None,
            pending_transition_tag_write: None,
            current_block: "entry".to_owned(),
            next_label_id: 0,
            current_block_terminated: false,
            pending_atomic_exit_unmask: false,
            sigma_loop_stack: Vec::new(),
            errors: Vec::new(),
        }
    }

    /// Pass 1: build the [`AutomatonInfo`] registry from every
    /// `Item::Automaton` in the program. Records field-name → IR
    /// type and:
    /// - For non-register-block automatons: the field's index in the
    ///   `getelementptr` table.
    /// - For register-block automatons (`#address: 0x…`): the
    ///   per-field offset, parsed from the `#offset: 0xHEX` clause.
    ///   Slice 4 then lowers register-block field accesses to
    ///   volatile loads/stores at `base_address + offset`.
    fn collect_automatons(&mut self, program: &Program) {
        for item in &program.items {
            if let Item::Automaton(decl) = item {
                let is_register_block = decl.address.is_some();
                let base_address = decl
                    .address
                    .as_ref()
                    .and_then(|a| parse_address_literal(&a.value))
                    .unwrap_or(0);

                let mut fields: Vec<(String, String, Option<u64>)> =
                    Vec::with_capacity(decl.fields.len());
                for f in &decl.fields {
                    let ir_ty = self.lower_type(&f.ty).unwrap_or_else(|e| {
                        self.errors.push(e);
                        "i32".to_owned()
                    });
                    let offset = if is_register_block {
                        // `clifford-check` enforces that every
                        // register-block field has `#offset:` per
                        // Decision #6, so a missing offset here is an
                        // upstream-validation gap. Defensive: record
                        // 0 and continue. The diagnostic surfaces
                        // upstream.
                        f.offset
                            .as_deref()
                            .and_then(parse_address_literal)
                            .or(Some(0))
                    } else {
                        None
                    };
                    fields.push((f.name.clone(), ir_ty, offset));
                }
                // Slice 9: build state-tag table from the
                // `#states: [...]` clause (Decision #5). The first
                // listed state is the initial state and gets tag 0;
                // subsequent states get sequential tags. Empty for
                // monoid automatons (no `#states` clause).
                let state_tags: Vec<(String, u32)> = decl
                    .states
                    .as_ref()
                    .map(|names| {
                        names
                            .iter()
                            .enumerate()
                            .map(|(i, sn)| (sn.name.clone(), i as u32))
                            .collect()
                    })
                    .unwrap_or_default();

                self.automatons.insert(
                    decl.name.clone(),
                    AutomatonInfo {
                        name: decl.name.clone(),
                        fields,
                        is_register_block,
                        base_address,
                        state_tags,
                        is_staged: decl.staged,
                        is_audited: decl.audited,
                    },
                );
                // Slice 4: record transition→owner mapping so
                // proc-call lowering can mangle cross-callable
                // transition references like `#> send()` from inside
                // an `#interrupt` that lists the transition's
                // automaton in its `#mutates`.
                for tr in &decl.transitions {
                    self.transition_owners
                        .entry(tr.name.clone())
                        .or_insert_with(|| decl.name.clone());
                }
            }
        }
    }

    /// Pass 2: emit the `%struct.<Name> = type { … }` definition and
    /// `@<Name>.state = global … zeroinitializer` for every
    /// non-register-block automaton.
    fn emit_automaton_state_structs(&mut self, program: &Program) {
        for item in &program.items {
            let Item::Automaton(decl) = item else {
                continue;
            };
            if decl.address.is_some() {
                // Register-block automaton — slice 4 work.
                continue;
            }
            let Some(info) = self.automatons.get(&decl.name) else {
                continue;
            };
            // Emit struct type.
            //
            // Slice 9: multi-state automatons prepend an `i32` state-
            // tag at LLVM struct-field index 0. The user-declared
            // fields shift up by one — see [`AutomatonInfo::llvm_field_index`].
            // The global stays `zeroinitializer` since the first
            // declared state always has tag 0.
            let mut parts: Vec<String> = Vec::with_capacity(info.fields.len() + 1);
            if info.is_multi_state() {
                parts.push("i32".to_owned()); // state tag
            }
            for (_, ty, _) in &info.fields {
                parts.push(ty.clone());
            }
            writeln!(
                &mut self.out,
                "%struct.{name} = type {{ {fields} }}",
                name = decl.name,
                fields = parts.join(", "),
            )
            .ok();
            // Emit zero-initialised global state instance.
            writeln!(
                &mut self.out,
                "@{name}.state = global %struct.{name} zeroinitializer",
                name = decl.name,
            )
            .ok();
            // Slice 18 (Decision #12): `#staged` automata get a
            // second global of identical type — the *shadow*
            // — into which `#mutate` writes are redirected. The
            // shadow stays consistent with the live struct's
            // initial zero state so a flush issued before any
            // intervening mutation is a memcpy of zeros (no-op
            // semantically; the user gets uniform behaviour
            // across the program lifecycle). Multi-state
            // automatons include the i32 state tag in the
            // shadow too — the v0.2 semantics is "shadow ==
            // pending replacement of the entire state struct";
            // a future refinement could shadow only user fields
            // but the cost is non-trivial (separate types) and
            // the wins are minor for the firmware patterns this
            // serves.
            if decl.staged {
                writeln!(
                    &mut self.out,
                    "@{name}.shadow = global %struct.{name} zeroinitializer",
                    name = decl.name,
                )
                .ok();
            }
            writeln!(&mut self.out).ok();
        }
    }

    /// Slice 18 (Decision #12): emit the `@llvm.memcpy` declaration
    /// once at module scope iff the program contains at least one
    /// `#staged` automaton. Skipping the declaration when no flush
    /// can possibly be emitted keeps non-staged programs' IR byte-
    /// identical to pre-slice-18 output.
    ///
    /// We use `@llvm.memcpy.p0.p0.i64` (i8\* dest, i8\* src, i64
    /// length, i1 isvolatile). The opaque-pointer form
    /// (`p0.p0` = ptr-to-ptr) is the LLVM-15+ canonical spelling;
    /// the `i64` length keeps the same intrinsic across 32-bit and
    /// 64-bit targets (the high bits are zero on 32-bit and LLVM
    /// truncates to the target pointer width internally).
    fn emit_staged_intrinsics_if_needed(&mut self, program: &Program) {
        let any_staged = program.items.iter().any(|item| {
            matches!(item, Item::Automaton(decl) if decl.staged && decl.address.is_none())
        });
        if !any_staged {
            return;
        }
        writeln!(
            &mut self.out,
            "declare void @llvm.memcpy.p0.p0.i64(i8*, i8*, i64, i1)",
        )
        .ok();
        writeln!(&mut self.out).ok();
    }

    /// Slice 18 (Decision #12): lower `#flush Name;` to a memcpy
    /// from `@Name.shadow` to `@Name.state`.
    ///
    /// The byte length is computed at IR-time via the
    /// `getelementptr (T\* null, 1) -> ptrtoint -> i64` idiom so
    /// the lowering is target-pointer-width-agnostic and doesn't
    /// require us to know the struct's layout in bytes. LLVM
    /// constant-folds this to a literal at the IR-to-machine
    /// translation step, so there's no runtime cost.
    ///
    /// Returns `CodegenError::UnresolvedName` if `automaton` is
    /// not in the registry (the resolver should have caught this
    /// upstream as E0413; the error here is a defence in depth).
    /// Returns `CodegenError::NotYetImplemented` if the named
    /// automaton is not `#staged` (resolver-side E0412 should
    /// have caught this; same defence-in-depth posture).
    fn emit_flush(&mut self, automaton: &str) -> Result<(), CodegenError> {
        let info = self.automatons.get(automaton).ok_or_else(|| {
            CodegenError::UnresolvedName {
                name: automaton.to_owned(),
            }
        })?;
        if !info.is_staged {
            return Err(CodegenError::NotYetImplemented {
                what: "`#flush` on a non-staged automaton (resolver should have caught this; please file a bug)",
            });
        }
        if info.is_register_block {
            return Err(CodegenError::NotYetImplemented {
                what: "`#flush` on a register-block automaton (no shadow exists; the combination is undefined for v0.2)",
            });
        }

        // Compute the struct size via the GEP-on-null idiom. LLVM
        // constant-folds the result to the literal byte count.
        let size_ptr = self.fresh_value();
        writeln!(
            &mut self.out,
            "  {size_ptr} = getelementptr %struct.{automaton}, %struct.{automaton}* null, i32 1",
        )
        .ok();
        let size_int = self.fresh_value();
        writeln!(
            &mut self.out,
            "  {size_int} = ptrtoint %struct.{automaton}* {size_ptr} to i64",
        )
        .ok();

        // Bitcast both globals to i8* (the memcpy intrinsic takes
        // i8* arguments). For LLVM-15+ opaque pointers these are
        // formal no-ops but we keep the bitcast spelling for
        // compatibility with the older typed-pointer dialect.
        let dst_ptr = self.fresh_value();
        writeln!(
            &mut self.out,
            "  {dst_ptr} = bitcast %struct.{automaton}* @{automaton}.state to i8*",
        )
        .ok();
        let src_ptr = self.fresh_value();
        writeln!(
            &mut self.out,
            "  {src_ptr} = bitcast %struct.{automaton}* @{automaton}.shadow to i8*",
        )
        .ok();
        writeln!(
            &mut self.out,
            "  call void @llvm.memcpy.p0.p0.i64(i8* {dst_ptr}, i8* {src_ptr}, i64 {size_int}, i1 false) ; #flush {automaton} (Decision #12)",
        )
        .ok();
        Ok(())
    }

    /// Emit one LLVM function per `#effect` declaration. Effects are
    /// lowered like `@fn` but with mutation access to the automatons
    /// listed in their `#mutates` clause; this slice's body walker
    /// handles `#mutate` / mutation-sugar / automaton-field reads.
    ///
    /// Decision #22 codegen consumer: if the trait list contains
    /// `Acquire` / `Release` / `SeqCst`, an LLVM `fence` is emitted
    /// at the function entry and/or before each `ret`. See
    /// [`memory_ordering_from_traits`].
    fn emit_effect(&mut self, decl: &EffectDecl) {
        // Reset per-function state.
        self.reset_per_function_state();

        let ret_ty = self.lower_return_type(decl.return_type.as_ref());

        let mut sig_parts: Vec<String> = Vec::with_capacity(decl.params.len());
        for p in &decl.params {
            match self.lower_param(p) {
                Ok(s) => sig_parts.push(s),
                Err(e) => {
                    self.errors.push(e);
                    return;
                }
            }
            let p_ir_ty = self.lower_type(&p.ty).unwrap_or_else(|_| "i32".to_owned());
            self.locals.push(LocalBinding {
                name: p.name.clone(),
                value: format!("%{}", p.name),
                ir_type: p_ir_ty,
                // Parameters are SSA-direct: the param SSA name
                // holds the value. `let mut` is the only thing
                // that uses Stack storage today.
                storage: LocalStorage::Ssa,
            });
        }

        // Decision #22 fence selection.
        let ordering = memory_ordering_from_traits(&decl.trait_list);
        self.pending_exit_fence = ordering.exit;

        writeln!(
            &mut self.out,
            "define {ret_ty} @{name}({params}) {{",
            name = decl.name,
            params = sig_parts.join(", "),
        )
        .ok();
        writeln!(&mut self.out, "entry:").ok();
        if let Some(entry_fence) = ordering.entry {
            writeln!(&mut self.out, "  fence {entry_fence}").ok();
        }
        self.emit_atomic_entry_mask(decl.atomic.as_ref());
        self.emit_block(&decl.body, &ret_ty);
        writeln!(&mut self.out, "}}").ok();
        writeln!(&mut self.out).ok();

        self.pending_exit_fence = None;
    }

    /// Emit one LLVM function per `#interrupt` declaration. Slice 3
    /// emits the function with the linker symbol matching the source
    /// name (Decision #10); the target-specific calling convention,
    /// `.interrupts` section attribute, and disable-interrupts wrapper
    /// per Refinement #5e are slice-4 work. Decision #22 fence
    /// emission is handled identically to effects.
    fn emit_interrupt(&mut self, decl: &InterruptDecl) {
        // Reset per-function state.
        self.reset_per_function_state();

        let ret_ty = self.lower_return_type(decl.return_type.as_ref());

        let mut sig_parts: Vec<String> = Vec::with_capacity(decl.params.len());
        for p in &decl.params {
            match self.lower_param(p) {
                Ok(s) => sig_parts.push(s),
                Err(e) => {
                    self.errors.push(e);
                    return;
                }
            }
            let p_ir_ty = self.lower_type(&p.ty).unwrap_or_else(|_| "i32".to_owned());
            self.locals.push(LocalBinding {
                name: p.name.clone(),
                value: format!("%{}", p.name),
                ir_type: p_ir_ty,
                // Parameters are SSA-direct: the param SSA name
                // holds the value. `let mut` is the only thing
                // that uses Stack storage today.
                storage: LocalStorage::Ssa,
            });
        }

        // Decision #22 fence selection.
        let ordering = memory_ordering_from_traits(&decl.trait_list);
        self.pending_exit_fence = ordering.exit;

        // Linker symbol = source name (Decision #10). Slice 4 adds
        // the `section ".interrupts"` attribute so the linker places
        // every `#interrupt` handler in a single contiguous section
        // the startup code can reference for the vector table.
        // Target-specific calling-convention attribute (e.g.
        // `cc 87` for ARM `thumb_intrcc`) is left to a later slice
        // when the target-data-layout pass lands; LLVM's default
        // target cc handles common cases for v0.1.
        writeln!(
            &mut self.out,
            "define {ret_ty} @{name}({params}) section \".interrupts\" {{",
            name = decl.name,
            params = sig_parts.join(", "),
        )
        .ok();
        writeln!(&mut self.out, "entry:").ok();
        if let Some(entry_fence) = ordering.entry {
            writeln!(&mut self.out, "  fence {entry_fence}").ok();
        }
        self.emit_atomic_entry_mask(decl.atomic.as_ref());
        self.emit_block(&decl.body, &ret_ty);
        writeln!(&mut self.out, "}}").ok();
        writeln!(&mut self.out).ok();

        self.pending_exit_fence = None;
    }

    /// Emit one LLVM function per `#transition` inside this
    /// `#automaton`. Transitions are named `<AutomatonName>_<transition>`
    /// in IR (`Counter_tick` for `#automaton Counter { #transition tick
    /// { … } }`) so there's no name clash across automatons. The
    /// owning-automaton context is set so `Self.field` reads resolve.
    fn emit_automaton_transitions(&mut self, decl: &AutomatonDecl) {
        // Slice 4: register-block automatons CAN have transitions
        // now — their field accesses go through volatile loads/stores
        // instead of the GEP+load/store path. Transitions on
        // register blocks were the slice-3 deferral that this slice
        // closes.
        for tr in &decl.transitions {
            self.emit_transition(&decl.name, tr);
        }
    }

    fn emit_transition(&mut self, owner: &str, decl: &TransitionDecl) {
        // Reset per-function state. Transitions then immediately
        // set enclosing_owner so `Self.field` resolves correctly.
        self.reset_per_function_state();
        self.enclosing_owner = Some(owner.to_owned());
        // Slice 21 (Decision #18): if the owning automaton is
        // `#audit`-marked, set the audit context so every
        // unsafe-primitive emission inside the body emits a
        // `; audit-wrap site for <Owner>` IR comment. Cleared
        // by the next `reset_per_function_state` call so this
        // is strictly transition-scoped.
        if let Some(info) = self.automatons.get(owner) {
            if info.is_audited {
                self.current_audited_owner = Some(owner.to_owned());
            }
        }

        let fn_name = format!("{owner}_{tr}", tr = decl.name);

        // Decision #22 fence selection.
        let ordering = memory_ordering_from_traits(&decl.trait_list);
        self.pending_exit_fence = ordering.exit;

        // Slice 9: if this transition has a `-> Dest` target on a
        // multi-state automaton, set up a pending tag write so a
        // `store i32 <dest_tag>, i32* …` is emitted before each
        // `ret`. The resolver has already validated that the
        // destination is one of the automaton's `#states`; we
        // surface a defensive `NotYetImplemented` if it isn't, in
        // case codegen runs ahead of upstream validation.
        self.pending_transition_tag_write = match &decl.destination {
            Some(dest) => {
                let info = self.automatons.get(owner);
                match info.and_then(|i| i.state_tag(dest)) {
                    Some(tag) => Some((owner.to_owned(), tag)),
                    None => {
                        // Either not multi-state, or `dest` isn't a
                        // declared state. Either way, the upstream
                        // validation should have rejected this; we
                        // skip emission but record an error so the
                        // user sees something concrete.
                        if info.map(|i| i.is_multi_state()).unwrap_or(false) {
                            self.errors.push(CodegenError::UnresolvedName {
                                name: format!("{owner}#states[{dest}]"),
                            });
                        }
                        None
                    }
                }
            }
            None => None,
        };

        // Slice 3: transitions take no value parameters at the AST
        // level (Decision #5 / Refinement #5b restricts transition
        // signatures). The generated IR fn signature is `void`.
        writeln!(&mut self.out, "define void @{fn_name}() {{").ok();
        writeln!(&mut self.out, "entry:").ok();
        if let Some(entry_fence) = ordering.entry {
            writeln!(&mut self.out, "  fence {entry_fence}").ok();
        }
        self.emit_atomic_entry_mask(decl.atomic.as_ref());
        self.emit_block(&decl.body, "void");
        writeln!(&mut self.out, "}}").ok();
        writeln!(&mut self.out).ok();

        self.enclosing_owner = None;
        self.pending_exit_fence = None;
        self.pending_transition_tag_write = None;
    }

    fn emit_module_header(&mut self) {
        writeln!(&mut self.out, "; ModuleID = '{}'", self.module_name).ok();
        writeln!(&mut self.out, "source_filename = \"{}\"", self.module_name).ok();
        writeln!(&mut self.out).ok();
    }

    /// Allocate a fresh SSA value ID. Returns `"%tmp.<n>"`.
    fn fresh_value(&mut self) -> String {
        let id = self.next_value_id;
        self.next_value_id += 1;
        format!("%tmp.{id}")
    }

    /// Slice 11: reset all per-function emitter state. Called at
    /// the top of every callable emitter (`emit_fn`, `emit_effect`,
    /// `emit_interrupt`, `emit_transition`) so each callable gets
    /// fresh SSA-id, label-id, locals, and basic-block tracking.
    /// Avoids four copies of the same reset block drifting out of
    /// sync as new fields are added.
    fn reset_per_function_state(&mut self) {
        self.next_value_id = 0;
        self.next_label_id = 0;
        self.locals.clear();
        self.enclosing_owner = None;
        // Slice 21: clear the audit-context marker. Re-set per
        // transition body in `emit_automaton_transitions` when the
        // owning automaton is `#audit`-marked. Effects, interrupts,
        // and `@fn`s start with `None` and stay there for v0.21.
        self.current_audited_owner = None;
        self.current_block = "entry".to_owned();
        self.current_block_terminated = false;
        self.pending_atomic_exit_unmask = false;
        self.sigma_loop_stack.clear();
    }

    fn emit_fn(&mut self, decl: &FnDecl) {
        // Reset per-function state.
        self.reset_per_function_state();

        let ret_ty = self.lower_return_type(decl.return_type.as_ref());

        // Param list: each param's source name is also its IR value
        // name (`%<name>`) for slice-1+ simplicity. Future slice with
        // a richer name-resolution pass may rename to avoid clashes.
        let mut sig_parts: Vec<String> = Vec::with_capacity(decl.params.len());
        for p in &decl.params {
            match self.lower_param(p) {
                Ok(s) => sig_parts.push(s),
                Err(e) => {
                    self.errors.push(e);
                    return;
                }
            }
            // Register the param as a local: name → `%<name>`, with
            // its IR type for later path-lookup typing (slice 2+).
            let p_ir_ty = self.lower_type(&p.ty).unwrap_or_else(|_| "i32".to_owned());
            self.locals.push(LocalBinding {
                name: p.name.clone(),
                value: format!("%{}", p.name),
                ir_type: p_ir_ty,
                // Parameters are SSA-direct: the param SSA name
                // holds the value. `let mut` is the only thing
                // that uses Stack storage today.
                storage: LocalStorage::Ssa,
            });
        }

        writeln!(
            &mut self.out,
            "define {ret_ty} @{name}({params}) {{",
            name = decl.name,
            params = sig_parts.join(", "),
        )
        .ok();
        writeln!(&mut self.out, "entry:").ok();

        self.emit_block(&decl.body, &ret_ty);

        writeln!(&mut self.out, "}}").ok();
        writeln!(&mut self.out).ok();
    }

    fn lower_param(&mut self, p: &Param) -> Result<String, CodegenError> {
        let ty = self.lower_type(&p.ty)?;
        Ok(format!("{ty} %{name}", name = p.name))
    }

    /// Lower a `Block` of statements. The terminating `return` (if
    /// present) emits the `ret` instruction; if no return is present
    /// and the function has unit return type, emits an implicit `ret
    /// void`. If no return is present but the function has a non-unit
    /// return type, emits `unreachable` and records an error.
    fn emit_block(&mut self, block: &Block, ret_ty: &str) {
        // Slice 13: drive termination off `current_block_terminated`
        // rather than a top-level Return-statement check. This
        // correctly handles the case where the LAST top-level
        // statement is an `if`/`sigma`/`return`-bearing block whose
        // branches all terminate — the function's current block is
        // already terminated by the time we reach the end-of-block
        // check, so we don't synthesize a redundant terminator.
        for stmt in &block.stmts {
            if self.current_block_terminated {
                // Statements after a terminator are dead — skip
                // silently. (Same fall-through semantics as before;
                // future slice may want a warning.)
                break;
            }
            self.emit_stmt(stmt);
        }
        if !self.current_block_terminated {
            // No terminator on the open block. Two cases:
            //   - void return: emit `ret void` (with any pending
            //     exit fence and slice-9 transition tag write).
            //   - non-void return: emit `unreachable` to close the
            //     block. This satisfies LLVM's "every basic block
            //     ends in a terminator" rule. We deliberately do
            //     not push an error here — the resolver / type
            //     checker is the right place to detect "a non-unit
            //     fn that may fall off the end without returning",
            //     and the IR `unreachable` instruction has the
            //     same UB semantics as the source-level oversight.
            if ret_ty == "void" {
                self.emit_exit_fence_if_pending();
                writeln!(&mut self.out, "  ret void").ok();
            } else {
                writeln!(&mut self.out, "  unreachable").ok();
            }
            self.current_block_terminated = true;
        }
    }

    /// v0.2-ε: emit the runtime interrupt-mask instruction at the
    /// start of an `#atomic: interrupt_critical` body, and queue the
    /// matching unmask for every `ret` exit.
    ///
    /// Cortex-M emits `cpsid i` (PRIMASK ← 1, all maskable
    /// interrupts disabled). The same IR works on QEMU's
    /// `lm3s6965evb` board which is what the QEMU CI test targets.
    /// Other architectures (x86, RISC-V) need different sequences;
    /// for v0.2-ε MVP we always emit the Cortex-M form. A future
    /// `cliffordc compile --target` slice will switch on the
    /// requested triple.
    ///
    /// **Codegen ↔ verifier soundness contract.** `clifford-ortho`
    /// (v0.2-δ) trusts that `#atomic: interrupt_critical` bodies
    /// run with interrupts masked. v0.2-ε makes that trust valid
    /// at runtime by emitting the actual masking. The two slices
    /// together close the gap that v0.2-δ deliberately documented.
    ///
    /// `MulticoreCritical` and `Custom(_)` are accepted by the
    /// parser but produce only an IR comment + a structured
    /// `NotYetImplemented` error so codegen rejects the program
    /// rather than silently producing unsafe binaries.
    fn emit_atomic_entry_mask(&mut self, atomic: Option<&clifford_ast::AtomicKind>) {
        let Some(kind) = atomic else { return };
        match kind {
            clifford_ast::AtomicKind::InterruptCritical => {
                // LLVM IR inline-asm form. The empty constraint
                // string is correct for a side-effecting
                // instruction with no operands.
                writeln!(
                    &mut self.out,
                    "  call void asm sideeffect \"cpsid i\", \"\"() ; #atomic: interrupt_critical entry (mask all maskable interrupts)"
                )
                .ok();
                self.pending_atomic_exit_unmask = true;
            }
            clifford_ast::AtomicKind::MulticoreCritical => {
                // Reserved for Decision #21 (v0.7+). Codegen for
                // the inter-core lock acquire/release isn't
                // wired yet.
                self.errors.push(CodegenError::NotYetImplemented {
                    what: "#atomic: multicore_critical (Decision #21 lock machinery, v0.7+)",
                });
            }
            clifford_ast::AtomicKind::Custom(name) => {
                // User-defined atomicity scope — codegen has no
                // way to know what masking semantics to emit.
                // Surface as NotYet rather than silently
                // ignoring.
                let _ = name;
                self.errors.push(CodegenError::NotYetImplemented {
                    what: "#atomic: <custom> kind (codegen only knows interrupt_critical today)",
                });
            }
            // `AtomicKind` is `#[non_exhaustive]`; future variants
            // need their own arm above. Defensively skip.
            _ => {}
        }
    }

    /// v0.2-ε: emit the matching unmask instruction at every `ret`
    /// site if an `#atomic: interrupt_critical` mask is currently
    /// pending. Called from `emit_exit_fence_if_pending` AFTER the
    /// release fence so the order at exit is:
    ///
    /// 1. State-tag write (slice 9)
    /// 2. Release / SeqCst fence (Decision #22) — publishes prior
    ///    writes to other agents.
    /// 3. **`cpsie i`** (this method) — re-enables interrupts.
    /// 4. `ret` — return to caller.
    ///
    /// This order matters: the fence completes BEFORE interrupts
    /// can fire, so a now-pending interrupt sees the published
    /// state. Reversed order would let an interrupt see partial
    /// state.
    fn emit_atomic_exit_unmask_if_pending(&mut self) {
        if self.pending_atomic_exit_unmask {
            writeln!(
                &mut self.out,
                "  call void asm sideeffect \"cpsie i\", \"\"() ; #atomic: interrupt_critical exit (unmask)"
            )
            .ok();
        }
    }

    /// Decision #22 + Slice 9 + v0.2-ε: emit any pending exit-time
    /// writes just before a `ret` is written. Called from every
    /// site that emits a `ret` so the contracts are honoured at
    /// every exit path.
    ///
    /// Order matters: the state-tag write happens BEFORE the
    /// release / SeqCst fence so the new state is visible to other
    /// agents only once the fence makes it so. The atomic unmask
    /// (v0.2-ε) happens AFTER the fence — the fence has to complete
    /// before interrupts can fire, otherwise a pending IRQ would
    /// see partial state.
    fn emit_exit_fence_if_pending(&mut self) {
        // Slice 9: emit the destination state-tag write before any
        // fence. Cloning the pair is fine — it's a one-shot per
        // `ret` and the table is small.
        if let Some((auto, tag)) = self.pending_transition_tag_write.clone() {
            // Stage 1: pointer to the state-tag field (LLVM index 0
            // for multi-state automatons by construction).
            //
            // Slice 18 (Decision #12): for `#staged` automata the
            // tag write is part of the deferred-mutation set —
            // route it to the shadow global so a `#flush` commits
            // both field updates AND the destination state tag in
            // one memcpy. This preserves the "atomic commit"
            // invariant for staged transitions.
            let target_global = self
                .automatons
                .get(&auto)
                .map(|info| info.write_global())
                .unwrap_or_else(|| format!("@{auto}.state"));
            let tag_ptr = self.fresh_value();
            writeln!(
                &mut self.out,
                "  {tag_ptr} = getelementptr %struct.{auto}, %struct.{auto}* {target_global}, i32 0, i32 0",
            )
            .ok();
            // Stage 2: store the destination tag.
            writeln!(
                &mut self.out,
                "  store i32 {tag}, i32* {tag_ptr}",
            )
            .ok();
        }
        if let Some(ordering) = self.pending_exit_fence {
            writeln!(&mut self.out, "  fence {ordering}").ok();
        }
        // v0.2-ε: unmask interrupts AFTER the release fence so the
        // fence's publication completes before any pending IRQ can
        // observe the state.
        self.emit_atomic_exit_unmask_if_pending();
    }

    fn emit_stmt(&mut self, stmt: &Stmt) {
        match &stmt.kind {
            StmtKind::Return(Some(e)) => {
                // Compute the return-value IR type *before* emit_expr
                // takes a mutable borrow on self.
                let ret_ty = self.expr_ir_type(e);
                match self.emit_expr(e) {
                    Ok(v) => {
                        // Decision #22: emit Release / SeqCst fence
                        // before the ret if the enclosing callable
                        // declared one.
                        self.emit_exit_fence_if_pending();
                        writeln!(&mut self.out, "  ret {ret_ty} {v}").ok();
                        self.current_block_terminated = true;
                    }
                    Err(err) => self.errors.push(err),
                }
            }
            StmtKind::Return(None) => {
                self.emit_exit_fence_if_pending();
                writeln!(&mut self.out, "  ret void").ok();
                self.current_block_terminated = true;
            }
            StmtKind::Let { name, ty, value, mutable } => {
                let ir_ty = match ty {
                    Some(annotated) => self.lower_type(annotated).unwrap_or_else(|e| {
                        self.errors.push(e);
                        "i32".to_owned() // best-effort fallback
                    }),
                    None => self.expr_ir_type(value),
                };
                match self.emit_expr(value) {
                    Ok(v) => {
                        if *mutable {
                            // Slice 12: `let mut name = …` —
                            // allocate a stack slot and store the
                            // initial value. Subsequent reads load
                            // through the alloca pointer; subsequent
                            // assigns store through it.
                            let ptr = self.fresh_value();
                            writeln!(
                                &mut self.out,
                                "  {ptr} = alloca {ir_ty}",
                            )
                            .ok();
                            writeln!(
                                &mut self.out,
                                "  store {ir_ty} {v}, {ir_ty}* {ptr}",
                            )
                            .ok();
                            self.locals.push(LocalBinding {
                                name: name.clone(),
                                value: ptr,
                                ir_type: ir_ty,
                                storage: LocalStorage::Stack,
                            });
                        } else {
                            // Immutable `let` — slice-1 SSA-direct
                            // path. The bind_via_identity helper
                            // gives the binding a stable SSA name
                            // that downstream code can reference.
                            let bind = self.bind_via_identity(&ir_ty, &v);
                            self.locals.push(LocalBinding {
                                name: name.clone(),
                                value: bind,
                                ir_type: ir_ty,
                                storage: LocalStorage::Ssa,
                            });
                        }
                    }
                    Err(e) => self.errors.push(e),
                }
            }
            StmtKind::LetShort { name, value, .. } => {
                let ir_ty = self.expr_ir_type(value);
                match self.emit_expr(value) {
                    Ok(v) => {
                        let bind = self.bind_via_identity(&ir_ty, &v);
                        self.locals.push(LocalBinding {
                            name: name.clone(),
                            value: bind,
                            ir_type: ir_ty,
                            // `let :=` is always immutable per Decision #8.
                            storage: LocalStorage::Ssa,
                        });
                    }
                    Err(e) => self.errors.push(e),
                }
            }
            StmtKind::Expr(e) => {
                // Discard result; emit for side effects (calls, etc.).
                if let Err(err) = self.emit_expr(e) {
                    self.errors.push(err);
                }
            }
            // Slice 3: `#mutate Auto { f1 = e1, f2 = e2, … };` —
            // each field-assign lowers to a getelementptr+store pair.
            StmtKind::Mutate { automaton, assigns } => {
                if let Err(e) = self.emit_mutate(automaton, assigns) {
                    self.errors.push(e);
                }
            }
            // Slice 3: `Auto.field <op>= expr;` sugar — single-field
            // form; for `=` it's a plain getelementptr+store, for
            // `<op>=` it's load+op+store.
            StmtKind::MutateShort {
                automaton,
                field,
                op,
                value,
                ..
            } => {
                if let Err(e) = self.emit_mutate_short(automaton, field, *op, value) {
                    self.errors.push(e);
                }
            }
            // Slice 3: `#> name(args);` — direct LLVM call to the
            // named effect / transition function. Transitions are
            // namespaced as `<Owner>_<name>` to avoid clashes; for
            // single-segment proc names the resolver tells us
            // whether the callee is an effect or a transition, so
            // codegen consults the resolution-time binding to pick
            // the right symbol.
            StmtKind::ProcCall { name, args } => {
                if let Err(e) = self.emit_proc_call(stmt, name, args) {
                    self.errors.push(e);
                }
            }
            // Slice 8: Decision #17 unsafe-store primitives. The
            // statement form discards the result (stores don't
            // produce values); both lower to a single LLVM `store`
            // (volatile when the source variant says so).
            StmtKind::UncheckedStore { ty, ptr, value } => {
                if let Err(e) = self.emit_unchecked_store(ty, ptr, value, false) {
                    self.errors.push(e);
                }
            }
            StmtKind::VolatileStore { ty, ptr, value } => {
                if let Err(e) = self.emit_unchecked_store(ty, ptr, value, true) {
                    self.errors.push(e);
                }
            }
            // Decision #14 / §5.8: `sigma var in source { body }`
            // bounded-iteration loop. Lowers to a counted-loop CFG
            // (header + body + exit blocks) per spec §8.4. v0.1
            // scope: range sources only.
            StmtKind::Sigma { var, source, body } => {
                if let Err(e) = self.emit_sigma(var, source, body) {
                    self.errors.push(e);
                }
            }
            // Slice 17: `break;` and `continue;` for sigma loops.
            // The resolver has already enforced lexical nesting
            // (E0411 outside any loop). Codegen branches to the
            // innermost loop's exit / continue label and marks
            // the current block terminated so the surrounding
            // emit walker treats subsequent statements as dead.
            StmtKind::Break => {
                if let Some((_, exit_label)) = self.sigma_loop_stack.last().cloned() {
                    writeln!(&mut self.out, "  br label %{exit_label}").ok();
                    self.current_block_terminated = true;
                } else {
                    // Resolver should have caught this; emit a
                    // structured error so the user sees something
                    // concrete if upstream gates are bypassed.
                    self.errors.push(CodegenError::NotYetImplemented {
                        what: "`break` outside a sigma loop (resolver should have caught this; please file a bug)",
                    });
                }
            }
            StmtKind::Continue => {
                if let Some((continue_label, _)) =
                    self.sigma_loop_stack.last().cloned()
                {
                    writeln!(&mut self.out, "  br label %{continue_label}").ok();
                    self.current_block_terminated = true;
                } else {
                    self.errors.push(CodegenError::NotYetImplemented {
                        what: "`continue` outside a sigma loop (resolver should have caught this; please file a bug)",
                    });
                }
            }
            // Slice 12: `name = expr;` — local mutable re-assignment.
            // The resolver has already verified that `name` is
            // `let mut`-declared, so the binding's storage is
            // Stack (alloca pointer). Lowers to a single store.
            StmtKind::Assign { name, value } => {
                if let Err(e) = self.emit_local_assign(name, value) {
                    self.errors.push(e);
                }
            }
            // Slice 13: `if cond { … } else { … }` — statement-form
            // conditional. Lowers to a `br i1` plus a then-block, an
            // optional else-block, and a merge label. Subsequent
            // statements emit into the merge block.
            StmtKind::If {
                cond,
                then_block,
                else_block,
            } => {
                if let Err(e) = self.emit_if(cond, then_block, else_block.as_ref()) {
                    self.errors.push(e);
                }
            }
            // Slice 18 (Decision #12): `#flush Name;` — commit a
            // `#staged` automaton's shadow into its live state.
            // Resolver has already verified the target is a
            // `#staged` automaton (E0412 / E0413), so we just
            // emit the memcpy here. Lowering shape:
            //
            //   call void @llvm.memcpy.p0.p0.i64(
            //       i8* bitcast (%struct.Name* @Name.state to i8*),
            //       i8* bitcast (%struct.Name* @Name.shadow to i8*),
            //       i64 ptrtoint (%struct.Name* getelementptr
            //           (%struct.Name, %struct.Name* null, i32 1)
            //           to i64),
            //       i1 false
            //   )
            //
            // The `getelementptr null, 1` idiom yields the struct's
            // size in bytes without us having to know the target
            // pointer width. The trailing `i1 false` is the
            // `isvolatile` argument.
            StmtKind::Flush { automaton } => {
                if let Err(e) = self.emit_flush(automaton) {
                    self.errors.push(e);
                }
            }
            other => {
                self.errors.push(CodegenError::NotYetImplemented {
                    what: stmt_kind_name(other),
                });
            }
        }
    }

    /// Slice 3: lower `#mutate Auto { field = expr, … };`.
    fn emit_mutate(
        &mut self,
        automaton: &str,
        assigns: &[FieldAssign],
    ) -> Result<(), CodegenError> {
        // Snapshot per-field locations so we can release the &-borrow
        // on `self.automatons` before calling `emit_expr` (which
        // needs `&mut self`). For each assign, capture
        // (location, ir_ty).
        let (is_register_block, struct_name, field_data) = {
            let info = self.automatons.get(automaton).ok_or_else(|| {
                CodegenError::UnresolvedName {
                    name: automaton.to_owned(),
                }
            })?;
            let struct_name = format!("%struct.{automaton}");
            let mut entries: Vec<(FieldLocation, String)> = Vec::with_capacity(assigns.len());
            for fa in assigns {
                let (idx, ir_ty, offset) = info
                    .fields
                    .iter()
                    .enumerate()
                    .find_map(|(i, (n, t, off))| {
                        if n == &fa.field {
                            Some((i, t.clone(), *off))
                        } else {
                            None
                        }
                    })
                    .ok_or_else(|| CodegenError::UnresolvedName {
                        name: format!("{automaton}.{}", fa.field),
                    })?;
                let loc = if info.is_register_block {
                    FieldLocation::RegisterBlock {
                        absolute_address: info.base_address + offset.unwrap_or(0),
                    }
                } else {
                    // Slice 9: shift user-field index by the state-
                    // tag slot for multi-state automatons.
                    FieldLocation::Struct {
                        idx: info.llvm_field_index(idx),
                    }
                };
                entries.push((loc, ir_ty));
            }
            (info.is_register_block, struct_name, entries)
        };

        for (fa, (loc, ir_ty)) in assigns.iter().zip(field_data.iter()) {
            if let Some(index_expr) = &fa.index {
                // Slice 5: indexed field assignment. Lower the
                // value first, then build the 2-level GEP (or
                // inttoptr-then-GEP for register-block) and store.
                let v = self.emit_expr(&fa.value)?;
                self.emit_indexed_field_store(
                    automaton,
                    loc,
                    ir_ty,
                    index_expr,
                    &v,
                    is_register_block,
                )?;
            } else {
                let v = self.emit_expr(&fa.value)?;
                self.emit_field_store(automaton, &struct_name, loc, ir_ty, &v, is_register_block);
            }
        }
        Ok(())
    }

    /// Slice 5: emit IR for `Auto.field[i] = value` inside a `#mutate`
    /// block. Mirrors [`Self::emit_index_expr`] but writes instead of
    /// reads, and walks the index expression up front (mutable borrow
    /// reuse).
    fn emit_indexed_field_store(
        &mut self,
        automaton: &str,
        loc: &FieldLocation,
        field_ir_ty: &str,
        index_expr: &Expr,
        value: &str,
        is_register_block: bool,
    ) -> Result<(), CodegenError> {
        let element_ir_ty = array_element_ir_type(field_ir_ty).ok_or(
            CodegenError::NotYetImplemented {
                what: "indexed field assignment on non-array field",
            },
        )?;
        let index_val = self.emit_expr(index_expr)?;
        let index_ir_ty = self.expr_ir_type(index_expr);

        // Stage 1: pointer to the array field itself.
        let field_ptr = match loc {
            FieldLocation::Struct { idx } => {
                // Slice 18 (Decision #12): writes to a `#staged`
                // automaton are redirected to its shadow global;
                // see [`AutomatonInfo::write_global`].
                let target_global = self
                    .automatons
                    .get(automaton)
                    .map(|info| info.write_global())
                    .unwrap_or_else(|| format!("@{automaton}.state"));
                let p = self.fresh_value();
                writeln!(
                    &mut self.out,
                    "  {p} = getelementptr %struct.{automaton}, %struct.{automaton}* {target_global}, i32 0, i32 {idx}",
                )
                .ok();
                p
            }
            FieldLocation::RegisterBlock { absolute_address } => {
                format!("inttoptr (i64 {absolute_address} to {field_ir_ty}*)")
            }
        };

        // Stage 2: pointer to the indexed element.
        let elem_ptr = self.fresh_value();
        writeln!(
            &mut self.out,
            "  {elem_ptr} = getelementptr {field_ir_ty}, {field_ir_ty}* {field_ptr}, i32 0, {index_ir_ty} {index_val}",
        )
        .ok();

        // Stage 3: store.
        let store_keyword = if is_register_block { "store volatile" } else { "store" };
        writeln!(
            &mut self.out,
            "  {store_keyword} {element_ir_ty} {value}, {element_ir_ty}* {elem_ptr}",
        )
        .ok();
        Ok(())
    }

    /// Slice 3+ helper: emit the IR for storing `value` into a single
    /// automaton field. Branches on `loc`:
    ///
    /// - `Struct { idx }` (non-register-block) — `getelementptr` +
    ///   `store`.
    /// - `RegisterBlock { absolute_address }` (slice 4) —
    ///   `store volatile <ir_ty> <value>, <ir_ty>* inttoptr (i64 …
    ///   to <ir_ty>*)`.
    fn emit_field_store(
        &mut self,
        automaton: &str,
        struct_name: &str,
        loc: &FieldLocation,
        ir_ty: &str,
        value: &str,
        is_register_block: bool,
    ) {
        match loc {
            FieldLocation::Struct { idx } => {
                // Slice 18 (Decision #12): writes to a `#staged`
                // automaton are redirected to its shadow global;
                // see [`AutomatonInfo::write_global`].
                let target_global = self
                    .automatons
                    .get(automaton)
                    .map(|info| info.write_global())
                    .unwrap_or_else(|| format!("@{automaton}.state"));
                let ptr = self.fresh_value();
                writeln!(
                    &mut self.out,
                    "  {ptr} = getelementptr {struct_name}, {struct_name}* {target_global}, i32 0, i32 {idx}",
                )
                .ok();
                writeln!(&mut self.out, "  store {ir_ty} {value}, {ir_ty}* {ptr}").ok();
            }
            FieldLocation::RegisterBlock { absolute_address } => {
                let _ = is_register_block; // tag for diagnostic-future use
                writeln!(
                    &mut self.out,
                    "  store volatile {ir_ty} {value}, {ir_ty}* inttoptr (i64 {abs} to {ir_ty}*)",
                    abs = absolute_address,
                )
                .ok();
            }
        }
    }

    /// Slice 3+ helper: emit the IR for loading the current value of
    /// a single automaton field. Mirrors [`Self::emit_field_store`]'s
    /// dispatch on `FieldLocation`. Returns the SSA value name
    /// holding the loaded value.
    fn emit_field_load(
        &mut self,
        automaton: &str,
        struct_name: &str,
        loc: &FieldLocation,
        ir_ty: &str,
    ) -> String {
        match loc {
            FieldLocation::Struct { idx } => {
                let ptr = self.fresh_value();
                writeln!(
                    &mut self.out,
                    "  {ptr} = getelementptr {struct_name}, {struct_name}* @{automaton}.state, i32 0, i32 {idx}",
                )
                .ok();
                let val = self.fresh_value();
                writeln!(&mut self.out, "  {val} = load {ir_ty}, {ir_ty}* {ptr}").ok();
                val
            }
            FieldLocation::RegisterBlock { absolute_address } => {
                let val = self.fresh_value();
                writeln!(
                    &mut self.out,
                    "  {val} = load volatile {ir_ty}, {ir_ty}* inttoptr (i64 {abs} to {ir_ty}*)",
                    abs = absolute_address,
                )
                .ok();
                val
            }
        }
    }

    /// Slice 3+: lower `Auto.field <op>= expr;` sugar.
    fn emit_mutate_short(
        &mut self,
        automaton: &str,
        field: &str,
        op: AssignOp,
        value: &Expr,
    ) -> Result<(), CodegenError> {
        let (struct_name, loc, ir_ty, is_register_block) = {
            let info = self.automatons.get(automaton).ok_or_else(|| {
                CodegenError::UnresolvedName {
                    name: automaton.to_owned(),
                }
            })?;
            let (idx, ir_ty, offset) = info
                .fields
                .iter()
                .enumerate()
                .find_map(|(i, (n, t, off))| {
                    if n == field {
                        Some((i, t.clone(), *off))
                    } else {
                        None
                    }
                })
                .ok_or_else(|| CodegenError::UnresolvedName {
                    name: format!("{automaton}.{field}"),
                })?;
            let loc = if info.is_register_block {
                FieldLocation::RegisterBlock {
                    absolute_address: info.base_address + offset.unwrap_or(0),
                }
            } else {
                // Slice 9: shift user-field index by the state-tag
                // slot for multi-state automatons (LLVM idx 0 holds
                // the i32 tag; user fields start at idx 1).
                FieldLocation::Struct {
                    idx: info.llvm_field_index(idx),
                }
            };
            (
                format!("%struct.{automaton}"),
                loc,
                ir_ty,
                info.is_register_block,
            )
        };

        let new_value = self.emit_expr(value)?;

        // For `=`, emit a plain store. For `<op>=`, load the current
        // value, apply the op, store the result.
        let final_value = if matches!(op, AssignOp::Eq) {
            new_value
        } else {
            let cur = self.emit_field_load(automaton, &struct_name, &loc, &ir_ty);
            let opcode = compound_assign_opcode(op, &ir_ty);
            let combined = self.fresh_value();
            writeln!(
                &mut self.out,
                "  {combined} = {opcode} {ir_ty} {cur}, {new_value}",
            )
            .ok();
            combined
        };
        self.emit_field_store(
            automaton,
            &struct_name,
            &loc,
            &ir_ty,
            &final_value,
            is_register_block,
        );
        Ok(())
    }

    /// Slice 3: lower `#> name(args);` — direct LLVM call to the
    /// named effect or transition. Transitions are namespaced as
    /// `<Owner>_<name>`. The resolver records whether the callee is
    /// an effect (top-level symbol) or a transition (per-automaton
    /// inner item); we consult `Resolution::lookup` on the proc-call
    /// statement's span to know which symbol shape to emit.
    fn emit_proc_call(
        &mut self,
        stmt: &Stmt,
        name: &str,
        args: &[Expr],
    ) -> Result<(), CodegenError> {
        // Lower args first.
        let mut arg_strs: Vec<String> = Vec::with_capacity(args.len());
        for a in args {
            let v = self.emit_expr(a)?;
            let ty = self.expr_ir_type(a);
            arg_strs.push(format!("{ty} {v}"));
        }

        // Decide the call symbol via two-step resolution:
        //
        // 1. Consult `Resolution::lookup` on the proc-call's statement
        //    span. The resolver tags every `#> name(args)` with a
        //    `BindingRef::Proc { ctx, … }` where `ctx` is one of:
        //    - `CallContext::Identity` — callee is a top-level
        //      `#effect` / `#interrupt`. Linker symbol = source name.
        //    - `CallContext::Transition` — callee is a `#transition`.
        //      Linker symbol = `<Owner>_<name>`.
        //    - `CallContext::Generic` — interface-method call (slice 5+).
        // 2. For a transition call, the owner is either:
        //    - The enclosing transition's owner (`enclosing_owner`)
        //      when one transition calls a sibling.
        //    - Otherwise: the registry's `transition_owners` map,
        //      which records the owner of every transition declared
        //      anywhere in the program. This handles the cross-
        //      callable case (an effect / interrupt dispatching a
        //      transition by name).
        let is_transition_call = self
            .resolution
            .lookup(stmt.span)
            .map(|b| {
                matches!(
                    b,
                    clifford_resolve::BindingRef::Proc {
                        ctx: clifford_resolve::CallContext::Transition,
                        ..
                    }
                )
            })
            .unwrap_or(false);
        let mangled = if is_transition_call {
            let owner = self
                .enclosing_owner
                .as_deref()
                .map(str::to_owned)
                .or_else(|| self.transition_owners.get(name).cloned())
                .unwrap_or_default();
            if owner.is_empty() {
                // No owner found — emit bare name as a defensive
                // fallback. Should not happen in well-formed programs.
                name.to_owned()
            } else {
                format!("{owner}_{name}")
            }
        } else {
            name.to_owned()
        };

        // Slice 3 emits the call as `void` return type — effects'
        // return types aren't yet threaded through to ProcCall sites
        // since the resolver / typing don't carry that info to the
        // statement span. A future slice will surface real return
        // types here.
        writeln!(
            &mut self.out,
            "  call void @{mangled}({args})",
            args = arg_strs.join(", ")
        )
        .ok();
        Ok(())
    }

    /// Emit IR for an expression, returning the IR value reference
    /// that holds its result (e.g. `"%tmp.5"`, `"42"`, `"true"`).
    fn emit_expr(&mut self, expr: &Expr) -> Result<String, CodegenError> {
        match &expr.kind {
            ExprKind::IntLit(s) => Ok(parse_int_literal(s)?.0),
            ExprKind::HexLit(s) => Ok(parse_hex_literal(s)?.0),
            ExprKind::BinLit(s) => Ok(parse_bin_literal(s)?.0),
            ExprKind::BoolLit(b) => Ok(if *b { "1".to_owned() } else { "0".to_owned() }),
            ExprKind::Path(segments) => {
                if segments.len() == 1 {
                    // Slice 12: dispatch on the binding's storage.
                    // Ssa: the local's value IS the SSA name — return
                    //      it directly (slice-1 path).
                    // Stack: the local's value is an alloca pointer —
                    //      emit a load through it.
                    let lookup = self.lookup_local_with_storage(&segments[0]);
                    match lookup {
                        Some((value, _ir_ty, LocalStorage::Ssa)) => Ok(value),
                        Some((ptr, ir_ty, LocalStorage::Stack)) => {
                            let val = self.fresh_value();
                            writeln!(
                                &mut self.out,
                                "  {val} = load {ir_ty}, {ir_ty}* {ptr}",
                            )
                            .ok();
                            Ok(val)
                        }
                        None => Err(CodegenError::UnresolvedName {
                            name: segments[0].clone(),
                        }),
                    }
                } else {
                    Err(CodegenError::NotYetImplemented {
                        what: "multi-segment path expression",
                    })
                }
            }
            ExprKind::Paren(inner) => self.emit_expr(inner),
            ExprKind::Unary { op, operand } => self.emit_unary(*op, operand),
            ExprKind::Binary { op, lhs, rhs } => self.emit_binary(*op, lhs, rhs),
            ExprKind::Call { callee, args } => self.emit_call(expr, callee, args),
            // Slice 3: automaton-field read.
            // `Counter.value` (`obj` is `Path([Counter])` where Counter
            // resolves to an `#automaton`) → getelementptr+load.
            // `Self.value` (`obj` is `Path([Self])` inside a
            // `#transition` body) resolves to the enclosing owner.
            ExprKind::FieldAccess { obj, field } => self.emit_field_access(obj, field),
            // v0.2-ζ: `@snapshot Auto.field` — Decision #24 / ADR
            // 0004 boundary-crossing read. Lowers to the same IR
            // as `Auto.field` (a single load); the safety
            // semantics — that this read is owned, immutable from
            // this point on, and acceptable to race with a single-
            // word write — is upstream of codegen. The verifier
            // (clifford-effect's read walker) excludes snapshots
            // from `actual_reads` so the v0.2-β read-write check
            // doesn't fire.
            //
            // v0.2-ζ MVP supports primitive fields only. Compound
            // (struct / array) snapshots would tear at the load
            // level; surface NotYetImplemented for those.
            ExprKind::Snapshot { automaton, field } => {
                self.emit_snapshot(automaton, field)
            }
            // Slice 5: indexed read on an automaton-array field.
            // `Counter.buf[3]` parses as
            // `Index { obj: FieldAccess(Path([Counter]), "buf"), index: 3 }`.
            // Lowers to a 2-level getelementptr (struct field → array
            // element) plus a `load` of the element type.
            ExprKind::Index { obj, index } => self.emit_index_expr(obj, index),
            // Slice 6: aggregate literals (tuples + arrays).
            // `(a, b, c)` and `[a, b, c]` lower to an `insertvalue`
            // chain on `undef` of the aggregate IR type. Tuples
            // become `{T1, T2, …}` structs; arrays become `[N x T]`.
            ExprKind::Tuple(elems) => self.emit_tuple_expr(expr, elems),
            ExprKind::Array(elems) => self.emit_array_expr(expr, elems),
            // Slice 6: array-repeat literals.
            // `[expr; count]` lowers to `count` insertvalue ops
            // chained on `undef`, all using the same evaluated
            // value. The count must be a const integer literal
            // today; non-const counts need a runtime memset / loop
            // and are deferred.
            ExprKind::ArrayRepeat { value, count } => {
                self.emit_array_repeat_expr(expr, value, count)
            }
            // Slice 7: integer casts (`expr as Type`).
            //
            // Source / dest IR types determine the LLVM opcode:
            //   - same type / same bit-width int → noop
            //   - dest narrower → `trunc`
            //   - dest wider, source signed   → `sext`
            //   - dest wider, source unsigned → `zext`
            //
            // Float casts and pointer-int casts are deferred —
            // the firmware tier doesn't need either yet.
            ExprKind::Cast { value, ty } => self.emit_cast_expr(value, ty),
            // Slice 9: `Auto@state` — read the current state tag.
            // Lowers to GEP+load of LLVM index 0 of the state
            // struct (multi-state automatons reserve index 0 for
            // the i32 tag). Monoid automatons reject this as
            // `NotYetImplemented` since they have no state tag.
            ExprKind::StateRead(name) => self.emit_state_read(name),
            // Slice 8: Decision #17 / #19 unsafe primitives
            // (expression forms). Each lowers to a single LLVM
            // op against a raw pointer value, with no implicit
            // checks — the caller is responsible for safety per
            // the spec's audit-log obligation.
            ExprKind::UncheckedLoad { ty, ptr } => self.emit_unchecked_load(ty, ptr, false),
            ExprKind::VolatileLoad { ty, ptr } => self.emit_unchecked_load(ty, ptr, true),
            ExprKind::UncheckedCast {
                from_ty,
                to_ty,
                value,
                ..
            } => self.emit_unchecked_cast(from_ty, to_ty, value),
            ExprKind::UncheckedOffset { ty, ptr, n } => self.emit_unchecked_offset(ty, ptr, n),
            other => Err(CodegenError::NotYetImplemented {
                what: expr_kind_name(other),
            }),
        }
    }

    /// Slice 5: lower `obj[index]` for an automaton-array field.
    ///
    /// Today this handler supports only the canonical firmware shape
    /// where `obj` is a `FieldAccess` on an automaton field whose
    /// type is `[T; N]`. Indexing on local arrays / slices / tuples
    /// requires the alloca-based borrow machinery (separate slice).
    ///
    /// The emitted IR is:
    ///
    /// ```text
    /// %field = getelementptr %struct.<Auto>, %struct.<Auto>* @<Auto>.state, i32 0, i32 <field_idx>
    /// %elem  = getelementptr [N x T], [N x T]* %field, i32 0, i32 <index>
    /// %val   = load T, T* %elem
    /// ```
    ///
    /// For register-block array fields, the first GEP is replaced by
    /// `inttoptr (i64 <abs> to [N x T]*)` so the GEP-into-array
    /// shape still applies, and the load is volatile.
    fn emit_index_expr(&mut self, obj: &Expr, index: &Expr) -> Result<String, CodegenError> {
        // Obj must be `FieldAccess { Path([Auto] | [Self]), field }`.
        let (auto_name, field) = match &obj.kind {
            ExprKind::FieldAccess { obj: inner_obj, field } => {
                let auto_name = match &inner_obj.kind {
                    ExprKind::Path(segs) if segs.len() == 1 => {
                        if segs[0] == "Self" {
                            self.enclosing_owner.clone().ok_or(
                                CodegenError::NotYetImplemented {
                                    what: "Self.field[i] outside a #transition body",
                                },
                            )?
                        } else {
                            segs[0].clone()
                        }
                    }
                    _ => {
                        return Err(CodegenError::NotYetImplemented {
                            what: "Index where receiver isn't Auto.field / Self.field",
                        });
                    }
                };
                (auto_name, field.clone())
            }
            _ => {
                return Err(CodegenError::NotYetImplemented {
                    what: "Index where receiver isn't a field access (slice 5 supports Auto.field[i] only)",
                });
            }
        };

        // Resolve the field's location and IR type, then split the
        // array IR type into `[N x T]` and `T`.
        let (field_ir_ty, field_loc, is_register_block) = {
            let info = self.automatons.get(&auto_name).ok_or_else(|| {
                CodegenError::UnresolvedName { name: auto_name.clone() }
            })?;
            let (idx, ir_ty, offset) = info
                .fields
                .iter()
                .enumerate()
                .find_map(|(i, (n, t, off))| {
                    if n == &field {
                        Some((i, t.clone(), *off))
                    } else {
                        None
                    }
                })
                .ok_or_else(|| CodegenError::UnresolvedName {
                    name: format!("{auto_name}.{field}"),
                })?;
            let loc = if info.is_register_block {
                FieldLocation::RegisterBlock {
                    absolute_address: info.base_address + offset.unwrap_or(0),
                }
            } else {
                // Slice 9: shift user-field index by the state-tag
                // slot for multi-state automatons (LLVM idx 0 holds
                // the i32 tag; user fields start at idx 1).
                FieldLocation::Struct {
                    idx: info.llvm_field_index(idx),
                }
            };
            (ir_ty, loc, info.is_register_block)
        };

        let element_ir_ty = array_element_ir_type(&field_ir_ty)
            .ok_or(CodegenError::NotYetImplemented {
                what: "indexed read on non-array field",
            })?;

        // Lower the index expression.
        let index_val = self.emit_expr(index)?;
        let index_ir_ty = self.expr_ir_type(index);

        // Stage 1: pointer to the array field.
        let field_ptr = match field_loc {
            FieldLocation::Struct { idx } => {
                let p = self.fresh_value();
                writeln!(
                    &mut self.out,
                    "  {p} = getelementptr %struct.{auto_name}, %struct.{auto_name}* @{auto_name}.state, i32 0, i32 {idx}",
                )
                .ok();
                p
            }
            FieldLocation::RegisterBlock { absolute_address } => {
                // Skip the GEP for register-block fields — the
                // `inttoptr` literal IS the field pointer.
                format!(
                    "inttoptr (i64 {absolute_address} to {field_ir_ty}*)"
                )
            }
        };

        // Stage 2: pointer to the indexed element. The array-GEP's
        // first index is `0` (deref the pointer); second index is the
        // element index (typed to match the supplied index).
        let elem_ptr = self.fresh_value();
        writeln!(
            &mut self.out,
            "  {elem_ptr} = getelementptr {field_ir_ty}, {field_ir_ty}* {field_ptr}, i32 0, {index_ir_ty} {index_val}",
        )
        .ok();

        // Stage 3: load.
        let val = self.fresh_value();
        let load_keyword = if is_register_block { "load volatile" } else { "load" };
        writeln!(
            &mut self.out,
            "  {val} = {load_keyword} {element_ir_ty}, {element_ir_ty}* {elem_ptr}",
        )
        .ok();
        Ok(val)
    }

    /// Slice 6: lower a tuple literal `(a, b, …)` to an `insertvalue`
    /// chain on `undef` of the tuple's struct IR type.
    ///
    /// ```text
    /// %t0 = insertvalue {T1, T2, T3} undef, T1 <a>, 0
    /// %t1 = insertvalue {T1, T2, T3} %t0, T2 <b>, 1
    /// %t2 = insertvalue {T1, T2, T3} %t1, T3 <c>, 2
    /// ```
    ///
    /// The aggregate IR type comes from `expr_ir_type` (i.e. from the
    /// typing record when present, else a syntactic fallback).
    fn emit_tuple_expr(
        &mut self,
        expr: &Expr,
        elems: &[Expr],
    ) -> Result<String, CodegenError> {
        let agg_ty = self.expr_ir_type(expr);
        self.emit_aggregate_insertvalue_chain(&agg_ty, elems)
    }

    /// Slice 6: lower an array literal `[a, b, …]` to an `insertvalue`
    /// chain on `undef` of the array's `[N x T]` IR type. Same shape
    /// as [`Self::emit_tuple_expr`] — the LLVM `insertvalue`
    /// instruction is uniform across struct and array aggregates.
    fn emit_array_expr(
        &mut self,
        expr: &Expr,
        elems: &[Expr],
    ) -> Result<String, CodegenError> {
        if elems.is_empty() {
            // `[]` of zero length isn't useful in v0.1; the type
            // checker rarely produces a usable element type for it.
            return Err(CodegenError::NotYetImplemented {
                what: "empty array literal `[]`",
            });
        }
        let agg_ty = self.expr_ir_type(expr);
        self.emit_aggregate_insertvalue_chain(&agg_ty, elems)
    }

    /// Slice 6: lower `[value; count]` array-repeat literal. The
    /// count must be a const integer literal today; non-const counts
    /// (computed sizes, generic-bound counts, etc.) are deferred to a
    /// later slice that needs a runtime loop.
    ///
    /// The value expression is emitted **once** and then re-used in
    /// every `insertvalue`; this preserves the semantics of "the same
    /// value at every index" without re-evaluating side-effecting
    /// expressions (which the type checker should reject for
    /// `[expr; N]` anyway, but defensive emission is cheap).
    fn emit_array_repeat_expr(
        &mut self,
        expr: &Expr,
        value: &Expr,
        count: &Expr,
    ) -> Result<String, CodegenError> {
        let n = const_int_count(count).ok_or(CodegenError::NotYetImplemented {
            what: "array-repeat count that isn't a const integer literal",
        })?;
        let agg_ty = self.expr_ir_type(expr);
        let elem_value = self.emit_expr(value)?;
        let elem_ir_ty = self.expr_ir_type(value);

        let mut current: String = "undef".to_owned();
        for i in 0..n {
            let next = self.fresh_value();
            writeln!(
                &mut self.out,
                "  {next} = insertvalue {agg_ty} {current}, {elem_ir_ty} {elem_value}, {i}",
            )
            .ok();
            current = next;
        }
        Ok(current)
    }

    /// Slice 6 shared core: walk `elems`, emit each element, and
    /// thread the SSA values into a chain of `insertvalue` ops on
    /// `undef` of `agg_ty`. Returns the final aggregate SSA name.
    ///
    /// Empty `elems` returns `"undef"` directly so callers don't
    /// emit a useless `insertvalue` of nothing; the public-facing
    /// callers (tuple / array) reject empty inputs upstream so this
    /// branch isn't currently exercisable from the language surface.
    fn emit_aggregate_insertvalue_chain(
        &mut self,
        agg_ty: &str,
        elems: &[Expr],
    ) -> Result<String, CodegenError> {
        if elems.is_empty() {
            return Ok("undef".to_owned());
        }
        // Emit each element first; capture (ir_type, ssa_value) pairs
        // before we start writing the insertvalue chain. This avoids
        // interleaving element-evaluation IR with the chain ops, which
        // keeps the output readable.
        let mut emitted: Vec<(String, String)> = Vec::with_capacity(elems.len());
        for e in elems {
            let v = self.emit_expr(e)?;
            let t = self.expr_ir_type(e);
            emitted.push((t, v));
        }

        let mut current: String = "undef".to_owned();
        for (i, (ir_ty, value)) in emitted.iter().enumerate() {
            let next = self.fresh_value();
            writeln!(
                &mut self.out,
                "  {next} = insertvalue {agg_ty} {current}, {ir_ty} {value}, {i}",
            )
            .ok();
            current = next;
        }
        Ok(current)
    }

    /// Slice 7: lower `value as Type` for integer-to-integer casts.
    ///
    /// Today this handler accepts:
    ///   - identical IR types (no-op, return the source value)
    ///   - integer source + integer dest of different bit widths:
    ///     `trunc` for narrowing, `sext`/`zext` for widening
    ///     (sign-extend if the source's primitive type is signed)
    ///
    /// Float casts (`fptrunc` / `fpext` / `fptoui` / `sitofp` etc.)
    /// and pointer ↔ int casts are out of scope for v0.1 firmware
    /// and surface as `NotYetImplemented`.
    fn emit_cast_expr(
        &mut self,
        value: &Expr,
        ty: &TypeExpr,
    ) -> Result<String, CodegenError> {
        let src_ir_ty = self.expr_ir_type(value);
        let dst_ir_ty = self.lower_type(ty)?;
        let src_value = self.emit_expr(value)?;

        // Same IR type → no instruction needed; just thread the
        // value through. This handles `5u32 as u32` and other
        // syntactically-redundant casts the user may have written
        // for documentation purposes.
        if src_ir_ty == dst_ir_ty {
            return Ok(src_value);
        }

        // Integer-to-integer cast — pick the opcode by bit width.
        let src_bits = int_bits(&src_ir_ty);
        let dst_bits = int_bits(&dst_ir_ty);
        if let (Some(s), Some(d)) = (src_bits, dst_bits) {
            let opcode = if d < s {
                "trunc"
            } else if self.expr_is_signed_int(value) {
                "sext"
            } else {
                "zext"
            };
            let result = self.fresh_value();
            writeln!(
                &mut self.out,
                "  {result} = {opcode} {src_ir_ty} {src_value} to {dst_ir_ty}",
            )
            .ok();
            return Ok(result);
        }

        // Non-integer cast (float ↔ int, ptr ↔ int, struct casts) —
        // surface as NotYetImplemented so the user gets a useful
        // error and we know to extend this when the firmware tier
        // grows a use case.
        Err(CodegenError::NotYetImplemented {
            what: "non-integer cast (float / pointer / aggregate)",
        })
    }

    /// Slice 8: lower `#unchecked_load<T>(ptr)` (and the volatile
    /// sibling). Both lower to a single LLVM `load` against the raw
    /// pointer SSA value. The pointer expression's IR type is
    /// expected to be `T*`; we emit the load with element type `T`.
    ///
    /// ```text
    /// %v = load <T>, <T>* <ptr>          ; #unchecked_load
    /// %v = load volatile <T>, <T>* <ptr> ; #volatile_load
    /// ```
    /// Slice 21 (Decision #18): emit a `; audit-wrap site for
    /// <Owner>` IR comment iff [`Self::current_audited_owner`] is
    /// `Some`. Called from every unsafe-primitive emission site
    /// (`#unchecked_load` / `#unchecked_store` /
    /// `#unchecked_offset` / `#unchecked_cast` /
    /// `#volatile_load` / `#volatile_store`) to give downstream
    /// instrumentation passes a stable place to inject
    /// `PointerAuditor` calls. The `kind` argument is the
    /// human-readable primitive name (`"unchecked_load"`, etc.)
    /// included in the comment so a single grep across the IR
    /// surfaces every wrap site categorised by primitive.
    ///
    /// No-op when `current_audited_owner` is `None` — non-audited
    /// transitions, effects, interrupts, and `@fn`s emit
    /// byte-identical IR to slice-20 output.
    fn emit_audit_marker_if_needed(&mut self, kind: &str) {
        if let Some(owner) = &self.current_audited_owner {
            writeln!(
                &mut self.out,
                "  ; audit-wrap site for {owner} ({kind}) ; Decision #18",
            )
            .ok();
        }
    }

    fn emit_unchecked_load(
        &mut self,
        ty: &TypeExpr,
        ptr: &Expr,
        is_volatile: bool,
    ) -> Result<String, CodegenError> {
        let elem_ir_ty = self.lower_type(ty)?;
        let ptr_value = self.emit_expr(ptr)?;
        let result = self.fresh_value();
        let keyword = if is_volatile { "load volatile" } else { "load" };
        self.emit_audit_marker_if_needed(if is_volatile {
            "volatile_load"
        } else {
            "unchecked_load"
        });
        writeln!(
            &mut self.out,
            "  {result} = {keyword} {elem_ir_ty}, {elem_ir_ty}* {ptr_value}",
        )
        .ok();
        Ok(result)
    }

    /// Slice 8: lower `#unchecked_store<T>(ptr, value)` (and the
    /// volatile sibling). Statement form — emits a single LLVM
    /// `store`, no result. The element type comes from the
    /// `TypeExpr` carried on the AST node, not from the value's
    /// inferred type, so the user can be explicit about the storage
    /// width independent of any implicit promotions.
    fn emit_unchecked_store(
        &mut self,
        ty: &TypeExpr,
        ptr: &Expr,
        value: &Expr,
        is_volatile: bool,
    ) -> Result<(), CodegenError> {
        let elem_ir_ty = self.lower_type(ty)?;
        let ptr_value = self.emit_expr(ptr)?;
        let val_ssa = self.emit_expr(value)?;
        let keyword = if is_volatile { "store volatile" } else { "store" };
        self.emit_audit_marker_if_needed(if is_volatile {
            "volatile_store"
        } else {
            "unchecked_store"
        });
        writeln!(
            &mut self.out,
            "  {keyword} {elem_ir_ty} {val_ssa}, {elem_ir_ty}* {ptr_value}",
        )
        .ok();
        Ok(())
    }

    /// Slice 8: lower `#unchecked_cast<S, T>("reason", value)`.
    ///
    /// Picks the LLVM opcode by the source / dest IR-type shapes:
    ///   - same IR type           → no-op (return the value as-is)
    ///   - both integer types     → reuse the slice-7 trunc/sext/zext
    ///                              dispatch
    ///   - source pointer + dest int  → `ptrtoint`
    ///   - source int + dest pointer  → `inttoptr`
    ///   - same bit-width otherwise   → `bitcast`
    ///
    /// The mandatory reason string is preserved in the AST and is
    /// already accessible via `cliffordc audit --list-unsafe`; we
    /// don't embed it in the IR (it would be a comment, which LLVM
    /// would discard during parsing).
    fn emit_unchecked_cast(
        &mut self,
        from_ty: &TypeExpr,
        to_ty: &TypeExpr,
        value: &Expr,
    ) -> Result<String, CodegenError> {
        let src_ir_ty = self.lower_type(from_ty)?;
        let dst_ir_ty = self.lower_type(to_ty)?;
        let src_value = self.emit_expr(value)?;

        if src_ir_ty == dst_ir_ty {
            // Same-IR-type cast is a no-op even semantically — the
            // `value` SSA name is returned unchanged. No instruction
            // is emitted, so we don't emit an audit marker either:
            // there's nothing for an instrumentation pass to wrap.
            return Ok(src_value);
        }
        self.emit_audit_marker_if_needed("unchecked_cast");

        // Pure integer ↔ integer: reuse the slice-7 logic. Source
        // signedness comes from the user-written `from_ty` since the
        // cast is explicit at this level.
        let src_bits = int_bits(&src_ir_ty);
        let dst_bits = int_bits(&dst_ir_ty);
        if let (Some(s), Some(d)) = (src_bits, dst_bits) {
            let opcode = if d < s {
                "trunc"
            } else if type_expr_is_signed_int(from_ty) {
                "sext"
            } else {
                "zext"
            };
            let result = self.fresh_value();
            writeln!(
                &mut self.out,
                "  {result} = {opcode} {src_ir_ty} {src_value} to {dst_ir_ty}",
            )
            .ok();
            return Ok(result);
        }

        // Pointer ↔ integer dispatches.
        let src_is_ptr = src_ir_ty.ends_with('*');
        let dst_is_ptr = dst_ir_ty.ends_with('*');
        let opcode = match (src_is_ptr, dst_is_ptr) {
            (true, false) if dst_bits.is_some() => "ptrtoint",
            (false, true) if src_bits.is_some() => "inttoptr",
            // Same-shape (both pointers, both aggregates, etc.) —
            // fall back to bitcast. LLVM accepts bitcast between
            // any two same-bit-width values.
            _ => "bitcast",
        };
        let result = self.fresh_value();
        writeln!(
            &mut self.out,
            "  {result} = {opcode} {src_ir_ty} {src_value} to {dst_ir_ty}",
        )
        .ok();
        Ok(result)
    }

    /// Slice 8: lower `#unchecked_offset<T>(ptr, n)` per Decision
    /// #19. Emits a single `getelementptr` against the raw pointer
    /// with element type `T`. The signed `n` is the element-count
    /// offset; LLVM's GEP semantics treat it as a signed integer
    /// extending to pointer width.
    ///
    /// ```text
    /// %p2 = getelementptr <T>, <T>* <ptr>, <n_ir_ty> <n>
    /// ```
    fn emit_unchecked_offset(
        &mut self,
        ty: &TypeExpr,
        ptr: &Expr,
        n: &Expr,
    ) -> Result<String, CodegenError> {
        let elem_ir_ty = self.lower_type(ty)?;
        let ptr_value = self.emit_expr(ptr)?;
        let n_ir_ty = self.expr_ir_type(n);
        let n_value = self.emit_expr(n)?;
        let result = self.fresh_value();
        self.emit_audit_marker_if_needed("unchecked_offset");
        writeln!(
            &mut self.out,
            "  {result} = getelementptr {elem_ir_ty}, {elem_ir_ty}* {ptr_value}, {n_ir_ty} {n_value}",
        )
        .ok();
        Ok(result)
    }

    /// Slice 11: lower `sigma var in lo..hi { body }` to a counted-
    /// loop CFG per spec §8.4 / Decision #14. The emitted shape:
    ///
    /// ```text
    ///   ; predecessor block (whatever the current_block was)
    ///   <emit lo and hi>
    ///   br label %sigma.header.<id>
    /// sigma.header.<id>:
    ///   %sigma.i.<id> = phi <ty> [ %lo, %<pred> ], [ %sigma.i_next.<id>, %sigma.continue.<id> ]
    ///   %sigma.cond.<id> = icmp <op> <ty> %sigma.i.<id>, %hi
    ///   br i1 %sigma.cond.<id>, label %sigma.body.<id>, label %sigma.exit.<id>
    /// sigma.body.<id>:
    ///   ; <body, with var bound to %sigma.i.<id>>
    ///   br label %sigma.continue.<id>            ; natural fall-through (suppressed if body terminated)
    /// sigma.continue.<id>:                        ; entry point for `continue;` statements (slice 17)
    ///   %sigma.i_next.<id> = add nuw <ty> %sigma.i.<id>, 1
    ///   br label %sigma.header.<id>
    /// sigma.exit.<id>:
    ///   ; (subsequent statements emit here)
    /// ```
    ///
    /// The compare opcode picks `ult`/`ule` (unsigned) or
    /// `slt`/`sle` (signed) based on the range bound's source
    /// type. Inclusive `..=` uses `<op>e`; half-open `..` uses
    /// `<op>` (strict).
    ///
    /// v0.1 scope: range sources only (`lo..hi`, `lo..=hi`).
    /// Slice 13: lower `if cond { … } else { … }` (statement form)
    /// to a conditional-branch CFG.
    ///
    /// Emitted shape (with else):
    /// ```text
    ///   <cond emitted in current block>
    ///   br i1 %cond, label %if.then.<id>, label %if.else.<id>
    /// if.then.<id>:
    ///   <then body>
    ///   br label %if.exit.<id>
    /// if.else.<id>:
    ///   <else body>
    ///   br label %if.exit.<id>
    /// if.exit.<id>:
    ///   <subsequent statements emit here>
    /// ```
    ///
    /// Without else: the false-edge of the `br i1` jumps directly
    /// to `if.exit.<id>`; no `if.else.<id>` block is emitted.
    ///
    /// If a branch terminates (e.g. `return` mid-body), its
    /// `br label %if.exit.<id>` is suppressed so we don't try to
    /// add a second terminator. If BOTH branches terminate, the
    /// merge block is unreachable; we still emit it (LLVM tolerates
    /// blocks with no predecessors and DCE removes them).
    fn emit_if(
        &mut self,
        cond: &Expr,
        then_block: &Block,
        else_block: Option<&Block>,
    ) -> Result<(), CodegenError> {
        // Lower the condition in the current block. The condition's
        // IR type must be `i1` (bool); we don't insert a coercion —
        // upstream typing should reject non-bool conditions in a
        // future slice.
        let cond_val = self.emit_expr(cond)?;

        // Allocate fresh label IDs for this `if`.
        let id = self.next_label_id;
        self.next_label_id += 1;
        let then_label = format!("if.then.{id}");
        let exit_label = format!("if.exit.{id}");
        let else_label = if else_block.is_some() {
            Some(format!("if.else.{id}"))
        } else {
            None
        };

        // Emit the conditional branch from the current block.
        let false_target = else_label.as_deref().unwrap_or(&exit_label);
        writeln!(
            &mut self.out,
            "  br i1 {cond_val}, label %{then_label}, label %{false_target}",
        )
        .ok();

        // Then block.
        writeln!(&mut self.out, "{then_label}:").ok();
        self.current_block = then_label.clone();
        self.current_block_terminated = false;
        let then_scope = self.locals.len();
        // Slice 17: drive termination off `current_block_terminated`
        // so any terminator (return / break / continue / unreachable)
        // stops the body cleanly. Pre-slice-17 only checked
        // `Return(_)`, which let `break;` followed by dead code
        // double-terminate the basic block.
        for s in &then_block.stmts {
            if self.current_block_terminated {
                break;
            }
            self.emit_stmt(s);
        }
        self.locals.truncate(then_scope);
        // Branch to exit if the then-block didn't already terminate
        // (e.g. via `return`/`break`/`continue`). A nested `if`/`sigma`
        // may have moved current_block to its own merge/exit; we
        // still emit the jump from THERE to our exit, which is correct.
        if !self.current_block_terminated {
            writeln!(&mut self.out, "  br label %{exit_label}").ok();
        }

        // Else block (if present).
        if let Some(else_blk) = else_block {
            let else_lbl = else_label.as_ref().expect("else_label set above");
            writeln!(&mut self.out, "{else_lbl}:").ok();
            self.current_block = else_lbl.clone();
            self.current_block_terminated = false;
            let else_scope = self.locals.len();
            for s in &else_blk.stmts {
                if self.current_block_terminated {
                    break;
                }
                self.emit_stmt(s);
            }
            self.locals.truncate(else_scope);
            if !self.current_block_terminated {
                writeln!(&mut self.out, "  br label %{exit_label}").ok();
            }
        }

        // Exit block — subsequent statements emit here. May be
        // unreachable if both branches terminate; LLVM tolerates
        // that and DCE collapses it.
        writeln!(&mut self.out, "{exit_label}:").ok();
        self.current_block = exit_label;
        self.current_block_terminated = false;

        Ok(())
    }

    /// Slice 12: lower `name = expr;` to a single LLVM `store`
    /// against the local's alloca pointer. The resolver has already
    /// verified that `name` resolves to a `let mut` binding (which
    /// always has [`LocalStorage::Stack`]); if for some reason the
    /// binding is SSA-direct or missing, we surface a structured
    /// error rather than silently emitting wrong IR.
    ///
    /// ```text
    ///   <emit value> -> %v
    ///   store <ir_ty> %v, <ir_ty>* %ptr
    /// ```
    fn emit_local_assign(
        &mut self,
        name: &str,
        value: &Expr,
    ) -> Result<(), CodegenError> {
        let lookup = self.lookup_local_with_storage(name);
        let (ptr, ir_ty) = match lookup {
            Some((ptr, ir_ty, LocalStorage::Stack)) => (ptr, ir_ty),
            Some((_, _, LocalStorage::Ssa)) => {
                // Defensive: if codegen sees Assign on an SSA-direct
                // binding, the resolver should have rejected it
                // upstream. Surface a structured error so the user
                // sees a meaningful message even if upstream gates
                // are bypassed.
                return Err(CodegenError::NotYetImplemented {
                    what: "Assign on an immutable local (resolver should have caught this; please file a bug)",
                });
            }
            None => {
                return Err(CodegenError::UnresolvedName {
                    name: name.to_owned(),
                });
            }
        };
        let v = self.emit_expr(value)?;
        writeln!(
            &mut self.out,
            "  store {ir_ty} {v}, {ir_ty}* {ptr}",
        )
        .ok();
        Ok(())
    }

    /// Array sources (`sigma x in &arr`) need the slice-indexing
    /// infrastructure and land in a future slice.
    fn emit_sigma(
        &mut self,
        var: &str,
        source: &Expr,
        body: &Block,
    ) -> Result<(), CodegenError> {
        // Extract the range bounds. v0.1 supports range sources
        // only; array sources surface a structured error.
        let (lo, hi, inclusive) = match &source.kind {
            ExprKind::Range { lo, hi, inclusive } => (lo.as_ref(), hi.as_ref(), *inclusive),
            _ => {
                return Err(CodegenError::NotYetImplemented {
                    what: "sigma over a non-range source (array source needs the slice-indexing slice)",
                });
            }
        };

        // Iteration type + signedness from the lower bound.
        // §5.8 says `lo` and `hi` must be the same integer type;
        // upstream typing already enforces this (BinaryTypeMismatch
        // for `..` / `..=`), so we trust `lo` here.
        let ir_ty = self.expr_ir_type(lo);
        let signed = self.expr_is_signed_int(lo);

        // Emit lo / hi once in the predecessor block (so they aren't
        // recomputed every iteration). Capture the predecessor's
        // basic-block name BEFORE branching so the phi node's
        // incoming-edge labels are correct.
        let lo_val = self.emit_expr(lo)?;
        let hi_val = self.emit_expr(hi)?;
        let pred_block = self.current_block.clone();

        // Allocate fresh label IDs for this loop. SSA-name conflicts
        // across nested sigmas are avoided because each loop has its
        // own `<id>` suffix.
        //
        // Slice 17 added the separate `sigma.continue.<id>` block:
        // the body's natural fall-through and any `continue;`
        // statement both branch to it, and the increment + back-
        // edge live there. This keeps the body block free of the
        // back-edge (so `continue;` doesn't have to duplicate the
        // increment) and gives `break;` a clean exit-label target.
        let id = self.next_label_id;
        self.next_label_id += 1;
        let header_label = format!("sigma.header.{id}");
        let body_label = format!("sigma.body.{id}");
        let continue_label = format!("sigma.continue.{id}");
        let exit_label = format!("sigma.exit.{id}");
        let i_name = format!("%sigma.i.{id}");
        let i_next_name = format!("%sigma.i_next.{id}");
        let cond_name = format!("%sigma.cond.{id}");

        // Branch from the predecessor into the header.
        writeln!(&mut self.out, "  br label %{header_label}").ok();

        // Header block: phi + condition + conditional branch.
        // Phi's body-incoming edge is from `continue`, NOT `body`,
        // because that's where the increment happens.
        writeln!(&mut self.out, "{header_label}:").ok();
        self.current_block = header_label.clone();
        self.current_block_terminated = false;
        writeln!(
            &mut self.out,
            "  {i_name} = phi {ir_ty} [ {lo_val}, %{pred_block} ], [ {i_next_name}, %{continue_label} ]",
        )
        .ok();
        let cmp_op = match (signed, inclusive) {
            (false, false) => "ult",
            (false, true) => "ule",
            (true, false) => "slt",
            (true, true) => "sle",
        };
        writeln!(
            &mut self.out,
            "  {cond_name} = icmp {cmp_op} {ir_ty} {i_name}, {hi_val}",
        )
        .ok();
        writeln!(
            &mut self.out,
            "  br i1 {cond_name}, label %{body_label}, label %{exit_label}",
        )
        .ok();

        // Body block: bind the loop variable, emit body statements.
        // The natural fall-through branches to `continue` (which
        // performs the increment + back-edge); `break;` /
        // `continue;` statements branch directly to exit /
        // continue respectively.
        writeln!(&mut self.out, "{body_label}:").ok();
        self.current_block = body_label.clone();
        self.current_block_terminated = false;

        // Push the loop variable as a local for the body's scope.
        // We snapshot the locals length so any lets inside the body
        // are also dropped when the loop scope closes — matching
        // Clifford's lexical scoping (the var isn't visible after
        // the loop).
        let scope_marker = self.locals.len();
        self.locals.push(LocalBinding {
            name: var.to_owned(),
            value: i_name.clone(),
            ir_type: ir_ty.clone(),
            // sigma loop variables are always immutable (the loop
            // controls the iteration; the body cannot rebind it).
            storage: LocalStorage::Ssa,
        });

        // Slice 17: push this loop's (continue, exit) labels onto
        // the stack so nested `break;` / `continue;` statements
        // resolve to the innermost loop. Pop after the body.
        self.sigma_loop_stack
            .push((continue_label.clone(), exit_label.clone()));

        // Emit body statements. We don't reuse `emit_block` here
        // because we need to inject the back-edge ourselves and
        // we need to detect whether the body terminated (a `return`,
        // `break;`, or `continue;` mid-body terminates `sigma.body.<id>`
        // and the fall-through-to-continue branch would be invalid).
        // Slice 17 uses `current_block_terminated` directly so that
        // any terminating statement (return / break / continue /
        // unreachable) stops the body emission cleanly.
        for stmt in &body.stmts {
            if self.current_block_terminated {
                break; // dead code after a terminator
            }
            self.emit_stmt(stmt);
        }

        // Pop loop-scope locals.
        self.locals.truncate(scope_marker);
        // Pop the loop's (continue, exit) labels — outer loops
        // (if any) are restored as the new innermost.
        self.sigma_loop_stack.pop();

        // Body fall-through: branch to the continue block (which
        // handles the increment + back-edge). Suppressed if the
        // body's current block was terminated by `return`,
        // `break;`, `continue;`, or an inner `unreachable`. For
        // nested sigmas, current_block now points at the inner
        // loop's exit block — that's still open, so the
        // fall-through branch correctly closes it.
        if !self.current_block_terminated {
            writeln!(&mut self.out, "  br label %{continue_label}").ok();
        }

        // Continue block: increment + back-edge to header. This is
        // where `continue;` statements branch to as well.
        writeln!(&mut self.out, "{continue_label}:").ok();
        self.current_block = continue_label.clone();
        self.current_block_terminated = false;
        let one_op = if signed { "add nsw" } else { "add nuw" };
        writeln!(
            &mut self.out,
            "  {i_next_name} = {one_op} {ir_ty} {i_name}, 1",
        )
        .ok();
        writeln!(&mut self.out, "  br label %{header_label}").ok();

        // Slice 17 design note: pre-slice-17 had the back-edge
        // in the body block, with a `synthetic i_next = add 0`
        // workaround when a body terminated via `return`. The
        // continue-block restructure makes that workaround
        // unnecessary: the increment + back-edge always
        // reachable via the continue label (which the body's
        // fall-through, an explicit `continue;`, or both jump
        // to). A body that always `return`s leaves the continue
        // block with no predecessors — LLVM's DCE collapses it
        // along with the rest of the loop.

        // Exit block: subsequent statements emit here.
        writeln!(&mut self.out, "{exit_label}:").ok();
        self.current_block = exit_label;
        self.current_block_terminated = false;

        Ok(())
    }

    /// Slice 9: lower `Auto@state` to a load of the state-tag
    /// field. Multi-state automatons reserve LLVM struct index 0
    /// for an `i32` tag; this emits the canonical
    /// `getelementptr` + `load i32` pair. Monoid automatons (no
    /// `#states` clause) and register-block automatons surface
    /// `NotYetImplemented` since neither has a tag field.
    ///
    /// ```text
    /// %tag_ptr = getelementptr %struct.<Auto>, %struct.<Auto>* @<Auto>.state, i32 0, i32 0
    /// %tag     = load i32, i32* %tag_ptr
    /// ```
    fn emit_state_read(&mut self, automaton: &str) -> Result<String, CodegenError> {
        let info = self.automatons.get(automaton).ok_or_else(|| {
            CodegenError::UnresolvedName {
                name: automaton.to_owned(),
            }
        })?;
        if info.is_register_block {
            // A register-block multi-state combo would need a
            // user-specified MMIO offset for the tag; the spec
            // doesn't define this yet. Defer the question to a
            // future slice if a real use-case appears.
            return Err(CodegenError::NotYetImplemented {
                what: "Auto@state on a register-block automaton",
            });
        }
        if !info.is_multi_state() {
            // Monoid automaton — no state tag exists. The resolver
            // accepts `Auto@state` for any automaton; codegen is
            // the layer that knows there's nothing to read.
            return Err(CodegenError::NotYetImplemented {
                what: "Auto@state on a monoid automaton (no `#states` clause)",
            });
        }
        let tag_ptr = self.fresh_value();
        writeln!(
            &mut self.out,
            "  {tag_ptr} = getelementptr %struct.{automaton}, %struct.{automaton}* @{automaton}.state, i32 0, i32 0",
        )
        .ok();
        let tag_val = self.fresh_value();
        writeln!(
            &mut self.out,
            "  {tag_val} = load i32, i32* {tag_ptr}",
        )
        .ok();
        Ok(tag_val)
    }

    /// Slice 3: lower an automaton field read. The emitted IR is:
    ///
    /// ```text
    /// %ptr = getelementptr %struct.<Auto>, %struct.<Auto>* @<Auto>.state, i32 0, i32 <idx>
    /// %val = load <ir_ty>, <ir_ty>* %ptr
    /// ```
    fn emit_field_access(
        &mut self,
        obj: &Expr,
        field: &str,
    ) -> Result<String, CodegenError> {
        // Determine the owning automaton name.
        let auto_name = match &obj.kind {
            ExprKind::Path(segs) if segs.len() == 1 => {
                if segs[0] == "Self" {
                    match &self.enclosing_owner {
                        Some(o) => o.clone(),
                        None => {
                            return Err(CodegenError::NotYetImplemented {
                                what: "Self.field outside a #transition body",
                            });
                        }
                    }
                } else {
                    segs[0].clone()
                }
            }
            _ => {
                return Err(CodegenError::NotYetImplemented {
                    what: "non-path receiver in FieldAccess (slice 3+ supports Auto.field / Self.field)",
                });
            }
        };
        self.emit_field_access_by_name(&auto_name, field)
    }

    /// v0.2-ζ refactor: lower a field read against a resolved
    /// automaton name. Used by both `emit_field_access` (the
    /// `Auto.field` / `Self.field` expression form) and the
    /// `Snapshot` arm in `emit_expr` (the `@snapshot Auto.field`
    /// form). The two surface forms produce the same IR — the
    /// difference is upstream: `clifford-effect`'s read walker
    /// excludes `@snapshot` from `actual_reads` so the verifier
    /// treats it as race-free per ADR 0004.
    fn emit_field_access_by_name(
        &mut self,
        auto_name: &str,
        field: &str,
    ) -> Result<String, CodegenError> {
        // Slice 4 split: register-block field reads use volatile
        // load at `inttoptr (i64 base+offset to T*)`; non-register-
        // block reads use the slice-3 GEP+load shape.
        let (is_register_block, abs_addr_or_idx, ir_ty) = {
            let info = self.automatons.get(auto_name).ok_or_else(|| {
                CodegenError::UnresolvedName {
                    name: auto_name.to_owned(),
                }
            })?;
            let (idx, ir_ty, offset) = info
                .fields
                .iter()
                .enumerate()
                .find_map(|(i, (n, t, off))| {
                    if n == field {
                        Some((i, t.clone(), *off))
                    } else {
                        None
                    }
                })
                .ok_or_else(|| CodegenError::UnresolvedName {
                    name: format!("{auto_name}.{field}"),
                })?;
            if info.is_register_block {
                let abs = info.base_address + offset.unwrap_or(0);
                (true, abs, ir_ty)
            } else {
                // Slice 9: shift user-field index by the state-tag
                // slot for multi-state automatons (LLVM idx 0 is
                // reserved for the i32 tag; user fields start at 1).
                // This mirrors the write-path shift in emit_mutate.
                (false, info.llvm_field_index(idx) as u64, ir_ty)
            }
        };

        if is_register_block {
            // Register-block field read — volatile load at the
            // computed absolute MMIO address. Decision #6's per-
            // operation atomicity contract holds at the LLVM level
            // because volatile loads of word-sized integers are
            // single-instruction on every supported target.
            let val = self.fresh_value();
            writeln!(
                &mut self.out,
                "  {val} = load volatile {ir_ty}, {ir_ty}* inttoptr (i64 {abs} to {ir_ty}*)",
                ir_ty = ir_ty,
                abs = abs_addr_or_idx,
            )
            .ok();
            Ok(val)
        } else {
            // Non-register-block — slice-3 GEP+load.
            let struct_name = format!("%struct.{auto_name}");
            let idx = abs_addr_or_idx as usize;
            let ptr = self.fresh_value();
            writeln!(
                &mut self.out,
                "  {ptr} = getelementptr {struct_name}, {struct_name}* @{auto_name}.state, i32 0, i32 {idx}",
                struct_name = struct_name,
                auto_name = auto_name,
                idx = idx,
            )
            .ok();
            let val = self.fresh_value();
            writeln!(
                &mut self.out,
                "  {val} = load {ir_ty}, {ir_ty}* {ptr}",
                ir_ty = ir_ty,
                ptr = ptr,
            )
            .ok();
            Ok(val)
        }
    }

    /// v0.2-ζ: lower `@snapshot Auto.field` (Decision #24 / ADR
    /// 0004). Resolves the automaton name (with `Self` mapping
    /// to the enclosing transition's owner if applicable), then
    /// rejects the snapshot if the field's IR type is not a
    /// single-word primitive (`i8` / `i16` / `i32` / `i64` / `i1`).
    /// Compound fields (`{T1, T2, …}` structs, `[N x T]` arrays,
    /// pointer fields) would tear at the load level and can't
    /// satisfy the snapshot-and-decide guarantee that ADR 0004
    /// assumes; surface a structured `NotYetImplemented` for
    /// those.
    ///
    /// For primitive fields, the IR is identical to a regular
    /// `Auto.field` read — a single GEP+load (or volatile load
    /// for register-block fields). The "snapshot" semantic is
    /// upstream of codegen: `clifford-effect`'s read walker
    /// excludes snapshots from `actual_reads`, so the v0.2-β
    /// graded check doesn't pair the snapshot site against any
    /// concurrent write.
    fn emit_snapshot(
        &mut self,
        automaton: &str,
        field: &str,
    ) -> Result<String, CodegenError> {
        // Resolve `Self` to the enclosing automaton if we're
        // inside a transition (the transition resolver upstream
        // accepts `@snapshot Self.field` per spec).
        let auto_name: String = if automaton == "Self" {
            self.enclosing_owner
                .clone()
                .ok_or(CodegenError::NotYetImplemented {
                    what: "@snapshot Self.field outside a #transition body",
                })?
        } else {
            automaton.to_owned()
        };

        // v0.2-ζ MVP: reject non-primitive fields. The snapshot-
        // and-decide guarantee depends on the load being a single
        // hardware instruction; aggregate types load as multiple
        // ops and would tear concurrently with a write.
        let ir_ty = self
            .automatons
            .get(&auto_name)
            .and_then(|info| {
                info.fields.iter().find_map(|(n, t, _)| {
                    if n == field {
                        Some(t.clone())
                    } else {
                        None
                    }
                })
            })
            .ok_or_else(|| CodegenError::UnresolvedName {
                name: format!("{auto_name}.{field}"),
            })?;
        if !is_primitive_ir_ty_for_snapshot(&ir_ty) {
            return Err(CodegenError::NotYetImplemented {
                what: "@snapshot of non-primitive field (compound types tear at the load level; future slice may add memcpy-snapshot)",
            });
        }

        // Delegate to the regular field-access lowering. The
        // snapshot semantic is purely upstream (verifier excludes
        // it from actual_reads); the IR is the same load.
        self.emit_field_access_by_name(&auto_name, field)
    }

    /// Emit an SSA-binding identity instruction so a value gets a
    /// stable name. For integer types we use `add ty 0, v`; for
    /// floats `fadd ty 0.0, v`. LLVM's optimiser flattens these
    /// trivially.
    ///
    /// For *non-scalar* types (Refs, structs, vectors), the identity
    /// idiom doesn't apply directly. The caller falls back to using
    /// the value reference as-is (no rebinding) — which is fine
    /// because non-scalar `let` bindings already produce SSA names
    /// at their producing instruction (e.g. `getelementptr`).
    fn bind_via_identity(&mut self, ir_ty: &str, v: &str) -> String {
        if is_integer_ir_type(ir_ty) {
            let bind = self.fresh_value();
            writeln!(&mut self.out, "  {bind} = add {ir_ty} 0, {v}").ok();
            bind
        } else if ir_ty == "float" || ir_ty == "double" {
            let bind = self.fresh_value();
            writeln!(&mut self.out, "  {bind} = fadd {ir_ty} 0.0, {v}").ok();
            bind
        } else {
            // Non-scalar — pass through. The producing instruction
            // already named the value.
            v.to_owned()
        }
    }

    fn emit_unary(&mut self, op: UnaryOp, operand: &Expr) -> Result<String, CodegenError> {
        match op {
            UnaryOp::Neg => {
                let v = self.emit_expr(operand)?;
                let ir_ty = self.expr_ir_type(operand);
                let dst = self.fresh_value();
                if is_integer_ir_type(&ir_ty) {
                    // `-x` ≡ `0 - x`.
                    writeln!(&mut self.out, "  {dst} = sub {ir_ty} 0, {v}").ok();
                } else if ir_ty == "float" || ir_ty == "double" {
                    writeln!(&mut self.out, "  {dst} = fneg {ir_ty} {v}").ok();
                } else {
                    return Err(CodegenError::NotYetImplemented {
                        what: "unary `-` on non-scalar type",
                    });
                }
                Ok(dst)
            }
            UnaryOp::Not => {
                // Logical NOT on `bool` (i1): `xor i1 v, true`.
                let v = self.emit_expr(operand)?;
                let dst = self.fresh_value();
                writeln!(&mut self.out, "  {dst} = xor i1 {v}, true").ok();
                Ok(dst)
            }
            UnaryOp::BitNot => {
                let v = self.emit_expr(operand)?;
                let ir_ty = self.expr_ir_type(operand);
                if !is_integer_ir_type(&ir_ty) {
                    return Err(CodegenError::NotYetImplemented {
                        what: "bitwise `~` on non-integer type",
                    });
                }
                let dst = self.fresh_value();
                // `~x` ≡ `xor x, -1` (LLVM accepts -1 as all-ones).
                writeln!(&mut self.out, "  {dst} = xor {ir_ty} {v}, -1").ok();
                Ok(dst)
            }
            UnaryOp::Deref => {
                // `*r` on `r: &T` lowers to `load T, T* %r`. We need
                // the pointee type — peek at the operand's recorded
                // type (which should be a Ref) and unwrap one layer.
                let v = self.emit_expr(operand)?;
                let pointee_ir_ty = self.expr_pointee_ir_type(operand)?;
                let dst = self.fresh_value();
                writeln!(
                    &mut self.out,
                    "  {dst} = load {pointee_ir_ty}, {pointee_ir_ty}* {v}"
                )
                .ok();
                Ok(dst)
            }
        }
    }

    fn emit_binary(
        &mut self,
        op: BinaryOp,
        lhs: &Expr,
        rhs: &Expr,
    ) -> Result<String, CodegenError> {
        let l = self.emit_expr(lhs)?;
        let r = self.emit_expr(rhs)?;
        let ir_ty = self.expr_ir_type(lhs);
        // Slice 2: signed-vs-unsigned division/remainder driven by
        // the operand's recorded type. `udiv`/`urem` for unsigned
        // primitives (u8/u16/u32/u64/usize), `sdiv`/`srem` for
        // signed (i8/i16/i32/i64/isize). Float div/rem will land
        // alongside float-arithmetic support in a later slice.
        let signed = self.expr_is_signed_int(lhs);

        // Slice 13: comparison ops produce i1 (bool) from any
        // integer source — the dest type differs from the input
        // type. Dispatch first so we can pick the right opcode and
        // force the dest IR type to `i1`.
        let cmp_op = match op {
            BinaryOp::Eq => Some("eq"),
            BinaryOp::Ne => Some("ne"),
            BinaryOp::Lt => Some(if signed { "slt" } else { "ult" }),
            BinaryOp::Le => Some(if signed { "sle" } else { "ule" }),
            BinaryOp::Gt => Some(if signed { "sgt" } else { "ugt" }),
            BinaryOp::Ge => Some(if signed { "sge" } else { "uge" }),
            _ => None,
        };
        if let Some(cmp) = cmp_op {
            let dst = self.fresh_value();
            writeln!(
                &mut self.out,
                "  {dst} = icmp {cmp} {ir_ty} {l}, {r}",
            )
            .ok();
            return Ok(dst);
        }

        let opcode = match op {
            BinaryOp::Add => "add",
            BinaryOp::Sub => "sub",
            BinaryOp::Mul => "mul",
            BinaryOp::Div => {
                if signed {
                    "sdiv"
                } else {
                    "udiv"
                }
            }
            BinaryOp::Rem => {
                if signed {
                    "srem"
                } else {
                    "urem"
                }
            }
            // Slice 13 add-ons: bitwise + shift + logical ops.
            // Logical and/or on `bool` (i1) and bitwise variants
            // on integer types both lower to the same LLVM op
            // (`and`, `or`) — the IR type from `lhs` distinguishes.
            // Shift right is `lshr` (logical) for unsigned and
            // `ashr` (arithmetic) for signed; spec aligns with
            // the source type's signedness.
            BinaryOp::And | BinaryOp::BitAnd => "and",
            BinaryOp::Or | BinaryOp::BitOr => "or",
            BinaryOp::BitXor => "xor",
            BinaryOp::Shl => "shl",
            BinaryOp::Shr => {
                if signed {
                    "ashr"
                } else {
                    "lshr"
                }
            }
            // Comparison ops are handled above.
            BinaryOp::Eq
            | BinaryOp::Ne
            | BinaryOp::Lt
            | BinaryOp::Le
            | BinaryOp::Gt
            | BinaryOp::Ge => unreachable!("handled by cmp_op dispatch above"),
        };
        let dst = self.fresh_value();
        writeln!(&mut self.out, "  {dst} = {opcode} {ir_ty} {l}, {r}").ok();
        Ok(dst)
    }

    fn emit_call(
        &mut self,
        call_expr: &Expr,
        callee: &Expr,
        args: &[Expr],
    ) -> Result<String, CodegenError> {
        // Slice 1+: only single-segment Path callees are supported.
        let name = match &callee.kind {
            ExprKind::Path(segs) if segs.len() == 1 => segs[0].clone(),
            _ => {
                return Err(CodegenError::NotYetImplemented {
                    what: "non-path call callee",
                });
            }
        };
        let mut arg_strs: Vec<String> = Vec::with_capacity(args.len());
        for a in args {
            let v = self.emit_expr(a)?;
            let ty = self.expr_ir_type(a);
            arg_strs.push(format!("{ty} {v}"));
        }
        // Return type via Typing (slice 2). The type checker records
        // the call's *result* type under the outer call expression's
        // span (`call_expr.span`), not the callee identifier's span,
        // so we look up there. Fallback to i32 if typing has nothing
        // (partial / failed typing).
        let ret_ty = self.expr_ir_type(call_expr);
        let dst = self.fresh_value();
        writeln!(
            &mut self.out,
            "  {dst} = call {ret_ty} @{name}({args})",
            args = arg_strs.join(", ")
        )
        .ok();
        Ok(dst)
    }

    /// Slice 2 typing-aware IR-type lookup. Consults `Typing` first
    /// for the expression's recorded `Type`; falls back to syntactic
    /// clues only when typing has nothing for the span (which can
    /// happen on partial/unknown typings).
    ///
    /// Path expressions: when typing is silent, the local-binding
    /// table's recorded `ir_type` is the next authority — a let
    /// binding registered its IR type at declaration and we honor it.
    fn expr_ir_type(&self, expr: &Expr) -> String {
        if let Some(ty) = self.typing.lookup(expr.span) {
            return type_to_ir(ty);
        }
        // Fallback (typing has nothing for this expr): syntactic
        // guess so partial inputs still produce SOME IR. Path
        // expressions consult the local-binding table.
        match &expr.kind {
            ExprKind::IntLit(s) | ExprKind::HexLit(s) | ExprKind::BinLit(s) => {
                int_literal_ir_type(s).to_owned()
            }
            ExprKind::BoolLit(_) => "i1".to_owned(),
            ExprKind::Paren(inner) => self.expr_ir_type(inner),
            // Slice 13: comparison ops (and short-circuit logical
            // ops) yield `i1`; everything else yields the LHS's
            // IR type. Without this, `if x < 10 { … }` would
            // compute the cond as `i32` from the LHS and the br i1
            // would type-mismatch.
            ExprKind::Binary { op, lhs, .. } => match op {
                BinaryOp::Eq
                | BinaryOp::Ne
                | BinaryOp::Lt
                | BinaryOp::Le
                | BinaryOp::Gt
                | BinaryOp::Ge
                | BinaryOp::And
                | BinaryOp::Or => "i1".to_owned(),
                _ => self.expr_ir_type(lhs),
            },
            ExprKind::Path(segs) if segs.len() == 1 => self
                .lookup_local_ir_type(&segs[0])
                .unwrap_or_else(|| "i32".to_owned()),
            _ => "i32".to_owned(),
        }
    }

    /// True if the expression's type is a signed integer primitive.
    /// Used to pick `sdiv`/`srem` over `udiv`/`urem`.
    fn expr_is_signed_int(&self, expr: &Expr) -> bool {
        if let Some(ty) = self.typing.lookup(expr.span) {
            return matches!(
                ty,
                Type::Primitive(
                    PrimitiveType::I8
                        | PrimitiveType::I16
                        | PrimitiveType::I32
                        | PrimitiveType::I64
                        | PrimitiveType::Isize
                )
            );
        }
        // Syntactic fallback: integer literal suffixes.
        match &expr.kind {
            ExprKind::IntLit(s) | ExprKind::HexLit(s) | ExprKind::BinLit(s) => {
                let suffix = literal_suffix(s);
                matches!(suffix, "i8" | "i16" | "i32" | "i64" | "isize")
            }
            ExprKind::Paren(inner) => self.expr_is_signed_int(inner),
            ExprKind::Binary { lhs, .. } => self.expr_is_signed_int(lhs),
            _ => false, // default to unsigned (firmware-friendly)
        }
    }

    /// Return the IR type of the *pointee* when the operand is a
    /// reference. Used by `*r` deref lowering. Errors if the operand
    /// isn't typed as a reference.
    fn expr_pointee_ir_type(&self, operand: &Expr) -> Result<String, CodegenError> {
        match self.typing.lookup(operand.span) {
            Some(Type::Ref { inner, .. }) => Ok(type_to_ir(inner)),
            _ => Err(CodegenError::NotYetImplemented {
                what: "deref of non-reference operand (Typing didn't record a Ref type)",
            }),
        }
    }

    fn lookup_local_ir_type(&self, name: &str) -> Option<String> {
        self.locals
            .iter()
            .rev()
            .find_map(|b| if b.name == name { Some(b.ir_type.clone()) } else { None })
    }

    /// Slice 12: look up a local and return its `(value, ir_type,
    /// storage)` triple. Used by `ExprKind::Path` lowering to
    /// dispatch on whether the binding is SSA-direct or stack-
    /// allocated. Returns owned `String`s so callers don't need to
    /// borrow `self` while emitting subsequent IR.
    fn lookup_local_with_storage(
        &self,
        name: &str,
    ) -> Option<(String, String, LocalStorage)> {
        self.locals.iter().rev().find_map(|b| {
            if b.name == name {
                Some((b.value.clone(), b.ir_type.clone(), b.storage))
            } else {
                None
            }
        })
    }

    fn lower_return_type(&mut self, ret: Option<&TypeExpr>) -> String {
        match ret {
            None => "void".to_owned(),
            Some(t) => self.lower_type(t).unwrap_or_else(|e| {
                self.errors.push(e);
                "void".to_owned()
            }),
        }
    }

    // `&mut self` is kept on lower_type for forward-compat: when ADT
    // / nominal lowering lands (codegen slice 3+) it'll need to mutate
    // emitter state (e.g. emit out-of-line struct type definitions
    // for tagged-union ADT representations), and forcing call sites
    // to thread `&mut` now is cheaper than churning later.
    #[allow(clippy::only_used_in_recursion)]
    fn lower_type(&mut self, t: &TypeExpr) -> Result<String, CodegenError> {
        match &t.kind {
            TypeKind::Unit => Ok("void".to_owned()),
            TypeKind::Primitive(p) => Ok(primitive_ir_type(*p).to_owned()),
            TypeKind::Ref(rt) => {
                // `&T` / `&mut T` → `T*` per §8.3. Slice 2 doesn't yet
                // emit `noalias` on `&mut` parameters at the IR-attribute
                // level (param attributes go on the `define` line, not
                // the type); a future slice will thread mutability
                // through to attribute generation.
                let inner = self.lower_type(&rt.inner)?;
                Ok(format!("{inner}*"))
            }
            TypeKind::Array(at) => {
                use clifford_ast::ArraySize;
                let ArraySize::IntLiteral(size) = &at.size;
                let elem = self.lower_type(&at.element)?;
                // LLVM accepts a literal integer; strip underscores.
                let n: String = size.chars().filter(|c| *c != '_').collect();
                Ok(format!("[{n} x {elem}]"))
            }
            TypeKind::Slice(st) => {
                // `[T]` is the standard fat-pointer (ptr + len) layout
                // per §8.3. We emit `{T*, i64}`.
                let elem = self.lower_type(&st.element)?;
                Ok(format!("{{{elem}*, i64}}"))
            }
            TypeKind::Tuple(tt) => {
                // `(T1, T2, …)` → LLVM struct.
                let mut parts: Vec<String> = Vec::with_capacity(tt.elements.len());
                for e in &tt.elements {
                    parts.push(self.lower_type(e)?);
                }
                Ok(format!("{{{}}}", parts.join(", ")))
            }
            // `Path(...)` (nominal-type aliases / ADTs) and
            // `Access(...)` and `Fn(...)` defer to subsequent slices.
            // ADT lowering needs tagged-union representation; access
            // pointer types follow the `&T` shape but with target-
            // specific provenance (Decision #19); fn pointers need
            // their full signature lowered.
            _ => Err(CodegenError::NotYetImplemented {
                what: type_kind_name(&t.kind),
            }),
        }
    }
}

/// Translate a `clifford_types::Type` to its LLVM IR type-text form.
///
/// Used by [`Emitter::expr_ir_type`] when typing has a recorded type
/// for an expression. Mirrors [`Emitter::lower_type`] but operates on
/// the semantic `Type` rather than the syntactic `TypeExpr`.
fn type_to_ir(ty: &Type) -> String {
    match ty {
        Type::Unit => "void".to_owned(),
        Type::Primitive(p) => primitive_ir_type(*p).to_owned(),
        Type::Ref { inner, .. } => format!("{}*", type_to_ir(inner)),
        Type::Array { element, size } => {
            let n: String = size.chars().filter(|c| *c != '_').collect();
            format!("[{n} x {}]", type_to_ir(element))
        }
        Type::Slice { element } => format!("{{{}*, i64}}", type_to_ir(element)),
        Type::Tuple(elems) => {
            let parts: Vec<String> = elems.iter().map(type_to_ir).collect();
            format!("{{{}}}", parts.join(", "))
        }
        Type::Range { element, .. } => {
            // Range value type — codegen for sigma loops will lower
            // these explicitly. Outside of sigma context, treat as
            // `{T, T}` (lo, hi pair).
            let e = type_to_ir(element);
            format!("{{{e}, {e}}}")
        }
        Type::StringSlice => "{i8*, i64}".to_owned(),
        // Nominals (aliases / ADTs) and Unknown — slice-2 punt:
        // aliases should have been unaliased upstream (T4b); ADTs
        // need tagged-union lowering (codegen slice 3+); Unknown
        // becomes `i32` as a conservative best-effort.
        Type::Nominal { .. } | Type::Unknown(_) => "i32".to_owned(),
    }
}

// ─── Type-mapping helpers ──────────────────────────────────────────────────

/// Map a Clifford primitive to its LLVM IR type name.
///
/// Pointer-sized integers (`usize` / `isize`) lower to `i64` for the
/// v0.1 default 64-bit target. A future target-aware slice will
/// thread the data-layout in.
///
/// `char` lowers to `i32` (Unicode scalar value, matching Rust's
/// `char` representation).
const fn primitive_ir_type(p: PrimitiveType) -> &'static str {
    match p {
        PrimitiveType::Bool => "i1",
        PrimitiveType::U8 | PrimitiveType::I8 => "i8",
        PrimitiveType::U16 | PrimitiveType::I16 => "i16",
        PrimitiveType::U32 | PrimitiveType::I32 | PrimitiveType::Char => "i32",
        PrimitiveType::U64 | PrimitiveType::I64 => "i64",
        PrimitiveType::Usize | PrimitiveType::Isize => "i64",
        PrimitiveType::F32 => "float",
        PrimitiveType::F64 => "double",
    }
}

/// Where an automaton field lives. Slice 3 introduced the
/// distinction implicitly; slice 4 makes it explicit so register-
/// block fields lower through volatile loads/stores at fixed
/// addresses (per Decision #6) and non-register-block fields lower
/// through `getelementptr` against the global state struct.
#[derive(Debug, Clone, Copy)]
enum FieldLocation {
    /// Non-register-block field — index into the
    /// `%struct.<Auto>` layout. `getelementptr` against
    /// `@<Auto>.state` with this index recovers the field's
    /// pointer.
    Struct { idx: usize },
    /// Register-block field — fixed MMIO address. The full
    /// address is `automaton.base_address + field.offset`,
    /// pre-summed at registry-build time (slice 4). The IR
    /// emits `inttoptr (i64 <abs> to <T>*)` and uses volatile
    /// loads/stores for atomicity per Decision #6.
    RegisterBlock { absolute_address: u64 },
}

/// Parse a hex-form address literal (`"0x4000_0000"` /
/// `"0xFF"` etc., possibly with `_` separators) to a `u64` value.
/// Decimal literals are also accepted as a defensive convenience.
/// Returns `None` if the literal is malformed.
/// v0.2-ζ: True iff the IR type is a single-word primitive that
/// loads atomically on every supported target. Used by
/// `emit_snapshot` to reject `@snapshot` on compound types,
/// where a single source-level read maps to multiple loads that
/// would tear under concurrent write.
fn is_primitive_ir_ty_for_snapshot(ir_ty: &str) -> bool {
    matches!(ir_ty, "i1" | "i8" | "i16" | "i32" | "i64")
}

/// v0.2-ε: kind-name for diagnostics + IR comments. Used by both
/// the entry-mask emitter (to label which kind of atomicity
/// triggered the wrapping) and the unmask-pending check.
fn atomic_kind_str(kind: &clifford_ast::AtomicKind) -> &'static str {
    match kind {
        clifford_ast::AtomicKind::InterruptCritical => "interrupt_critical",
        clifford_ast::AtomicKind::MulticoreCritical => "multicore_critical",
        clifford_ast::AtomicKind::Custom(_) => "<custom>",
        // `AtomicKind` is `#[non_exhaustive]`. Forward-compat:
        // unknown variants render as `<unknown>` so callers can
        // emit a structured diagnostic without crashing.
        _ => "<unknown>",
    }
}

/// (kept for legacy callers — superseded by the
/// [`Emitter::emit_atomic_entry_mask`] method which actually emits
/// the runtime wrapping. Existing call sites have been migrated;
/// this stub exists only to mark the v0.2-δ → v0.2-ε transition
/// in the diff history and may be deleted in a follow-up cleanup.)
#[allow(dead_code)]
fn emit_atomic_marker_if_any(out: &mut String, atomic: Option<&clifford_ast::AtomicKind>) {
    if let Some(kind) = atomic {
        let kind_str = atomic_kind_str(kind);
        writeln!(
            out,
            "  ; #atomic: {kind_str} (legacy marker — runtime wrapping is now emitted; see emit_atomic_entry_mask)"
        )
        .ok();
    }
}

fn parse_address_literal(s: &str) -> Option<u64> {
    let trimmed: String = s.chars().filter(|c| *c != '_').collect();
    if let Some(body) = trimmed.strip_prefix("0x").or_else(|| trimmed.strip_prefix("0X")) {
        u64::from_str_radix(body, 16).ok()
    } else if let Some(body) = trimmed.strip_prefix("0b").or_else(|| trimmed.strip_prefix("0B")) {
        u64::from_str_radix(body, 2).ok()
    } else {
        trimmed.parse::<u64>().ok()
    }
}

/// Decision #22 codegen: memory-ordering fences derived from a
/// callable's `$ [TraitList]`.
///
/// The pure-side `Acquire` / `Release` / `SeqCst` row labels (per
/// ADR 0003 P2) and the imperative-side `Acquire` / `Release` /
/// `SeqCst` traits (per Decision #22) share names because they
/// share intent: they're memory-ordering markers consumed by codegen
/// to emit appropriate fences.
///
/// **Mapping:**
///
/// - `Acquire` → entry fence `acquire`. Prevents loads/stores
///   *after* the fence from being reordered before it. Pairs with
///   a Release on the other side of the synchronisation.
/// - `Release` → exit fence `release` (emitted before each `ret`).
///   Prevents loads/stores *before* the fence from being reordered
///   after it.
/// - `SeqCst` → both entry and exit fences with `seq_cst` ordering.
///   Sequential consistency is the strongest LLVM ordering;
///   `seq_cst` operations participate in a single global total
///   order. Supersedes `Acquire`/`Release` when present.
///
/// Combinations:
/// - `[Acquire, Release]` → entry `acquire`, exit `release`.
/// - `[SeqCst]` → entry `seq_cst`, exit `seq_cst`. (`SeqCst` alone
///   is the canonical strongest form.)
/// - `[SeqCst, Acquire]` / `[SeqCst, Release]` → `seq_cst` wins
///   (no point downgrading; subsumes both).
/// - Empty / no ordering trait → no fences.
///
/// LLVM's backend selects target-appropriate fence instructions
/// (`dmb ish` on ARM, `mfence` on x86 for `seq_cst`, etc.) — we
/// just emit the abstract `fence <ordering>` IR form.
struct MemoryOrdering {
    /// LLVM ordering keyword for the entry fence, or `None` if no
    /// entry fence should be emitted.
    entry: Option<&'static str>,
    /// LLVM ordering keyword for the fence emitted before each `ret`,
    /// or `None`.
    exit: Option<&'static str>,
}

fn memory_ordering_from_traits(trait_list: &[clifford_ast::TraitRef]) -> MemoryOrdering {
    let has_seqcst = trait_list.iter().any(|t| t.name == "SeqCst");
    if has_seqcst {
        return MemoryOrdering {
            entry: Some("seq_cst"),
            exit: Some("seq_cst"),
        };
    }
    let has_acquire = trait_list.iter().any(|t| t.name == "Acquire");
    let has_release = trait_list.iter().any(|t| t.name == "Release");
    MemoryOrdering {
        entry: if has_acquire { Some("acquire") } else { None },
        exit: if has_release { Some("release") } else { None },
    }
}

/// Pick the LLVM opcode for a compound-assignment operator
/// (`+=`, `-=`, `*=`, `/=`, `%=`, `&=`, `|=`, `^=`, `<<=`, `>>=`).
/// `Eq` is handled at the call site (plain store, no opcode).
///
/// Slice 3: integer-only — float compound assigns surface as a
/// `NotYetImplemented` upstream when the call sites pass `float` /
/// `double` IR types. Sign-aware ops (`sdiv`/`srem`) are NOT chosen
/// here — `+=` / `-=` etc. don't care about sign; only `/` and `%`
/// do, and within compound-assign they default to the unsigned
/// form for slice-3 simplicity (sign-aware is slice 4 work).
fn compound_assign_opcode(op: AssignOp, _ir_ty: &str) -> &'static str {
    match op {
        AssignOp::PlusEq => "add",
        AssignOp::MinusEq => "sub",
        AssignOp::StarEq => "mul",
        AssignOp::SlashEq => "udiv",
        AssignOp::PercentEq => "urem",
        AssignOp::AmpEq => "and",
        AssignOp::PipeEq => "or",
        AssignOp::CaretEq => "xor",
        AssignOp::ShlEq => "shl",
        AssignOp::ShrEq => "lshr",
        AssignOp::Eq => "store", // unreachable — caller branches on Eq before calling
    }
}

/// Slice 5: parse `[N x T]` and return `T`. Used by indexed-field
/// operations to know what type to load / store at the element
/// pointer.
///
/// Returns `None` for non-array IR types (primitives, refs, structs,
/// slices, etc.); the caller is expected to surface a
/// `NotYetImplemented` for those cases.
fn array_element_ir_type(ir_ty: &str) -> Option<String> {
    let body = ir_ty.strip_prefix('[')?;
    let body = body.strip_suffix(']')?;
    // Body is `<N> x <T>`. Split on the FIRST ` x ` only, so element
    // types containing spaces (e.g. `[4 x [4 x i8]]`) survive intact.
    let (_n, t) = body.split_once(" x ")?;
    Some(t.trim().to_owned())
}

/// Slice 8: True if a syntactic `TypeExpr` names a signed integer
/// primitive. Used by `#unchecked_cast` to pick `sext` vs `zext`
/// when widening from a user-written source type.
fn type_expr_is_signed_int(t: &TypeExpr) -> bool {
    matches!(
        &t.kind,
        TypeKind::Primitive(
            PrimitiveType::I8
                | PrimitiveType::I16
                | PrimitiveType::I32
                | PrimitiveType::I64
                | PrimitiveType::Isize
        )
    )
}

/// Slice 7: bit width for an integer LLVM IR type-text.
///
/// Returns `Some(n)` for `i1` / `i8` / `i16` / `i32` / `i64` / `i128`
/// (the integer types Clifford lowers to today). Returns `None` for
/// anything else (`void`, `i32*`, `[N x T]`, `{T1, T2}`, `float`, …)
/// so the caller can dispatch to a different code path.
fn int_bits(ir_ty: &str) -> Option<u32> {
    match ir_ty {
        "i1" => Some(1),
        "i8" => Some(8),
        "i16" => Some(16),
        "i32" => Some(32),
        "i64" => Some(64),
        "i128" => Some(128),
        _ => None,
    }
}

/// Slice 6: extract a const integer count from an AST expression
/// suitable for an array-repeat `[v; count]` literal. Returns `Some(n)`
/// only if `count` is a literal integer node (decimal / hex / binary)
/// whose parsed value fits in `usize`. Returns `None` for any other
/// expression shape — callers surface that as `NotYetImplemented`.
fn const_int_count(expr: &Expr) -> Option<usize> {
    let raw = match &expr.kind {
        ExprKind::IntLit(s) => parse_int_literal(s).ok()?.0,
        ExprKind::HexLit(s) => parse_hex_literal(s).ok()?.0,
        ExprKind::BinLit(s) => parse_bin_literal(s).ok()?.0,
        ExprKind::Paren(inner) => return const_int_count(inner),
        _ => return None,
    };
    raw.parse::<usize>().ok()
}

/// True if the IR type-text names an integer LLVM type (`i1`, `i8`,
/// `i16`, `i32`, `i64`, `i128`, …). Used to gate integer-only ops
/// (the SSA-add-zero binding idiom; integer-shape unary `-` and
/// `~`; integer-only div/rem).
fn is_integer_ir_type(ir_ty: &str) -> bool {
    if let Some(rest) = ir_ty.strip_prefix('i') {
        rest.bytes().all(|b| b.is_ascii_digit()) && !rest.is_empty()
    } else {
        false
    }
}

/// Extract the alphabetic suffix from an integer literal (after the
/// digits / hex / binary body). Returns `""` for an unsuffixed
/// literal; otherwise something like `"u32"` or `"isize"`.
fn literal_suffix(literal: &str) -> &str {
    let trimmed_len = literal.trim_end_matches(|c: char| c.is_ascii_alphabetic()).len();
    &literal[trimmed_len..]
}

/// Pick an IR integer-type from an integer literal's source suffix.
///
/// Recognised suffixes: `u8` / `u16` / `u32` / `u64` / `usize` /
/// `i8` / `i16` / `i32` / `i64` / `isize`. Unsuffixed defaults to
/// `i32` per spec §4.8 (the same default the type checker uses).
fn int_literal_ir_type(literal: &str) -> &'static str {
    // Strip underscores so suffix detection is straightforward.
    // The suffix is the trailing alphanumeric run after the digits.
    let trimmed: String = literal.chars().filter(|c| *c != '_').collect();
    let suffix_start = trimmed
        .char_indices()
        .rev()
        .take_while(|(_, c)| c.is_ascii_alphabetic() || c.is_ascii_digit())
        .filter(|(_, c)| !c.is_ascii_digit())
        .last()
        .map(|(i, _)| i);
    let suffix = match suffix_start {
        Some(i) => &trimmed[i..],
        None => return "i32",
    };
    match suffix {
        "u8" | "i8" => "i8",
        "u16" | "i16" => "i16",
        "u32" | "i32" => "i32",
        "u64" | "i64" => "i64",
        "usize" | "isize" => "i64",
        _ => "i32",
    }
}

/// Parse a decimal integer literal into its IR-form value text.
/// Returns `(value_text, ir_type)`.
///
/// Strips underscores and any type suffix, returning just the digits.
/// LLVM accepts decimal integer constants directly without modification.
fn parse_int_literal(literal: &str) -> Result<(String, &'static str), CodegenError> {
    let trimmed: String = literal.chars().filter(|c| *c != '_').collect();
    // Find suffix start.
    let suffix_start = trimmed
        .char_indices()
        .rev()
        .take_while(|(_, c)| c.is_ascii_alphabetic() || c.is_ascii_digit())
        .filter(|(_, c)| !c.is_ascii_digit())
        .last()
        .map(|(i, _)| i);
    let (digits, suffix) = match suffix_start {
        Some(i) => (&trimmed[..i], &trimmed[i..]),
        None => (trimmed.as_str(), ""),
    };
    if digits.is_empty() {
        return Err(CodegenError::BadLiteral {
            literal: literal.to_owned(),
            reason: "no digits before suffix",
        });
    }
    let ir_ty = match suffix {
        "" => "i32",
        "u8" | "i8" => "i8",
        "u16" | "i16" => "i16",
        "u32" | "i32" => "i32",
        "u64" | "i64" => "i64",
        "usize" | "isize" => "i64",
        _ => {
            return Err(CodegenError::BadLiteral {
                literal: literal.to_owned(),
                reason: "unrecognised integer suffix",
            });
        }
    };
    Ok((digits.to_owned(), ir_ty))
}

/// Parse a `0xHEX` hex literal into its IR-form decimal value text.
fn parse_hex_literal(literal: &str) -> Result<(String, &'static str), CodegenError> {
    let trimmed: String = literal.chars().filter(|c| *c != '_').collect();
    if !trimmed.starts_with("0x") && !trimmed.starts_with("0X") {
        return Err(CodegenError::BadLiteral {
            literal: literal.to_owned(),
            reason: "hex literal must start with `0x`",
        });
    }
    let body = &trimmed[2..];
    // Find suffix start in body.
    let suffix_start = body
        .char_indices()
        .rev()
        .take_while(|(_, c)| c.is_ascii_alphabetic() || c.is_ascii_digit())
        .filter(|(_, c)| !c.is_ascii_hexdigit())
        .last()
        .map(|(i, _)| i);
    let (hex_digits, suffix) = match suffix_start {
        Some(i) => (&body[..i], &body[i..]),
        None => (body, ""),
    };
    let value = u64::from_str_radix(hex_digits, 16).map_err(|_| CodegenError::BadLiteral {
        literal: literal.to_owned(),
        reason: "hex digits don't fit in u64",
    })?;
    let ir_ty = match suffix {
        "" => "i32",
        "u8" | "i8" => "i8",
        "u16" | "i16" => "i16",
        "u32" | "i32" => "i32",
        "u64" | "i64" => "i64",
        "usize" | "isize" => "i64",
        _ => {
            return Err(CodegenError::BadLiteral {
                literal: literal.to_owned(),
                reason: "unrecognised hex suffix",
            });
        }
    };
    Ok((value.to_string(), ir_ty))
}

/// Parse a `0bBINARY` binary literal into its IR-form decimal value text.
fn parse_bin_literal(literal: &str) -> Result<(String, &'static str), CodegenError> {
    let trimmed: String = literal.chars().filter(|c| *c != '_').collect();
    if !trimmed.starts_with("0b") && !trimmed.starts_with("0B") {
        return Err(CodegenError::BadLiteral {
            literal: literal.to_owned(),
            reason: "binary literal must start with `0b`",
        });
    }
    let body = &trimmed[2..];
    let suffix_start = body
        .char_indices()
        .rev()
        .take_while(|(_, c)| c.is_ascii_alphabetic() || c.is_ascii_digit())
        .filter(|(_, c)| *c != '0' && *c != '1')
        .last()
        .map(|(i, _)| i);
    let (bin_digits, suffix) = match suffix_start {
        Some(i) => (&body[..i], &body[i..]),
        None => (body, ""),
    };
    let value = u64::from_str_radix(bin_digits, 2).map_err(|_| CodegenError::BadLiteral {
        literal: literal.to_owned(),
        reason: "binary digits don't fit in u64",
    })?;
    let ir_ty = match suffix {
        "" => "i32",
        "u8" | "i8" => "i8",
        "u16" | "i16" => "i16",
        "u32" | "i32" => "i32",
        "u64" | "i64" => "i64",
        "usize" | "isize" => "i64",
        _ => {
            return Err(CodegenError::BadLiteral {
                literal: literal.to_owned(),
                reason: "unrecognised binary suffix",
            });
        }
    };
    Ok((value.to_string(), ir_ty))
}

// ─── Diagnostic helpers ────────────────────────────────────────────────────

const fn stmt_kind_name(s: &StmtKind) -> &'static str {
    match s {
        StmtKind::Mutate { .. } => "#mutate statement",
        StmtKind::MutateShort { .. } => "mutation-sugar statement",
        StmtKind::ProcCall { .. } => "#> proc() call statement",
        StmtKind::UncheckedStore { .. } => "#unchecked_store statement",
        StmtKind::VolatileStore { .. } => "#volatile_store statement",
        // The exhaustive variants we already handle:
        StmtKind::Let { .. } => "let statement",
        StmtKind::LetShort { .. } => "let-short statement",
        StmtKind::Expr(_) => "expression statement",
        StmtKind::Return(_) => "return statement",
        // `Stmt` is `#[non_exhaustive]`; unknown variants fall through.
        _ => "unknown statement kind",
    }
}

const fn expr_kind_name(e: &ExprKind) -> &'static str {
    match e {
        ExprKind::IntLit(_) => "integer literal",
        ExprKind::HexLit(_) => "hex literal",
        ExprKind::BinLit(_) => "binary literal",
        ExprKind::FloatLit(_) => "float literal",
        ExprKind::CharLit(_) => "char literal",
        ExprKind::ByteLit(_) => "byte literal",
        ExprKind::StringLit(_) => "string literal",
        ExprKind::BoolLit(_) => "bool literal",
        ExprKind::Null => "null literal",
        ExprKind::Path(_) => "path expression",
        ExprKind::StateRead(_) => "Auto@state expression",
        ExprKind::Snapshot { .. } => "@snapshot expression",
        ExprKind::Paren(_) => "parenthesised expression",
        ExprKind::Tuple(_) => "tuple expression",
        ExprKind::Array(_) => "array literal",
        ExprKind::ArrayRepeat { .. } => "array-repeat literal",
        ExprKind::FieldAccess { .. } => "field access",
        ExprKind::Index { .. } => "index expression",
        ExprKind::Call { .. } => "call expression",
        ExprKind::MethodCall { .. } => "method call",
        ExprKind::Unary { .. } => "unary expression",
        ExprKind::Ref { .. } => "borrow expression",
        ExprKind::Binary { .. } => "binary expression",
        ExprKind::Cast { .. } => "cast expression",
        ExprKind::Range { .. } => "range expression",
        ExprKind::UncheckedLoad { .. } => "#unchecked_load expression",
        ExprKind::VolatileLoad { .. } => "#volatile_load expression",
        ExprKind::UncheckedCast { .. } => "#unchecked_cast expression",
        ExprKind::UncheckedOffset { .. } => "#unchecked_offset expression",
        // `ExprKind` is `#[non_exhaustive]`; future variants fall through.
        _ => "unknown expression kind",
    }
}

const fn type_kind_name(t: &TypeKind) -> &'static str {
    match t {
        TypeKind::Unit => "unit type",
        TypeKind::Primitive(_) => "primitive type",
        TypeKind::Path(_) => "nominal-path type",
        TypeKind::Ref(_) => "reference type",
        TypeKind::Array(_) => "array type",
        TypeKind::Slice(_) => "slice type",
        TypeKind::Tuple(_) => "tuple type",
        TypeKind::Access(_) => "access<T> type",
        TypeKind::Fn(_) => "@fn pointer type",
        // `TypeKind` is `#[non_exhaustive]`; unknown variants fall through.
        _ => "unknown type kind",
    }
}

/// Slice 13: human-readable name for a [`BinaryOp`]. Reserved for
/// diagnostics — every operator is now lowered, so this is no
/// longer reachable from `emit_binary`'s fall-through.
#[allow(dead_code)]
const fn binary_op_name(op: BinaryOp) -> &'static str {
    match op {
        BinaryOp::Add => "+",
        BinaryOp::Sub => "-",
        BinaryOp::Mul => "*",
        BinaryOp::Div => "/",
        BinaryOp::Rem => "%",
        BinaryOp::And => "&&",
        BinaryOp::Or => "||",
        BinaryOp::Eq => "==",
        BinaryOp::Ne => "!=",
        BinaryOp::Lt => "<",
        BinaryOp::Le => "<=",
        BinaryOp::Gt => ">",
        BinaryOp::Ge => ">=",
        BinaryOp::BitAnd => "&",
        BinaryOp::BitOr => "|",
        BinaryOp::BitXor => "^",
        BinaryOp::Shl => "<<",
        BinaryOp::Shr => ">>",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use clifford_lexer::tokenize;
    use clifford_parser::parse;
    use clifford_resolve::resolve;
    use clifford_types::infer;

    fn lower_str(src: &str) -> Result<String, Vec<CodegenError>> {
        let tokens = tokenize(src).expect("tokenize");
        let program = parse(&tokens).expect("parse");
        let resolution = resolve(&program).expect("resolve");
        // Allow typing failures: tests may exercise programs that
        // have type errors but still want to verify codegen's
        // syntactic-fallback behaviour. When typing fails we use
        // a default empty Typing.
        let typing = infer(&program, &resolution).unwrap_or_default();
        lower(&program, &resolution, &typing, "test")
    }

    // ─── Module + empty-program shape ────────────────────────────────────

    #[test]
    fn empty_program_emits_module_header() {
        let ir = lower_str("").expect("empty program lowers");
        assert!(ir.contains("ModuleID = 'test'"), "missing ModuleID; got:\n{ir}");
        assert!(
            ir.contains("source_filename = \"test\""),
            "missing source_filename; got:\n{ir}"
        );
    }

    #[test]
    fn non_fn_items_now_lowered_per_slice_3() {
        // Renamed from `non_fn_items_silently_skipped`. Slice 1 lowered
        // @fn only and #automaton was skipped; slice 3 emits a state
        // struct + global state instance for non-register-block
        // automatons. The test asserts the slice-3 surface.
        let src = "#automaton C { v: u32; }\n@fn add(a: u32, b: u32) -> u32 { return a; }\n";
        let ir = lower_str(src).expect("partial program lowers");
        assert!(
            ir.contains("define i32 @add"),
            "expected @add to be lowered; got:\n{ir}"
        );
        assert!(
            ir.contains("%struct.C = type { i32 }"),
            "expected state struct for #automaton C; got:\n{ir}"
        );
        assert!(
            ir.contains("@C.state = global %struct.C zeroinitializer"),
            "expected zero-initialised global state; got:\n{ir}"
        );
    }

    // ─── @fn signature lowering ──────────────────────────────────────────

    #[test]
    fn fn_no_params_void_return() {
        let ir = lower_str("@fn nothing() { return; }").expect("lower nothing");
        assert!(
            ir.contains("define void @nothing() {"),
            "expected void no-arg signature; got:\n{ir}"
        );
        assert!(ir.contains("ret void"), "missing ret void; got:\n{ir}");
    }

    #[test]
    fn fn_with_primitive_params_and_return() {
        let ir = lower_str("@fn id(x: u32) -> u32 { return x; }").expect("lower id");
        assert!(
            ir.contains("define i32 @id(i32 %x) {"),
            "expected `define i32 @id(i32 %x)`; got:\n{ir}"
        );
        assert!(ir.contains("ret i32 %x"), "expected `ret i32 %x`; got:\n{ir}");
    }

    #[test]
    fn fn_bool_param_lowers_to_i1() {
        let ir = lower_str("@fn neg(b: bool) -> bool { return b; }").expect("lower neg");
        assert!(
            ir.contains("define i1 @neg(i1 %b)"),
            "expected i1 for bool; got:\n{ir}"
        );
    }

    #[test]
    fn fn_returning_int_literal() {
        let ir = lower_str("@fn five() -> u32 { return 5u32; }").expect("lower five");
        assert!(
            ir.contains("define i32 @five()") && ir.contains("ret i32 5"),
            "expected literal lowering; got:\n{ir}"
        );
    }

    #[test]
    fn fn_with_simple_arithmetic() {
        let ir = lower_str("@fn add(a: u32, b: u32) -> u32 { return a + b; }")
            .expect("lower add");
        // The binary expression yields a fresh SSA temp via `add i32 %a, %b`.
        assert!(
            ir.contains("add i32 %a, %b"),
            "expected `add i32 %a, %b`; got:\n{ir}"
        );
        assert!(ir.contains("ret i32 %tmp."), "expected ret of SSA temp; got:\n{ir}");
    }

    #[test]
    fn fn_with_multiple_arithmetic_ops() {
        let ir = lower_str("@fn calc(a: u32, b: u32) -> u32 { return a * b - a; }")
            .expect("lower calc");
        // Both `mul` and `sub` should appear.
        assert!(ir.contains("mul i32"), "expected mul; got:\n{ir}");
        assert!(ir.contains("sub i32"), "expected sub; got:\n{ir}");
    }

    #[test]
    fn fn_with_call_expression() {
        let src = "\
            @fn double(x: u32) -> u32 { return x; }\n\
            @fn caller(n: u32) -> u32 { return double(n); }\n\
        ";
        let ir = lower_str(src).expect("lower call");
        assert!(ir.contains("define i32 @double"), "missing double; got:\n{ir}");
        assert!(
            ir.contains("call i32 @double(i32 %n)"),
            "expected call site; got:\n{ir}"
        );
    }

    #[test]
    fn fn_with_let_binding() {
        let ir = lower_str("@fn use_let(a: u32) -> u32 { let _x: u32 = a; return _x; }")
            .expect("lower let");
        // The let binds via an SSA-add identity; ret should reference
        // the bound name.
        assert!(
            ir.contains("add i32 0, %a"),
            "expected SSA-bind via add 0,a; got:\n{ir}"
        );
    }

    #[test]
    fn fn_with_let_short_binding() {
        let ir = lower_str("@fn use_letshort(a: u32) -> u32 { let _x := a; return _x; }")
            .expect("lower let-short");
        assert!(
            ir.contains("add i32 0, %a"),
            "expected SSA-bind for let-short; got:\n{ir}"
        );
    }

    #[test]
    fn multiple_fns_each_emit_independently() {
        let src = "\
            @fn one() -> u32 { return 1u32; }\n\
            @fn two() -> u32 { return 2u32; }\n\
        ";
        let ir = lower_str(src).expect("lower two fns");
        assert!(ir.contains("define i32 @one()"), "missing @one; got:\n{ir}");
        assert!(ir.contains("define i32 @two()"), "missing @two; got:\n{ir}");
        assert!(ir.contains("ret i32 1"), "expected ret 1; got:\n{ir}");
        assert!(ir.contains("ret i32 2"), "expected ret 2; got:\n{ir}");
    }

    // ─── Error paths ─────────────────────────────────────────────────────

    #[test]
    fn tuple_expression_now_lowered_per_slice_6() {
        // Renamed from `unsupported_expression_emits_e0810`. Slice 1
        // surfaced tuples as `NotYetImplemented`; slice 6 lowers them
        // to an `insertvalue` chain on `undef` of the tuple's struct
        // IR type. The test now asserts the slice-6 surface.
        let src = "@fn t() { let _x := (1u32, 2u32); return; }";
        let ir = lower_str(src).expect("slice 6 lowers tuple literal");
        // Two insertvalue ops on `{i32, i32} undef`, indices 0 and 1.
        assert!(
            ir.contains("insertvalue {i32, i32} undef, i32 1, 0"),
            "expected first insertvalue at index 0; got:\n{ir}"
        );
        assert!(
            ir.contains(", i32 2, 1"),
            "expected second insertvalue at index 1; got:\n{ir}"
        );
    }

    #[test]
    fn unsupported_type_emits_e0810() {
        // Slice 2 supports `&T` / `[T; N]` / `[T]` / `(T1, T2)`, but
        // `access<T>` (Decision #19's nominal pointer) still defers
        // to a later codegen slice — its lowering needs target-
        // specific provenance handling.
        let src = "@fn r(p: access<u32>) -> u32 { return 0u32; }";
        let errors = lower_str(src).expect_err("expected E0810 for access<T>");
        let saw = errors
            .iter()
            .any(|e| matches!(e, CodegenError::NotYetImplemented { what } if *what == "access<T> type"));
        assert!(saw, "expected NotYetImplemented(access); got {errors:?}");
    }

    // ─── Primitive type-mapping table ────────────────────────────────────

    #[test]
    fn all_primitive_types_map_correctly() {
        for (clf_ty, expected_ir) in [
            ("u8", "i8"),
            ("u16", "i16"),
            ("u32", "i32"),
            ("u64", "i64"),
            ("usize", "i64"),
            ("i8", "i8"),
            ("i16", "i16"),
            ("i32", "i32"),
            ("i64", "i64"),
            ("isize", "i64"),
            ("f32", "float"),
            ("f64", "double"),
            ("bool", "i1"),
        ] {
            // Build a minimal source emitting a fn that takes that type.
            let src = format!("@fn p(x: {clf_ty}) {{ return; }}");
            let ir = lower_str(&src).unwrap_or_else(|e| panic!("lowering {clf_ty}: {e:?}"));
            assert!(
                ir.contains(&format!("define void @p({expected_ir} %x)")),
                "expected {clf_ty} → {expected_ir}; got:\n{ir}"
            );
        }
    }

    // ─── Hex / binary literal lowering ───────────────────────────────────

    #[test]
    fn hex_literal_lowers_to_decimal() {
        // 0xFF should appear as 255 in the IR text.
        let ir =
            lower_str("@fn h() -> u32 { return 0xFFu32; }").expect("lower hex");
        assert!(ir.contains("ret i32 255"), "expected hex→255; got:\n{ir}");
    }

    #[test]
    fn binary_literal_lowers_to_decimal() {
        let ir = lower_str("@fn b() -> u32 { return 0b1010u32; }").expect("lower bin");
        assert!(ir.contains("ret i32 10"), "expected bin→10; got:\n{ir}");
    }

    // ─── Determinism / shape ─────────────────────────────────────────────

    #[test]
    fn same_input_same_output() {
        let src = "@fn add(a: u32, b: u32) -> u32 { return a + b; }";
        let a = lower_str(src).expect("lower 1");
        let b = lower_str(src).expect("lower 2");
        assert_eq!(a, b, "codegen output must be deterministic");
    }

    #[test]
    fn snapshot_canonical_add_fn() {
        // Canonical example: a 2-arg `add` fn. Locks the IR shape so
        // any unintentional change to the emitter surfaces as a
        // diff. (Snapshot-style; if the emitter's whitespace / labels
        // change deliberately, update the expected text.)
        let ir = lower_str("@fn add(a: u32, b: u32) -> u32 { return a + b; }")
            .expect("lower add");
        let expected = concat!(
            "; ModuleID = 'test'\n",
            "source_filename = \"test\"\n",
            "\n",
            "define i32 @add(i32 %a, i32 %b) {\n",
            "entry:\n",
            "  %tmp.0 = add i32 %a, %b\n",
            "  ret i32 %tmp.0\n",
            "}\n",
            "\n",
        );
        assert_eq!(ir, expected, "IR shape changed; got:\n{ir}");
    }

    #[test]
    fn snapshot_canonical_call_chain() {
        // Slightly bigger: caller invokes callee with a let in
        // between. Verifies the per-fn SSA-ID counter resets and
        // calls render correctly.
        let src = "\
            @fn double(x: u32) -> u32 { return x; }\n\
            @fn caller(n: u32) -> u32 { let _y: u32 = double(n); return _y; }\n\
        ";
        let ir = lower_str(src).expect("lower call chain");
        // Verify both fns are present in order.
        let double_pos = ir.find("define i32 @double").expect("missing @double");
        let caller_pos = ir.find("define i32 @caller").expect("missing @caller");
        assert!(double_pos < caller_pos, "fn order should match source");
        // Verify caller calls double with the right arg/return types.
        assert!(
            ir.contains("call i32 @double(i32 %n)"),
            "expected typed call site; got:\n{ir}"
        );
        // Verify the let binding goes through the SSA-add identity.
        assert!(
            ir.contains("add i32 0,"),
            "expected SSA-bind via add 0,...; got:\n{ir}"
        );
        // Per-fn SSA reset: each fn starts at %tmp.0.
        let double_tmp_zero = ir.matches("%tmp.0 ").count();
        assert!(
            double_tmp_zero >= 1,
            "expected %tmp.0 in at least one fn; got:\n{ir}"
        );
    }

    // ─── Slice 2: typing integration + sign-aware ops + composites ───────

    #[test]
    fn s2_signed_div_uses_sdiv() {
        // i32 div should use `sdiv`, not `udiv` (the slice-1 default).
        let ir = lower_str("@fn d(a: i32, b: i32) -> i32 { return a / b; }")
            .expect("lower signed div");
        assert!(ir.contains("sdiv i32"), "expected sdiv; got:\n{ir}");
        assert!(!ir.contains("udiv i32"), "should not have udiv; got:\n{ir}");
    }

    #[test]
    fn s2_signed_rem_uses_srem() {
        let ir = lower_str("@fn r(a: i32, b: i32) -> i32 { return a % b; }")
            .expect("lower signed rem");
        assert!(ir.contains("srem i32"), "expected srem; got:\n{ir}");
    }

    #[test]
    fn s2_unsigned_div_still_uses_udiv() {
        // Sanity check: u32 still picks `udiv` post-T2.
        let ir = lower_str("@fn d(a: u32, b: u32) -> u32 { return a / b; }")
            .expect("lower unsigned div");
        assert!(ir.contains("udiv i32"), "expected udiv; got:\n{ir}");
    }

    #[test]
    fn s2_unary_neg_int() {
        // `-x` for i32 lowers to `sub i32 0, x`.
        let ir = lower_str("@fn n(x: i32) -> i32 { return -x; }").expect("lower neg");
        assert!(ir.contains("sub i32 0, %x"), "expected sub 0,x; got:\n{ir}");
    }

    #[test]
    fn s2_unary_not_bool() {
        let ir = lower_str("@fn no(b: bool) -> bool { return !b; }").expect("lower not");
        assert!(ir.contains("xor i1 %b, true"), "expected xor i1; got:\n{ir}");
    }

    #[test]
    fn s2_unary_bitnot_int() {
        let ir = lower_str("@fn bn(x: u32) -> u32 { return ~x; }").expect("lower bitnot");
        assert!(ir.contains("xor i32 %x, -1"), "expected xor -1; got:\n{ir}");
    }

    #[test]
    fn s2_ref_type_in_signature() {
        // `&T` lowers as `T*`. Slice 2 supports this in the signature.
        let ir = lower_str("@fn r(p: &u32) -> u32 { return 0u32; }")
            .expect("lower ref signature");
        assert!(
            ir.contains("define i32 @r(i32* %p)"),
            "expected i32* param; got:\n{ir}"
        );
    }

    #[test]
    fn s2_ref_mut_type_in_signature() {
        // `&mut T` also lowers as `T*` (mutability-as-attribute is
        // a future slice; the IR-type form is the same).
        let ir = lower_str("@fn r(p: &mut u32) -> u32 { return 0u32; }")
            .expect("lower &mut signature");
        assert!(
            ir.contains("define i32 @r(i32* %p)"),
            "expected i32* (mut as attr later); got:\n{ir}"
        );
    }

    #[test]
    fn s2_array_type_in_signature() {
        let ir = lower_str("@fn a(buf: [u8; 64]) { return; }")
            .expect("lower array signature");
        assert!(
            ir.contains("define void @a([64 x i8] %buf)"),
            "expected [64 x i8] param; got:\n{ir}"
        );
    }

    #[test]
    fn s2_tuple_type_in_signature() {
        let ir = lower_str("@fn t(p: (u32, bool)) { return; }")
            .expect("lower tuple signature");
        assert!(
            ir.contains("define void @t({i32, i1} %p)"),
            "expected {{i32, i1}} param; got:\n{ir}"
        );
    }

    #[test]
    fn s2_deref_loads_through_ref() {
        // `*p` for `p: &u32` lowers to `load i32, i32* %p`.
        let ir = lower_str("@fn d(p: &u32) -> u32 { return *p; }")
            .expect("lower deref");
        assert!(
            ir.contains("load i32, i32* %p"),
            "expected typed load; got:\n{ir}"
        );
    }

    #[test]
    fn s2_typing_path_lookup_uses_recorded_type() {
        // A `let x: u8 = …` binding has IR type `i8`. When `x` is
        // used in path position, slice 2's typing lookup picks `i8`
        // (slice 1 would have defaulted to i32). Verify by checking
        // that the SSA-bind add uses i8.
        let ir = lower_str("@fn f(a: u8) -> u8 { let _x: u8 = a; return _x; }")
            .expect("lower typed let");
        assert!(
            ir.contains("add i8 0, %a"),
            "expected SSA-bind via add i8; got:\n{ir}"
        );
        assert!(
            ir.contains("ret i8 %tmp."),
            "expected ret i8 of bound value; got:\n{ir}"
        );
    }

    #[test]
    fn s2_call_return_type_via_typing() {
        // The callee returns `bool`. Slice 1 always picked `i32` for
        // call return types; slice 2 reads the typing map and picks
        // the right type.
        let src = "\
            @fn returns_bool() -> bool { return true; }\n\
            @fn caller() -> bool { return returns_bool(); }\n\
        ";
        let ir = lower_str(src).expect("lower bool-returning call");
        // Check the call site uses `i1` for the return.
        assert!(
            ir.contains("call i1 @returns_bool()"),
            "expected `call i1` for bool-returning fn; got:\n{ir}"
        );
    }

    #[test]
    fn s2_signed_division_with_signed_param() {
        // The lhs is i64; div should be sdiv.
        let ir = lower_str("@fn d(a: i64, b: i64) -> i64 { return a / b; }")
            .expect("lower i64 div");
        assert!(ir.contains("sdiv i64"), "expected sdiv i64; got:\n{ir}");
    }

    #[test]
    fn s2_isize_treated_as_signed() {
        // isize lowers to i64 and is signed → sdiv.
        let ir = lower_str("@fn d(a: isize, b: isize) -> isize { return a / b; }")
            .expect("lower isize div");
        assert!(ir.contains("sdiv i64"), "expected sdiv i64; got:\n{ir}");
    }

    #[test]
    fn s2_usize_treated_as_unsigned() {
        let ir = lower_str("@fn d(a: usize, b: usize) -> usize { return a / b; }")
            .expect("lower usize div");
        assert!(ir.contains("udiv i64"), "expected udiv i64; got:\n{ir}");
    }

    // ─── Slice 3: automaton state structs + effects + transitions ────────

    #[test]
    fn s3_automaton_state_struct_emitted() {
        let ir = lower_str("#automaton Counter { value: u32; }\n").expect("lower automaton");
        assert!(
            ir.contains("%struct.Counter = type { i32 }"),
            "expected state struct; got:\n{ir}"
        );
        assert!(
            ir.contains("@Counter.state = global %struct.Counter zeroinitializer"),
            "expected zero-initialised global; got:\n{ir}"
        );
    }

    #[test]
    fn s3_multi_field_struct_layout() {
        let src = "#automaton Multi { a: u32; b: bool; c: u8; }\n";
        let ir = lower_str(src).expect("lower multi-field");
        // Fields appear in declaration order.
        assert!(
            ir.contains("%struct.Multi = type { i32, i1, i8 }"),
            "expected ordered field layout; got:\n{ir}"
        );
    }

    #[test]
    fn s3_register_block_automaton_skipped() {
        // `#address: 0x…` marks register-block — slice 3 doesn't emit
        // a state struct for it (volatile-load/store lowering is
        // slice 4 work).
        let src = "#automaton Mmio { ctrl: u32 #offset: 0x00; #address: 0x4000_0000; }\n";
        let ir = lower_str(src).expect("lower register block");
        assert!(
            !ir.contains("%struct.Mmio"),
            "register-block should be skipped in slice 3; got:\n{ir}"
        );
        assert!(
            !ir.contains("@Mmio.state"),
            "register-block should not emit a global; got:\n{ir}"
        );
    }

    #[test]
    fn s3_effect_lowers_to_define() {
        let src = "\
            #automaton Counter { value: u32; }\n\
            #effect tick() #mutates: [Counter] { return; }\n\
        ";
        let ir = lower_str(src).expect("lower effect");
        assert!(
            ir.contains("define void @tick()"),
            "expected effect fn; got:\n{ir}"
        );
    }

    #[test]
    fn s3_mutate_short_eq_lowers_to_gep_store() {
        let src = "\
            #automaton Counter { value: u32; }\n\
            #effect set_to_five() #mutates: [Counter] { Counter.value = 5u32; }\n\
        ";
        let ir = lower_str(src).expect("lower mutate-short");
        assert!(
            ir.contains("getelementptr %struct.Counter, %struct.Counter* @Counter.state, i32 0, i32 0"),
            "expected GEP at field 0; got:\n{ir}"
        );
        assert!(
            ir.contains("store i32 5, i32* "),
            "expected typed store; got:\n{ir}"
        );
    }

    #[test]
    fn s3_mutate_short_compound_load_op_store() {
        let src = "\
            #automaton Counter { value: u32; }\n\
            #effect inc() #mutates: [Counter] { Counter.value += 1u32; }\n\
        ";
        let ir = lower_str(src).expect("lower compound mutate");
        // Should be load + add + store.
        assert!(
            ir.contains("getelementptr %struct.Counter"),
            "expected GEP; got:\n{ir}"
        );
        assert!(
            ir.matches("load i32, i32* ").count() >= 1,
            "expected load before op; got:\n{ir}"
        );
        assert!(
            ir.contains("add i32"),
            "expected add for +=; got:\n{ir}"
        );
        assert!(
            ir.contains("store i32"),
            "expected store after op; got:\n{ir}"
        );
    }

    #[test]
    fn s3_mutate_block_form() {
        // `#mutate Counter { value = …, status = … };` — block form
        // with multiple field assignments.
        let src = "\
            #automaton Counter { value: u32; flag: bool; }\n\
            #effect setup() #mutates: [Counter] {\n  \
              #mutate Counter { value = 7u32, flag = true };\n\
            }\n\
        ";
        let ir = lower_str(src).expect("lower mutate block");
        // Two GEPs + two stores.
        assert!(
            ir.matches("getelementptr %struct.Counter").count() >= 2,
            "expected GEPs for both field assigns; got:\n{ir}"
        );
        assert!(
            ir.contains("store i32 7, i32* "),
            "expected store i32 7; got:\n{ir}"
        );
        assert!(
            ir.contains("store i1 1, i1* ") || ir.contains("store i1 true, i1* "),
            "expected store i1 for flag; got:\n{ir}"
        );
    }

    #[test]
    fn s3_field_read_in_effect_body() {
        let src = "\
            #automaton Counter { value: u32; }\n\
            #effect read() #mutates: [Counter] {\n  \
              let _v: u32 = Counter.value;\n  \
              return;\n\
            }\n\
        ";
        let ir = lower_str(src).expect("lower field read");
        assert!(
            ir.contains("getelementptr %struct.Counter"),
            "expected GEP for read; got:\n{ir}"
        );
        assert!(
            ir.contains("load i32, i32* "),
            "expected typed load; got:\n{ir}"
        );
    }

    #[test]
    fn s3_transition_lowers_to_namespaced_fn() {
        let src = "\
            #automaton Counter { value: u32;\n  \
              #transition tick { Counter.value = 1u32; }\n\
            }\n\
        ";
        let ir = lower_str(src).expect("lower transition");
        assert!(
            ir.contains("define void @Counter_tick()"),
            "expected namespaced transition fn; got:\n{ir}"
        );
    }

    #[test]
    fn s3_self_field_read_in_transition() {
        // `Self.value` (read position, expression) inside a
        // transition resolves to the owner's field. Mutation-sugar
        // (`Self.value = …;` statement) requires the full automaton
        // name per the parser; that's exercised in
        // `s3_mutate_short_eq_lowers_to_gep_store` instead.
        let src = "\
            #automaton Counter { value: u32;\n  \
              #transition double {\n    \
                let _v: u32 = Self.value;\n    \
                Counter.value = _v;\n  \
              }\n\
            }\n\
        ";
        let ir = lower_str(src).expect("lower Self");
        assert!(
            ir.contains("getelementptr %struct.Counter"),
            "expected GEP for Self.value; got:\n{ir}"
        );
        // Both a load (for the read) and a store (for the write).
        assert!(
            ir.contains("load i32, i32*"),
            "expected load i32 for read; got:\n{ir}"
        );
        assert!(
            ir.contains("store i32"),
            "expected store i32 for write; got:\n{ir}"
        );
    }

    #[test]
    fn s3_proc_call_to_effect_uses_bare_name() {
        let src = "\
            #automaton C { v: u32; }\n\
            #effect helper() #mutates: [C] { return; }\n\
            #effect main() #mutates: [C] { #> helper(); }\n\
        ";
        let ir = lower_str(src).expect("lower proc call");
        assert!(
            ir.contains("call void @helper()"),
            "expected bare-name call to effect; got:\n{ir}"
        );
    }

    #[test]
    fn s3_proc_call_to_transition_uses_namespaced_name() {
        let src = "\
            #automaton Counter {\n  \
              value: u32;\n  \
              #transition tick { Counter.value = 1u32; }\n  \
              #transition twice { #> tick(); #> tick(); }\n\
            }\n\
        ";
        let ir = lower_str(src).expect("lower transition-to-transition");
        assert!(
            ir.contains("call void @Counter_tick()"),
            "expected namespaced call from one transition to another; got:\n{ir}"
        );
    }

    #[test]
    fn s3_interrupt_emits_define_with_source_name() {
        // Slice 3 emits the interrupt as a regular `define` with the
        // source name as the linker symbol. Section attribute and
        // calling convention are slice 4.
        let src = "\
            #automaton T { x: u32; }\n\
            #interrupt SysTick() #mutates: [T] #priority: HIGH { return; }\n\
        ";
        let ir = lower_str(src).expect("lower interrupt");
        assert!(
            ir.contains("define void @SysTick()"),
            "expected SysTick fn; got:\n{ir}"
        );
    }

    #[test]
    fn s3_register_block_field_access_now_supported_per_slice_4() {
        // Renamed from `..._emits_e0810`. Slice 3 surfaced register-
        // block field access as `NotYetImplemented`; slice 4 lowers
        // it to a volatile load at the absolute MMIO address.
        let src = "\
            #automaton Mmio { #address: 0x4000_0000; ctl: u32 #offset: 0x00; }\n\
            #effect r() #mutates: [Mmio] { let _x: u32 = Mmio.ctl; return; }\n\
        ";
        let ir = lower_str(src).expect("slice 4 lowers register-block reads");
        // 0x40000000 = 1073741824
        assert!(
            ir.contains("load volatile i32, i32* inttoptr (i64 1073741824 to i32*)"),
            "expected volatile load at MMIO address; got:\n{ir}"
        );
    }

    // ─── Slice 4: register-block volatile loads/stores ───────────────────

    #[test]
    fn s4_register_block_field_read_volatile_load() {
        // `Mmio.ctl` for `#automaton Mmio { #address: 0x4000_0000;
        // ctl: u32 #offset: 0x00; }` → volatile load at 0x40000000.
        let src = "\
            #automaton Mmio { #address: 0x4000_0000; ctl: u32 #offset: 0x00; }\n\
            #effect r() #mutates: [Mmio] { let _x: u32 = Mmio.ctl; return; }\n\
        ";
        let ir = lower_str(src).expect("lower mmio read");
        assert!(
            ir.contains("load volatile i32, i32* inttoptr (i64 1073741824 to i32*)"),
            "expected volatile load at 0x40000000; got:\n{ir}"
        );
    }

    #[test]
    fn s4_register_block_field_write_volatile_store() {
        let src = "\
            #automaton Mmio { #address: 0x4000_0000; ctl: u32 #offset: 0x00; }\n\
            #effect w() #mutates: [Mmio] { Mmio.ctl = 5u32; }\n\
        ";
        let ir = lower_str(src).expect("lower mmio write");
        assert!(
            ir.contains("store volatile i32 5, i32* inttoptr (i64 1073741824 to i32*)"),
            "expected volatile store at 0x40000000; got:\n{ir}"
        );
    }

    #[test]
    fn s4_register_block_field_offset_added_to_base() {
        // Field at offset 0x04 on a base of 0x4000_0000 → absolute
        // 0x4000_0004 = 1073741828.
        let src = "\
            #automaton Mmio { #address: 0x4000_0000; status: u32 #offset: 0x04; }\n\
            #effect r() #mutates: [Mmio] { let _s: u32 = Mmio.status; return; }\n\
        ";
        let ir = lower_str(src).expect("lower offset read");
        assert!(
            ir.contains("inttoptr (i64 1073741828 to i32*)"),
            "expected base+offset address; got:\n{ir}"
        );
    }

    #[test]
    fn s4_register_block_no_state_struct_emitted() {
        // Register-block automatons don't get a state struct or
        // global; their fields live at MMIO addresses, not in
        // process memory.
        let src = "#automaton Mmio { #address: 0x4000_0000; ctl: u32 #offset: 0x00; }\n";
        let ir = lower_str(src).expect("lower bare mmio");
        assert!(
            !ir.contains("%struct.Mmio"),
            "register-block should skip state struct; got:\n{ir}"
        );
        assert!(
            !ir.contains("@Mmio.state"),
            "register-block should skip global; got:\n{ir}"
        );
    }

    #[test]
    fn s4_register_block_compound_assign() {
        // `Mmio.ctl |= 0x01u32;` → volatile load + or + volatile
        // store, all at the absolute MMIO address.
        let src = "\
            #automaton Mmio { #address: 0x4000_0000; ctl: u32 #offset: 0x00; }\n\
            #effect set_bit() #mutates: [Mmio] { Mmio.ctl |= 1u32; }\n\
        ";
        let ir = lower_str(src).expect("lower compound mmio");
        assert!(
            ir.contains("load volatile i32, i32* inttoptr (i64 1073741824 to i32*)"),
            "expected volatile load; got:\n{ir}"
        );
        assert!(ir.contains("or i32"), "expected or for |=; got:\n{ir}");
        assert!(
            ir.contains("store volatile i32"),
            "expected volatile store; got:\n{ir}"
        );
    }

    #[test]
    fn s4_register_block_mutate_block_form() {
        // `#mutate Mmio { ctl = 1u32, status = 2u32 };` — each
        // field lowers to its own volatile store at its own address.
        let src = "\
            #automaton Mmio {\n  \
              #address: 0x4000_0000;\n  \
              ctl: u32 #offset: 0x00;\n  \
              status: u32 #offset: 0x04;\n\
            }\n\
            #effect setup() #mutates: [Mmio] { #mutate Mmio { ctl = 1u32, status = 2u32 }; }\n\
        ";
        let ir = lower_str(src).expect("lower mutate-block mmio");
        assert!(
            ir.contains("store volatile i32 1, i32* inttoptr (i64 1073741824 to i32*)"),
            "expected store at base; got:\n{ir}"
        );
        assert!(
            ir.contains("store volatile i32 2, i32* inttoptr (i64 1073741828 to i32*)"),
            "expected store at base+4; got:\n{ir}"
        );
    }

    #[test]
    fn s4_register_block_transition() {
        // Slice 3 punted on register-block transitions; slice 4
        // lowers them. The transition's mutation goes through
        // volatile store.
        let src = "\
            #automaton Mmio {\n  \
              #address: 0x4000_0000;\n  \
              ctl: u32 #offset: 0x00;\n  \
              #transition init { Mmio.ctl = 7u32; }\n\
            }\n\
        ";
        let ir = lower_str(src).expect("lower register-block transition");
        assert!(
            ir.contains("define void @Mmio_init()"),
            "expected namespaced transition fn; got:\n{ir}"
        );
        assert!(
            ir.contains("store volatile i32 7, i32* inttoptr (i64 1073741824 to i32*)"),
            "expected volatile store inside transition; got:\n{ir}"
        );
    }

    // ─── Slice 4: interrupt section attribute ────────────────────────────

    #[test]
    fn s4_interrupt_has_section_interrupts() {
        let src = "\
            #automaton T { c: u32; }\n\
            #interrupt SysTick() #mutates: [T] #priority: HIGH { return; }\n\
        ";
        let ir = lower_str(src).expect("lower interrupt");
        assert!(
            ir.contains("define void @SysTick() section \".interrupts\" {"),
            "expected `section \".interrupts\"` on interrupt; got:\n{ir}"
        );
    }

    #[test]
    fn s4_effect_does_not_get_section_attribute() {
        // Effects (non-interrupts) should NOT carry the
        // `.interrupts` section.
        let src = "\
            #automaton T { c: u32; }\n\
            #effect tick() #mutates: [T] { return; }\n\
        ";
        let ir = lower_str(src).expect("lower effect");
        assert!(
            !ir.contains("section \".interrupts\""),
            "effect should not have interrupts section; got:\n{ir}"
        );
    }

    #[test]
    fn s4_interrupt_with_acquire_fence_combines_correctly() {
        // Interrupt with both section attribute AND fence (Decision
        // #22 combination). Both must coexist on the `define` line +
        // body.
        let src = "\
            #automaton T { c: u32; }\n\
            #interrupt UART1_IRQ() #mutates: [T] #priority: HIGH $ [Acquire] { return; }\n\
        ";
        let ir = lower_str(src).expect("lower interrupt+acquire");
        assert!(
            ir.contains("define void @UART1_IRQ() section \".interrupts\" {"),
            "expected section attribute; got:\n{ir}"
        );
        assert!(
            ir.contains("entry:\n  fence acquire\n"),
            "expected acquire fence in body; got:\n{ir}"
        );
    }

    #[test]
    fn s4_full_mmio_program_lowers_cleanly() {
        // End-to-end smoke: register-block automaton + transition +
        // interrupt that mutates the register block. All the slice-4
        // pieces in one canonical program.
        let src = "\
            #automaton Uart {\n  \
              #address: 0x4000_4000;\n  \
              tx_data: u32 #offset: 0x00;\n  \
              status: u32 #offset: 0x18;\n  \
              #transition send { Uart.tx_data = 65u32; }\n\
            }\n\
            #interrupt USART1_IRQ() #mutates: [Uart] #priority: HIGH { #> send(); }\n\
        ";
        let ir = lower_str(src).expect("lower full mmio program");
        for needle in [
            // No state struct / global for the register-block.
            // (Explicitly check absence below; for `for needle`-loop
            // we just collect positives.)
            "define void @Uart_send()",
            "store volatile i32 65, i32* inttoptr (i64 1073758208 to i32*)",
            "define void @USART1_IRQ() section \".interrupts\" {",
            "call void @Uart_send()",
        ] {
            assert!(ir.contains(needle), "missing `{needle}` in IR; got:\n{ir}");
        }
        assert!(
            !ir.contains("%struct.Uart"),
            "register-block should not emit state struct; got:\n{ir}"
        );
    }

    // ─── Slice 4: parse_address_literal helper ───────────────────────────

    #[test]
    fn s4_parse_address_literal_hex() {
        assert_eq!(parse_address_literal("0x4000_0000"), Some(0x4000_0000));
        assert_eq!(parse_address_literal("0xFF"), Some(255));
        assert_eq!(parse_address_literal("0X1A"), Some(26)); // uppercase prefix
    }

    #[test]
    fn s4_parse_address_literal_decimal() {
        assert_eq!(parse_address_literal("42"), Some(42));
        assert_eq!(parse_address_literal("1_000"), Some(1000));
    }

    #[test]
    fn s4_parse_address_literal_binary() {
        assert_eq!(parse_address_literal("0b1010"), Some(10));
    }

    #[test]
    fn s4_parse_address_literal_malformed_returns_none() {
        assert_eq!(parse_address_literal("0xZZ"), None);
        assert_eq!(parse_address_literal("not_a_number"), None);
    }

    // ─── Slice 5: indexed field read / write on automaton arrays ─────────

    #[test]
    fn s5_array_element_ir_type_extracts_element() {
        // Plain primitive arrays.
        assert_eq!(
            array_element_ir_type("[64 x i8]").as_deref(),
            Some("i8"),
            "element of [64 x i8] should be i8"
        );
        assert_eq!(
            array_element_ir_type("[16 x i32]").as_deref(),
            Some("i32"),
            "element of [16 x i32] should be i32"
        );
    }

    #[test]
    fn s5_array_element_ir_type_handles_nested_arrays() {
        // Nested array element type contains spaces; the helper splits
        // on the FIRST ` x ` only so the inner array survives intact.
        assert_eq!(
            array_element_ir_type("[4 x [4 x i8]]").as_deref(),
            Some("[4 x i8]"),
            "nested array element should preserve inner array"
        );
    }

    #[test]
    fn s5_array_element_ir_type_returns_none_for_non_array() {
        assert_eq!(array_element_ir_type("i32"), None);
        assert_eq!(array_element_ir_type("i8*"), None);
        assert_eq!(array_element_ir_type("%struct.Counter"), None);
        assert_eq!(array_element_ir_type(""), None);
    }

    #[test]
    fn s5_indexed_read_on_struct_field_emits_two_level_gep_and_load() {
        // `Counter.buf[3]` on a non-register-block automaton: GEP to
        // the array field, GEP to the element, plain load.
        let src = "\
            #automaton Counter { buf: [u8; 64]; }\n\
            #effect peek() #mutates: [Counter] { let _x: u8 = Counter.buf[3u32]; return; }\n\
        ";
        let ir = lower_str(src).expect("lower indexed read");
        // Stage 1: GEP into struct to grab the field pointer.
        assert!(
            ir.contains("getelementptr %struct.Counter, %struct.Counter* @Counter.state, i32 0, i32 0"),
            "expected struct-field GEP; got:\n{ir}"
        );
        // Stage 2: GEP into the array using the index.
        assert!(
            ir.contains("getelementptr [64 x i8], [64 x i8]*"),
            "expected array-element GEP on [64 x i8]; got:\n{ir}"
        );
        // Stage 3: plain (non-volatile) load of the element type.
        assert!(
            ir.contains("load i8, i8*"),
            "expected plain load i8; got:\n{ir}"
        );
        // Should NOT be volatile for non-register-block.
        assert!(
            !ir.contains("load volatile i8"),
            "non-register-block read must not be volatile; got:\n{ir}"
        );
    }

    #[test]
    fn s5_indexed_read_on_register_block_emits_inttoptr_gep_and_volatile_load() {
        // Register-block array field: skip the struct-field GEP (use
        // `inttoptr` of the absolute MMIO address instead), then GEP
        // into the array, then volatile load.
        let src = "\
            #automaton Uart {\n  \
              #address: 0x4000_4000;\n  \
              fifo: [u8; 16] #offset: 0x20;\n\
            }\n\
            #effect read_byte() #mutates: [Uart] { let _b: u8 = Uart.fifo[2u32]; return; }\n\
        ";
        let ir = lower_str(src).expect("lower mmio array read");
        // 0x4000_4000 + 0x20 = 0x4000_4020 = 1073758240.
        assert!(
            ir.contains("inttoptr (i64 1073758240 to [16 x i8]*)"),
            "expected inttoptr to array type at base+offset; got:\n{ir}"
        );
        assert!(
            ir.contains("getelementptr [16 x i8], [16 x i8]*"),
            "expected array-element GEP; got:\n{ir}"
        );
        assert!(
            ir.contains("load volatile i8, i8*"),
            "MMIO array read must be volatile load; got:\n{ir}"
        );
    }

    #[test]
    fn s5_indexed_write_in_mutate_block_emits_two_level_gep_and_store() {
        // `#mutate Counter { buf[3] = 5u8 };` — block form is the
        // canonical surface for indexed assignment.
        let src = "\
            #automaton Counter { buf: [u8; 64]; }\n\
            #effect poke() #mutates: [Counter] { #mutate Counter { buf[3u32] = 5u8 }; }\n\
        ";
        let ir = lower_str(src).expect("lower indexed write");
        assert!(
            ir.contains("getelementptr %struct.Counter, %struct.Counter* @Counter.state, i32 0, i32 0"),
            "expected struct-field GEP for write; got:\n{ir}"
        );
        assert!(
            ir.contains("getelementptr [64 x i8], [64 x i8]*"),
            "expected array-element GEP for write; got:\n{ir}"
        );
        assert!(
            ir.contains("store i8 5, i8*"),
            "expected plain store i8; got:\n{ir}"
        );
        assert!(
            !ir.contains("store volatile i8"),
            "non-register-block write must not be volatile; got:\n{ir}"
        );
    }

    #[test]
    fn s5_indexed_write_on_register_block_emits_volatile_store() {
        // `#mutate Uart { fifo[2] = 0xAAu8 };` on a register-block
        // automaton: inttoptr → array GEP → volatile store.
        let src = "\
            #automaton Uart {\n  \
              #address: 0x4000_4000;\n  \
              fifo: [u8; 16] #offset: 0x20;\n\
            }\n\
            #effect tx() #mutates: [Uart] { #mutate Uart { fifo[2u32] = 65u8 }; }\n\
        ";
        let ir = lower_str(src).expect("lower mmio array write");
        assert!(
            ir.contains("inttoptr (i64 1073758240 to [16 x i8]*)"),
            "expected inttoptr at base+offset; got:\n{ir}"
        );
        assert!(
            ir.contains("getelementptr [16 x i8], [16 x i8]*"),
            "expected array-element GEP; got:\n{ir}"
        );
        assert!(
            ir.contains("store volatile i8 65, i8*"),
            "MMIO array write must be volatile store; got:\n{ir}"
        );
    }

    #[test]
    fn s5_indexed_field_in_transition_uses_self_owner() {
        // Inside a `#transition` body, `Self.buf[0u32]` resolves the
        // owner automaton from the enclosing context, not from the
        // path. Verifies the `Self` → enclosing-owner branch in
        // `emit_index_expr`.
        let src = "\
            #automaton Counter {\n  \
              buf: [u8; 4];\n  \
              #transition init { #mutate Counter { buf[0u32] = 1u8 }; }\n\
            }\n\
        ";
        let ir = lower_str(src).expect("lower transition with indexed write");
        // Transition body must touch the right struct + global.
        assert!(
            ir.contains("define void @Counter_init()"),
            "expected mangled transition fn; got:\n{ir}"
        );
        assert!(
            ir.contains("getelementptr %struct.Counter, %struct.Counter* @Counter.state, i32 0, i32 0"),
            "expected struct-field GEP for buf; got:\n{ir}"
        );
        assert!(
            ir.contains("store i8 1, i8*"),
            "expected store of 1 into buf; got:\n{ir}"
        );
    }

    #[test]
    fn s5_indexed_write_alongside_plain_writes_in_same_mutate_block() {
        // Mixing indexed and non-indexed assigns in one `#mutate`
        // block. Each assignment lowers independently per the
        // `fa.index.is_some()` dispatch.
        let src = "\
            #automaton T { count: u32; buf: [u8; 8]; }\n\
            #effect setup() #mutates: [T] {\n  \
              #mutate T { count = 7u32, buf[1u32] = 9u8 };\n\
            }\n\
        ";
        let ir = lower_str(src).expect("lower mixed mutate");
        // Plain field write: store directly through struct-field GEP.
        assert!(
            ir.contains("store i32 7, i32*"),
            "expected plain store of count; got:\n{ir}"
        );
        // Indexed field write: array-element GEP then store.
        assert!(
            ir.contains("getelementptr [8 x i8], [8 x i8]*"),
            "expected array-element GEP for buf; got:\n{ir}"
        );
        assert!(
            ir.contains("store i8 9, i8*"),
            "expected store i8 9 for buf[1]; got:\n{ir}"
        );
    }

    // ─── Slice 6: tuple / array / array-repeat literal expressions ───────

    #[test]
    fn s6_const_int_count_parses_decimal_hex_binary() {
        use clifford_ast::{Expr, ExprKind};
        use clifford_lexer::Span;
        let span = Span::new(0, 0);
        let dec = Expr {
            kind: ExprKind::IntLit("64u32".to_owned()),
            span,
        };
        let hex = Expr {
            kind: ExprKind::HexLit("0x10u32".to_owned()),
            span,
        };
        let bin = Expr {
            kind: ExprKind::BinLit("0b1000u32".to_owned()),
            span,
        };
        assert_eq!(const_int_count(&dec), Some(64));
        assert_eq!(const_int_count(&hex), Some(16));
        assert_eq!(const_int_count(&bin), Some(8));
    }

    #[test]
    fn s6_const_int_count_returns_none_for_non_literal() {
        use clifford_ast::{Expr, ExprKind};
        use clifford_lexer::Span;
        let span = Span::new(0, 0);
        let path = Expr {
            kind: ExprKind::Path(vec!["n".to_owned()]),
            span,
        };
        assert_eq!(const_int_count(&path), None);
    }

    #[test]
    fn s6_tuple_literal_lowers_to_insertvalue_chain() {
        // 3-element tuple of mixed types: (u32, bool, u8). Type
        // becomes `{i32, i1, i8}`. Three insertvalues at indices
        // 0, 1, 2.
        let src = "@fn t() { let _x: (u32, bool, u8) = (5u32, true, 7u8); return; }";
        let ir = lower_str(src).expect("lower 3-tuple");
        assert!(
            ir.contains("insertvalue {i32, i1, i8} undef, i32 5, 0"),
            "expected first insertvalue (i32, idx 0); got:\n{ir}"
        );
        assert!(
            ir.contains(", i1 1, 1"),
            "expected i1 (true) at idx 1; got:\n{ir}"
        );
        assert!(
            ir.contains(", i8 7, 2"),
            "expected i8 7 at idx 2; got:\n{ir}"
        );
    }

    #[test]
    fn s6_two_tuple_with_different_element_types() {
        let src = "@fn p() { let _x: (u32, bool) = (42u32, false); return; }";
        let ir = lower_str(src).expect("lower 2-tuple");
        assert!(
            ir.contains("insertvalue {i32, i1} undef, i32 42, 0"),
            "expected i32 42 at idx 0; got:\n{ir}"
        );
        assert!(
            ir.contains(", i1 0, 1"),
            "expected i1 0 (false) at idx 1; got:\n{ir}"
        );
    }

    #[test]
    fn s6_array_literal_lowers_to_insertvalue_chain() {
        // Three-element u32 array. Type becomes `[3 x i32]`. Three
        // insertvalues at indices 0, 1, 2 with i32 element type.
        let src = "@fn a() { let _x: [u32; 3] = [10u32, 20u32, 30u32]; return; }";
        let ir = lower_str(src).expect("lower 3-array");
        assert!(
            ir.contains("insertvalue [3 x i32] undef, i32 10, 0"),
            "expected first insertvalue [3 x i32] idx 0; got:\n{ir}"
        );
        assert!(
            ir.contains(", i32 20, 1"),
            "expected i32 20 at idx 1; got:\n{ir}"
        );
        assert!(
            ir.contains(", i32 30, 2"),
            "expected i32 30 at idx 2; got:\n{ir}"
        );
    }

    #[test]
    fn s6_array_repeat_literal_const_count() {
        // `[0u8; 4]` → four insertvalues all with the same value.
        let src = "@fn b() { let _x: [u8; 4] = [0u8; 4]; return; }";
        let ir = lower_str(src).expect("lower array repeat");
        // 4 insertvalues at indices 0..=3 — one per element.
        for idx in 0..4 {
            let needle = format!(", i8 0, {idx}");
            assert!(
                ir.contains(&needle),
                "missing insertvalue at idx {idx}; got:\n{ir}"
            );
        }
        // Aggregate type appears verbatim on each insertvalue line.
        assert_eq!(
            ir.matches("insertvalue [4 x i8]").count(),
            4,
            "expected exactly 4 insertvalue ops on [4 x i8]; got:\n{ir}"
        );
    }

    #[test]
    fn s6_array_repeat_with_non_constant_value_emits_value_once() {
        // `[v; 3]` where `v` is an SSA name (not a constant). The
        // value expression is emitted once and reused in every
        // insertvalue. We verify the SSA name appears in three
        // insertvalues.
        let src = "\
            @fn c() {\n  \
              let v: u32 = 0u32 + 1u32;\n  \
              let _arr: [u32; 3] = [v; 3];\n  \
              return;\n\
            }\n\
        ";
        let ir = lower_str(src).expect("lower array repeat with non-const value");
        // 3 insertvalues on [3 x i32].
        assert_eq!(
            ir.matches("insertvalue [3 x i32]").count(),
            3,
            "expected 3 insertvalue ops; got:\n{ir}"
        );
    }

    #[test]
    fn s6_array_repeat_zero_count_emits_nothing() {
        // `[0u8; 0]` is an edge case but a const count of 0 is legal;
        // the resulting aggregate is `[0 x i8] undef` with no
        // insertvalues. We check the chain returns a usable SSA name
        // (or undef directly).
        let src = "@fn d() { let _x: [u8; 0] = [0u8; 0]; return; }";
        let ir = lower_str(src).expect("lower zero-count repeat");
        // No insertvalue should appear; the aggregate is just undef.
        assert_eq!(
            ir.matches("insertvalue [0 x i8]").count(),
            0,
            "expected zero insertvalue ops for [0 x i8; 0]; got:\n{ir}"
        );
    }

    #[test]
    fn s6_array_repeat_non_const_count_returns_e0810() {
        // `[v; n]` where `n` is a runtime variable can't be lowered
        // to a static-count insertvalue chain; surface as
        // NotYetImplemented.
        let src = "\
            @fn e() {\n  \
              let n: u32 = 4u32;\n  \
              let _arr: [u8; 4] = [0u8; n];\n  \
              return;\n\
            }\n\
        ";
        let errors = lower_str(src).expect_err("expected NotYetImplemented");
        let saw = errors.iter().any(|e| matches!(
            e,
            CodegenError::NotYetImplemented { what }
                if *what == "array-repeat count that isn't a const integer literal"
        ));
        assert!(
            saw,
            "expected NotYetImplemented(array-repeat count); got {errors:?}"
        );
    }

    #[test]
    fn s6_nested_tuple_in_array_literal() {
        // `[(1u32, 2u32), (3u32, 4u32)]` — array of tuples.
        // Outer is `[2 x {i32, i32}]`; each element is a tuple
        // value built by its own insertvalue chain.
        let src = "\
            @fn f() {\n  \
              let _x: [(u32, u32); 2] = [(1u32, 2u32), (3u32, 4u32)];\n  \
              return;\n\
            }\n\
        ";
        let ir = lower_str(src).expect("lower nested tuple-in-array");
        // Two outer insertvalues on [2 x {i32, i32}] aggregate.
        assert_eq!(
            ir.matches("insertvalue [2 x {i32, i32}]").count(),
            2,
            "expected 2 outer insertvalues on array; got:\n{ir}"
        );
        // Four inner insertvalues on {i32, i32} (two per tuple).
        assert_eq!(
            ir.matches("insertvalue {i32, i32}").count(),
            4,
            "expected 4 inner tuple insertvalues; got:\n{ir}"
        );
    }

    // ─── Slice 7: integer cast expressions (trunc / sext / zext) ─────────

    #[test]
    fn s7_int_bits_table() {
        assert_eq!(int_bits("i1"), Some(1));
        assert_eq!(int_bits("i8"), Some(8));
        assert_eq!(int_bits("i16"), Some(16));
        assert_eq!(int_bits("i32"), Some(32));
        assert_eq!(int_bits("i64"), Some(64));
        assert_eq!(int_bits("i128"), Some(128));
    }

    #[test]
    fn s7_int_bits_none_for_non_integer() {
        assert_eq!(int_bits("void"), None);
        assert_eq!(int_bits("i32*"), None);
        assert_eq!(int_bits("[8 x i8]"), None);
        assert_eq!(int_bits("{i32, i1}"), None);
        assert_eq!(int_bits("float"), None);
        assert_eq!(int_bits(""), None);
    }

    #[test]
    fn s7_widening_unsigned_emits_zext() {
        // `5u8 as u32` — zero-extend i8 → i32.
        let src = "@fn w() -> u32 { return 5u8 as u32; }";
        let ir = lower_str(src).expect("lower zext widen");
        assert!(
            ir.contains("zext i8 5 to i32"),
            "expected zext i8 -> i32; got:\n{ir}"
        );
        // No sext should appear for an unsigned source.
        assert!(
            !ir.contains("sext i8"),
            "unsigned source must not use sext; got:\n{ir}"
        );
    }

    #[test]
    fn s7_widening_signed_emits_sext() {
        // `-3i8 as i32` — sign-extend i8 → i32. We need an i8 SSA
        // value so the unary minus runs first; then the cast applies.
        let src = "@fn w() -> i32 { let v: i8 = -3i8; return v as i32; }";
        let ir = lower_str(src).expect("lower sext widen");
        assert!(
            ir.contains("sext i8"),
            "expected sext for signed source; got:\n{ir}"
        );
        assert!(
            ir.contains(" to i32"),
            "expected widening to i32; got:\n{ir}"
        );
    }

    #[test]
    fn s7_narrowing_emits_trunc() {
        // `5u32 as u8` — truncate i32 → i8. Narrowing is unsigned/signed
        // agnostic at the IR opcode level (always `trunc`).
        let src = "@fn n() -> u8 { return 5u32 as u8; }";
        let ir = lower_str(src).expect("lower trunc");
        assert!(
            ir.contains("trunc i32 5 to i8"),
            "expected trunc i32 -> i8; got:\n{ir}"
        );
    }

    #[test]
    fn s7_same_type_cast_is_noop() {
        // `5u32 as u32` — no instruction emitted; the SSA value is
        // returned as-is.
        let src = "@fn s() -> u32 { return 5u32 as u32; }";
        let ir = lower_str(src).expect("lower noop cast");
        assert!(
            !ir.contains("zext"),
            "same-type cast should not emit zext; got:\n{ir}"
        );
        assert!(
            !ir.contains("sext"),
            "same-type cast should not emit sext; got:\n{ir}"
        );
        assert!(
            !ir.contains("trunc"),
            "same-type cast should not emit trunc; got:\n{ir}"
        );
        // The literal `5` is returned directly.
        assert!(
            ir.contains("ret i32 5"),
            "expected direct return of 5; got:\n{ir}"
        );
    }

    #[test]
    fn s7_bool_to_int_emits_zext() {
        // `true as u32` — zext i1 → i32; bool is always treated as
        // unsigned for widening.
        let src = "@fn b() -> u32 { return true as u32; }";
        let ir = lower_str(src).expect("lower bool widen");
        assert!(
            ir.contains("zext i1 1 to i32"),
            "expected zext i1 (true) -> i32; got:\n{ir}"
        );
    }

    #[test]
    fn s7_chained_cast_widening_then_narrowing() {
        // `(5u8 as u32) as u16` — zext then trunc.
        let src = "@fn c() -> u16 { return (5u8 as u32) as u16; }";
        let ir = lower_str(src).expect("lower chained cast");
        assert!(ir.contains("zext i8 5 to i32"), "expected zext; got:\n{ir}");
        assert!(
            ir.contains(" to i16"),
            "expected trunc to i16; got:\n{ir}"
        );
        assert_eq!(
            ir.matches("trunc").count(),
            1,
            "expected exactly one trunc; got:\n{ir}"
        );
    }

    #[test]
    fn s7_cast_used_inside_larger_expression() {
        // The cast result feeds a binary op, exercising the SSA-name
        // threading.
        let src = "@fn x() -> u32 { return (5u8 as u32) + 1u32; }";
        let ir = lower_str(src).expect("lower cast in binary");
        assert!(ir.contains("zext i8 5 to i32"), "expected zext; got:\n{ir}");
        assert!(
            ir.contains("add i32"),
            "expected add i32 using cast result; got:\n{ir}"
        );
    }

    #[test]
    fn s7_signed_narrowing_uses_trunc_not_sext() {
        // `-1i32 as i8` — narrowing is `trunc` regardless of source
        // sign; the bits flow through unchanged.
        let src = "@fn n() -> i8 { let v: i32 = -1i32; return v as i8; }";
        let ir = lower_str(src).expect("lower signed narrow");
        assert!(
            ir.contains("trunc i32"),
            "expected trunc on signed narrow; got:\n{ir}"
        );
        assert!(
            ir.contains(" to i8"),
            "expected narrowing to i8; got:\n{ir}"
        );
    }

    // ─── Slice 8: Decision #17 / #19 unsafe primitives ───────────────────

    #[test]
    fn s8_unchecked_load_emits_plain_load() {
        // `#unchecked_load<u32>(p)` where `p: &u32` — load i32 from
        // the i32* parameter. Plain (non-volatile).
        let src = "@fn r(p: &u32) -> u32 { return #unchecked_load<u32>(p); }";
        let ir = lower_str(src).expect("lower unchecked load");
        assert!(
            ir.contains("load i32, i32* %p"),
            "expected plain load i32 from %p; got:\n{ir}"
        );
        assert!(
            !ir.contains("load volatile"),
            "non-volatile load should not be volatile; got:\n{ir}"
        );
    }

    #[test]
    fn s8_volatile_load_emits_volatile_load() {
        let src = "@fn r(p: &u32) -> u32 { return #volatile_load<u32>(p); }";
        let ir = lower_str(src).expect("lower volatile load");
        assert!(
            ir.contains("load volatile i32, i32* %p"),
            "expected volatile load i32; got:\n{ir}"
        );
    }

    #[test]
    fn s8_unchecked_store_emits_plain_store() {
        // `#unchecked_store<u32>(p, 7u32);` — write a constant to
        // the pointer.
        let src = "@fn w(p: &u32) { #unchecked_store<u32>(p, 7u32); return; }";
        let ir = lower_str(src).expect("lower unchecked store");
        assert!(
            ir.contains("store i32 7, i32* %p"),
            "expected plain store i32 7 to %p; got:\n{ir}"
        );
        assert!(
            !ir.contains("store volatile"),
            "non-volatile store should not be volatile; got:\n{ir}"
        );
    }

    #[test]
    fn s8_volatile_store_emits_volatile_store() {
        let src = "@fn w(p: &u32) { #volatile_store<u32>(p, 7u32); return; }";
        let ir = lower_str(src).expect("lower volatile store");
        assert!(
            ir.contains("store volatile i32 7, i32* %p"),
            "expected volatile store i32 7; got:\n{ir}"
        );
    }

    #[test]
    fn s8_unchecked_cast_int_widen_unsigned_emits_zext() {
        let src = "@fn c() -> u32 { return #unchecked_cast<u8, u32>(\"widen for index\", 5u8); }";
        let ir = lower_str(src).expect("lower unchecked cast widen");
        assert!(
            ir.contains("zext i8 5 to i32"),
            "expected zext for unsigned widen; got:\n{ir}"
        );
    }

    #[test]
    fn s8_unchecked_cast_int_widen_signed_emits_sext() {
        let src = "@fn c() -> i32 {\n  \
            let v: i8 = -1i8;\n  \
            return #unchecked_cast<i8, i32>(\"sign extend\", v);\n\
        }";
        let ir = lower_str(src).expect("lower unchecked cast widen signed");
        assert!(
            ir.contains("sext i8"),
            "expected sext for signed widen; got:\n{ir}"
        );
        assert!(ir.contains(" to i32"), "expected widen to i32; got:\n{ir}");
    }

    #[test]
    fn s8_unchecked_cast_int_narrowing_emits_trunc() {
        let src = "@fn c() -> u8 { return #unchecked_cast<u32, u8>(\"narrow\", 5u32); }";
        let ir = lower_str(src).expect("lower unchecked cast narrow");
        assert!(
            ir.contains("trunc i32 5 to i8"),
            "expected trunc; got:\n{ir}"
        );
    }

    #[test]
    fn s8_unchecked_cast_same_type_is_noop() {
        let src = "@fn c() -> u32 { return #unchecked_cast<u32, u32>(\"identity\", 5u32); }";
        let ir = lower_str(src).expect("lower unchecked cast same");
        assert!(
            !ir.contains("zext") && !ir.contains("sext") && !ir.contains("trunc"),
            "same-type unchecked cast should be no-op; got:\n{ir}"
        );
        assert!(ir.contains("ret i32 5"), "expected ret 5; got:\n{ir}");
    }

    #[test]
    fn s8_unchecked_cast_pointer_to_int_emits_ptrtoint() {
        // `&u32 -> u64` via #unchecked_cast: ptrtoint i32* to i64.
        let src = "@fn c(p: &u32) -> u64 { return #unchecked_cast<&u32, u64>(\"addr capture\", p); }";
        let ir = lower_str(src).expect("lower ptrtoint");
        assert!(
            ir.contains("ptrtoint i32* %p to i64"),
            "expected ptrtoint i32* -> i64; got:\n{ir}"
        );
    }

    #[test]
    fn s8_unchecked_offset_emits_getelementptr() {
        // `#unchecked_offset<u32>(p, 4i32)` — gep with element type
        // u32 at signed offset 4.
        let src = "@fn o(p: &u32) -> &u32 {\n  \
            let q: &u32 = #unchecked_offset<u32>(p, 4i32);\n  \
            return q;\n\
        }";
        let ir = lower_str(src).expect("lower unchecked offset");
        assert!(
            ir.contains("getelementptr i32, i32* %p, i32 4"),
            "expected getelementptr i32 i32* %p i32 4; got:\n{ir}"
        );
    }

    #[test]
    fn s8_unchecked_load_inside_binary_op() {
        // Read the pointer, add a constant, return.
        let src = "@fn x(p: &u32) -> u32 {\n  \
            return #unchecked_load<u32>(p) + 1u32;\n\
        }";
        let ir = lower_str(src).expect("lower load+add");
        assert!(
            ir.contains("load i32, i32* %p"),
            "expected load; got:\n{ir}"
        );
        assert!(
            ir.contains("add i32"),
            "expected add using load result; got:\n{ir}"
        );
    }

    #[test]
    fn s8_type_expr_is_signed_int_table() {
        use clifford_ast::{TypeExpr, TypeKind};
        use clifford_lexer::Span;
        let span = Span::new(0, 0);
        let make = |p: PrimitiveType| TypeExpr {
            kind: TypeKind::Primitive(p),
            span,
        };
        for (p, expected) in [
            (PrimitiveType::I8, true),
            (PrimitiveType::I16, true),
            (PrimitiveType::I32, true),
            (PrimitiveType::I64, true),
            (PrimitiveType::Isize, true),
            (PrimitiveType::U8, false),
            (PrimitiveType::U16, false),
            (PrimitiveType::U32, false),
            (PrimitiveType::U64, false),
            (PrimitiveType::Usize, false),
            (PrimitiveType::Bool, false),
        ] {
            assert_eq!(
                type_expr_is_signed_int(&make(p)),
                expected,
                "wrong signedness for {p:?}"
            );
        }
    }

    // ─── Slice 9: multi-state automatons (Decision #5 categorical) ───────

    #[test]
    fn s9_monoid_struct_unchanged() {
        // Sanity: monoid automatons (no `#states` clause) keep their
        // slice-3 struct shape — `{ <user fields> }` with no tag
        // prepended.
        let src = "#automaton C { v: u32; }\n";
        let ir = lower_str(src).expect("lower monoid");
        assert!(
            ir.contains("%struct.C = type { i32 }"),
            "expected single-field struct; got:\n{ir}"
        );
    }

    #[test]
    fn s9_multi_state_struct_prepends_i32_tag() {
        // Multi-state automaton's struct has `i32` (the tag) at
        // index 0, then the user fields.
        let src = "\
            #automaton Counter {\n  \
              #states: [Idle, Counting, Done];\n  \
              count: u32;\n\
            }\n\
        ";
        let ir = lower_str(src).expect("lower multi-state");
        assert!(
            ir.contains("%struct.Counter = type { i32, i32 }"),
            "expected {{ i32, i32 }} (tag + count); got:\n{ir}"
        );
        // Initial state is the first listed; tag 0 = `Idle`. The
        // global stays `zeroinitializer` because `Idle` is tag 0.
        assert!(
            ir.contains("@Counter.state = global %struct.Counter zeroinitializer"),
            "expected zeroinitializer global; got:\n{ir}"
        );
    }

    #[test]
    fn s9_state_read_emits_gep_load_at_index_0() {
        // `Counter@state` lowers to a GEP+load at LLVM struct
        // index 0 with i32 element type.
        let src = "\
            #automaton Counter {\n  \
              #states: [Idle, Active];\n  \
              count: u32;\n\
            }\n\
            #effect read() -> u32 #mutates: [Counter] {\n  \
              let s: u32 = Counter@state;\n  \
              return s;\n\
            }\n\
        ";
        let ir = lower_str(src).expect("lower @state read");
        assert!(
            ir.contains("getelementptr %struct.Counter, %struct.Counter* @Counter.state, i32 0, i32 0"),
            "expected GEP at idx 0 (state tag); got:\n{ir}"
        );
        assert!(
            ir.contains("load i32, i32*"),
            "expected load i32 of tag; got:\n{ir}"
        );
    }

    #[test]
    fn s9_user_field_index_shifts_for_multi_state() {
        // The user's `count: u32` is the FIRST user field but lives
        // at LLVM struct index 1 because the tag occupies index 0.
        let src = "\
            #automaton Counter {\n  \
              #states: [Idle, Active];\n  \
              count: u32;\n\
            }\n\
            #effect bump() #mutates: [Counter] {\n  \
              Counter.count += 1u32;\n\
            }\n\
        ";
        let ir = lower_str(src).expect("lower bump");
        // Read of count goes through idx 1, NOT idx 0.
        assert!(
            ir.contains("getelementptr %struct.Counter, %struct.Counter* @Counter.state, i32 0, i32 1"),
            "expected GEP at idx 1 (user field after tag); got:\n{ir}"
        );
        // Crucially, NO GEP at idx 0 should appear for a field op
        // (that would be the tag, not `count`).
        let count_at_0 = ir.matches("@Counter.state, i32 0, i32 0").count();
        // The tag GEP would only appear if there's a state-read or
        // a transition tag write; this effect has neither.
        assert_eq!(
            count_at_0, 0,
            "did not expect tag GEP for field-only effect; got:\n{ir}"
        );
    }

    #[test]
    fn s9_transition_with_destination_writes_tag_before_ret() {
        // `#transition start -> Counting { ... }` — the destination
        // tag (`Counting` = tag 1) is written via a store before
        // the `ret void`.
        let src = "\
            #automaton Counter {\n  \
              #states: [Idle, Counting];\n  \
              count: u32;\n  \
              #transition start -> Counting { Counter.count = 0u32; }\n\
            }\n\
        ";
        let ir = lower_str(src).expect("lower transition with dest");
        // Tag pointer GEP at idx 0:
        assert!(
            ir.contains("getelementptr %struct.Counter, %struct.Counter* @Counter.state, i32 0, i32 0"),
            "expected tag GEP at idx 0; got:\n{ir}"
        );
        // Tag store i32 1 (Counting):
        assert!(
            ir.contains("store i32 1, i32*"),
            "expected store of tag 1 (Counting); got:\n{ir}"
        );
        // Tag write must precede ret void:
        let tag_pos = ir.find("store i32 1, i32*").expect("tag store missing");
        let ret_pos = ir.find("ret void").expect("ret void missing");
        assert!(
            tag_pos < ret_pos,
            "tag write must come before ret; got:\n{ir}"
        );
    }

    #[test]
    fn s9_transition_without_destination_emits_no_tag_write() {
        // `#transition tick { ... }` — no `-> Dest`, so no tag
        // write should be emitted (state stays the same).
        let src = "\
            #automaton Counter {\n  \
              #states: [Idle, Counting];\n  \
              count: u32;\n  \
              #transition tick { Counter.count += 1u32; }\n\
            }\n\
        ";
        let ir = lower_str(src).expect("lower tickless transition");
        // We expect a GEP at idx 1 (count) — but NOT a store at idx 0
        // (tag). Verify by checking that no tag store happens inside
        // the @Counter_tick body.
        // Easy heuristic: no `store i32 N, i32*` where N matches a
        // tag value (0 or 1) AND the surrounding op is the tag GEP.
        // A simpler check: the only `store` should be for `count`.
        // Tag values are 0 (Idle) and 1 (Counting). The transition
        // body's `+= 1u32` produces `store i32 ..., i32* %tag.0`
        // for the count, so we check for an absence of GEP at idx 0
        // inside the transition.
        // Pull out the @Counter_tick body for inspection:
        let body_start = ir
            .find("define void @Counter_tick()")
            .expect("Counter_tick fn missing");
        let body_end = ir[body_start..]
            .find("\n}\n")
            .map(|p| body_start + p)
            .unwrap_or(ir.len());
        let body = &ir[body_start..body_end];
        assert!(
            !body.contains("@Counter.state, i32 0, i32 0"),
            "tickless transition must not emit tag GEP; got body:\n{body}"
        );
    }

    #[test]
    fn s9_transition_destination_uses_correct_tag_for_third_state() {
        // `#transition finish -> Done` on `[Idle, Counting, Done]`
        // — `Done` is the third state, tag 2.
        let src = "\
            #automaton Counter {\n  \
              #states: [Idle, Counting, Done];\n  \
              count: u32;\n  \
              #transition finish -> Done { return; }\n\
            }\n\
        ";
        let ir = lower_str(src).expect("lower 3-state transition");
        assert!(
            ir.contains("store i32 2, i32*"),
            "expected store of tag 2 (Done = 3rd state); got:\n{ir}"
        );
    }

    #[test]
    fn s9_state_read_on_monoid_returns_e0810() {
        // `Auto@state` on a monoid (no `#states`) is meaningless —
        // we surface it as NotYetImplemented.
        let src = "\
            #automaton C { v: u32; }\n\
            #effect r() -> u32 #mutates: [C] { return C@state; }\n\
        ";
        let errors = lower_str(src).expect_err("expected E0810");
        let saw = errors.iter().any(|e| matches!(
            e,
            CodegenError::NotYetImplemented { what }
                if *what == "Auto@state on a monoid automaton (no `#states` clause)"
        ));
        assert!(
            saw,
            "expected NotYetImplemented(monoid @state); got {errors:?}"
        );
    }

    #[test]
    fn s9_state_read_on_register_block_returns_e0810() {
        // `Auto@state` on a register-block automaton has no defined
        // semantics yet (the tag has no MMIO offset); deferred.
        let src = "\
            #automaton Mmio {\n  \
              #address: 0x4000_0000;\n  \
              #states: [Off, On];\n  \
              ctl: u32 #offset: 0x00;\n\
            }\n\
            #effect r() -> u32 #mutates: [Mmio] { return Mmio@state; }\n\
        ";
        let errors = lower_str(src).expect_err("expected E0810");
        let saw = errors.iter().any(|e| matches!(
            e,
            CodegenError::NotYetImplemented { what }
                if *what == "Auto@state on a register-block automaton"
        ));
        assert!(
            saw,
            "expected NotYetImplemented(register-block @state); got {errors:?}"
        );
    }

    #[test]
    fn s9_full_three_state_program_lowers_cleanly() {
        // End-to-end smoke: 3 states, 2 transitions (one with
        // destination, one without), one state-read.
        let src = "\
            #automaton Counter {\n  \
              #states: [Idle, Counting, Done];\n  \
              count: u32;\n  \
              #transition start -> Counting { Counter.count = 0u32; }\n  \
              #transition finish -> Done { return; }\n\
            }\n\
            #effect bump() #mutates: [Counter] { Counter.count += 1u32; }\n\
            #effect peek() -> u32 #mutates: [Counter] { return Counter@state; }\n\
        ";
        let ir = lower_str(src).expect("lower full 3-state program");
        for needle in [
            "%struct.Counter = type { i32, i32 }",
            "@Counter.state = global %struct.Counter zeroinitializer",
            "define void @Counter_start()",
            "define void @Counter_finish()",
            "store i32 1, i32*", // start -> Counting
            "store i32 2, i32*", // finish -> Done
            "define void @bump()",
            "define i32 @peek()",
        ] {
            assert!(
                ir.contains(needle),
                "missing `{needle}` in IR; got:\n{ir}"
            );
        }
    }

    #[test]
    fn s9_destination_tag_write_combines_with_release_fence() {
        // Decision #22 + slice 9 interaction: a transition with
        // both `-> Dest` and `$ [Release]` should emit the tag
        // write FIRST, then the release fence, then the ret.
        let src = "\
            #automaton C {\n  \
              #states: [A, B];\n  \
              v: u32;\n  \
              #transition flip -> B $ [Release] { return; }\n\
            }\n\
        ";
        let ir = lower_str(src).expect("lower transition with release");
        let tag_pos = ir.find("store i32 1, i32*").expect("tag store missing");
        let fence_pos = ir.find("fence release").expect("release fence missing");
        let ret_pos = ir.find("ret void").expect("ret missing");
        assert!(
            tag_pos < fence_pos && fence_pos < ret_pos,
            "expected order: tag write < fence < ret; got positions {tag_pos} / {fence_pos} / {ret_pos} in:\n{ir}"
        );
    }

    #[test]
    fn s9_field_read_on_multi_state_uses_shifted_index() {
        // Regression test for the bug found by examples/dual_uart_telemetry.cl:
        // emit_field_access (read path) was using the user-field
        // index without applying the slice-9 +1 tag shift, so reads
        // of any user field on a multi-state automaton produced a
        // GEP at the wrong LLVM index. The bug was hidden by
        // s9_user_field_index_shifts_for_multi_state because that
        // test only exercised the WRITE path (`+= 1u32`) and only
        // had a single user field.
        //
        // This test reads the SECOND user field (`bytes_total`,
        // user idx 1, LLVM idx 2) on a multi-state automaton.
        // Pre-fix the IR contained `i32 0, i32 1` (reading bytes_uart1
        // instead of bytes_total). Post-fix: `i32 0, i32 2`.
        let src = "\
            #automaton T {\n  \
              #states: [Empty, NonEmpty];\n  \
              bytes_uart1: u32;\n  \
              bytes_total: u32;\n\
            }\n\
            #effect drain_total() -> u32 #mutates: [T] {\n  \
              return T.bytes_total;\n\
            }\n\
        ";
        let ir = lower_str(src).expect("lower multi-state read");
        // bytes_total is the SECOND user field (user idx 1) on a
        // multi-state automaton; LLVM idx must be 1 + 1 = 2 (state
        // tag occupies idx 0).
        assert!(
            ir.contains("getelementptr %struct.T, %struct.T* @T.state, i32 0, i32 2"),
            "expected read of bytes_total at LLVM idx 2; got:\n{ir}"
        );
        // Critical: the IR must NOT GEP at LLVM idx 1 (that would
        // be reading bytes_uart1 — the slice-9 bug was emitting
        // exactly this).
        assert!(
            !ir.contains("getelementptr %struct.T, %struct.T* @T.state, i32 0, i32 1"),
            "regression: read path must not use unshifted user index; got:\n{ir}"
        );
    }

    #[test]
    fn s9_helper_llvm_field_index_monoid() {
        // Direct unit test on the helper. Monoid: idx 0 -> 0.
        let info = AutomatonInfo {
            name: "M".to_owned(),
            fields: vec![],
            is_register_block: false,
            base_address: 0,
            state_tags: vec![],
            is_staged: false,
            is_audited: false,
        };
        assert_eq!(info.llvm_field_index(0), 0);
        assert_eq!(info.llvm_field_index(3), 3);
        assert!(!info.is_multi_state());
    }

    #[test]
    fn s9_helper_llvm_field_index_multi_state() {
        // Multi-state: idx 0 -> 1, idx 3 -> 4 (the i32 tag occupies
        // LLVM idx 0).
        let info = AutomatonInfo {
            name: "C".to_owned(),
            fields: vec![],
            is_register_block: false,
            base_address: 0,
            state_tags: vec![("A".to_owned(), 0), ("B".to_owned(), 1)],
            is_staged: false,
            is_audited: false,
        };
        assert!(info.is_multi_state());
        assert_eq!(info.llvm_field_index(0), 1);
        assert_eq!(info.llvm_field_index(3), 4);
        assert_eq!(info.state_tag("A"), Some(0));
        assert_eq!(info.state_tag("B"), Some(1));
        assert_eq!(info.state_tag("C"), None);
    }

    // ─── Slice 13: if / else statement form ──────────────────────────────

    #[test]
    fn s13_if_no_else_emits_conditional_branch_to_exit() {
        // `if cond { … }` (no else) — false-edge of br i1 jumps
        // straight to the exit label. Only two basic blocks
        // emitted (then + exit; no else).
        let src = "@fn t() { if true { return; } return; }";
        let ir = lower_str(src).expect("lower if-no-else");
        assert!(
            ir.contains("br i1 1, label %if.then.0, label %if.exit.0"),
            "expected br i1 to then or exit; got:\n{ir}"
        );
        assert!(ir.contains("\nif.then.0:\n"), "missing then label; got:\n{ir}");
        assert!(ir.contains("\nif.exit.0:\n"), "missing exit label; got:\n{ir}");
        // No else label should be emitted.
        assert!(
            !ir.contains("if.else.0:"),
            "no else block: must not emit else label; got:\n{ir}"
        );
    }

    #[test]
    fn s13_if_with_else_emits_three_blocks() {
        // `if cond { … } else { … }` — three labels, br i1 picks
        // between then and else, both jump to exit.
        let src = "\
            @fn t() {\n  \
              if true { let _x: u32 = 1u32; }\n  \
              else { let _y: u32 = 2u32; }\n  \
              return;\n\
            }\n\
        ";
        let ir = lower_str(src).expect("lower if-else");
        assert!(
            ir.contains("br i1 1, label %if.then.0, label %if.else.0"),
            "expected br to then/else; got:\n{ir}"
        );
        for label in [
            "\nif.then.0:\n",
            "\nif.else.0:\n",
            "\nif.exit.0:\n",
        ] {
            assert!(ir.contains(label), "missing {}; got:\n{ir}", label.trim());
        }
        // Both then and else branch to exit (two `br label %exit`).
        assert!(
            ir.matches("br label %if.exit.0").count() >= 2,
            "expected branches from both then and else to exit; got:\n{ir}"
        );
    }

    #[test]
    fn s13_if_condition_uses_dynamic_value() {
        // Boolean condition computed at runtime (binary compare).
        let src = "\
            @fn t(x: u32) {\n  \
              if x < 10u32 { return; }\n  \
              return;\n\
            }\n\
        ";
        let ir = lower_str(src).expect("lower if with dynamic cond");
        // The compare produces an i1 SSA value used by br i1.
        assert!(
            ir.contains("icmp ult i32 %x, 10"),
            "expected icmp ult for `<`; got:\n{ir}"
        );
        // br i1 should reference the SSA name from the icmp.
        assert!(
            ir.contains("br i1 %tmp."),
            "expected br i1 from a fresh-value SSA; got:\n{ir}"
        );
    }

    #[test]
    fn s13_if_then_returns_no_back_edge_emitted() {
        // If the then-block returns, no `br label %if.exit.0`
        // should be emitted from the then block (would be a
        // double terminator).
        let src = "\
            @fn t(x: u32) -> u32 {\n  \
              if true { return x; }\n  \
              return 0u32;\n\
            }\n\
        ";
        let ir = lower_str(src).expect("lower if-with-return");
        // Find the then block content, ensure exactly ONE branch
        // to exit (from the implicit fall-through after the if;
        // the then-block returned so no jump from it).
        // We expect: br to then/exit, then label, ret x, exit
        // label — no `br label %if.exit.0` between ret and exit.
        let then_pos = ir.find("\nif.then.0:\n").unwrap();
        let exit_pos = ir.find("\nif.exit.0:\n").unwrap();
        let between = &ir[then_pos..exit_pos];
        assert!(
            !between.contains("br label %if.exit.0"),
            "then-block returned; must not emit branch to exit; got:\n{between}"
        );
    }

    #[test]
    fn s13_if_else_both_return_exit_block_unreachable() {
        // Both branches return — no edges into the exit block;
        // it's unreachable. Code after the `if` is dead.
        let src = "\
            @fn t() -> u32 {\n  \
              if true { return 1u32; }\n  \
              else { return 2u32; }\n  \
              return 0u32;\n\
            }\n\
        ";
        let ir = lower_str(src).expect("lower if-else-both-return");
        // Both branches have ret without a follow-up branch.
        assert!(
            ir.contains("ret i32 1") && ir.contains("ret i32 2"),
            "expected both rets; got:\n{ir}"
        );
        // No `br label %if.exit.0` should be emitted.
        assert!(
            !ir.contains("br label %if.exit.0"),
            "both branches return; no merge branch should be emitted; got:\n{ir}"
        );
    }

    #[test]
    fn s13_else_if_chain_emits_nested_blocks() {
        // `if a { } else if b { } else { }` — the inner else-if
        // produces its own set of labels with a fresh ID.
        let src = "\
            @fn t() {\n  \
              if true { return; }\n  \
              else if false { return; }\n  \
              else { return; }\n\
            }\n\
        ";
        let ir = lower_str(src).expect("lower else-if chain");
        // Outer if uses ID 0, inner uses ID 1.
        for label in [
            "\nif.then.0:\n",
            "\nif.else.0:\n",
            "\nif.then.1:\n",
            "\nif.else.1:\n",
        ] {
            assert!(
                ir.contains(label),
                "missing {}; got:\n{ir}",
                label.trim()
            );
        }
    }

    #[test]
    fn s13_if_let_inside_branch_invisible_outside() {
        // A `let` inside a then-branch is NOT visible after the
        // if. Resolver enforces this — verifies our scope
        // bracketing is correct.
        let src = "\
            @fn t() -> u32 {\n  \
              if true { let x: u32 = 5u32; }\n  \
              return x;\n\
            }\n\
        ";
        let tokens = tokenize(src).expect("tokenize");
        let program = parse(&tokens).expect("parse");
        let result = resolve(&program);
        assert!(
            result.is_err(),
            "expected resolver to reject `x` after the if branch; got Ok"
        );
    }

    #[test]
    fn s13_if_with_local_assign_in_branch() {
        // Slice 12 + slice 13 interaction: a `let mut` declared
        // outside the if, reassigned inside one branch.
        let src = "\
            @fn t(c: bool) -> u32 {\n  \
              let mut x: u32 = 0u32;\n  \
              if c { x = 1u32; }\n  \
              else { x = 2u32; }\n  \
              return x;\n\
            }\n\
        ";
        let ir = lower_str(src).expect("lower if-with-assign");
        // Two `store i32` for the assigns + 1 for the initial =
        // 3 stores. (One initial + two branch-side stores.)
        assert_eq!(
            ir.matches("store i32").count(),
            3,
            "expected 3 stores; got:\n{ir}"
        );
        // The return at the end loads the final value.
        assert!(
            ir.contains("load i32, i32*"),
            "expected load on return; got:\n{ir}"
        );
    }

    #[test]
    fn s13_nested_if_inside_if() {
        // Nested ifs get distinct label IDs (0 outer, 1 inner).
        let src = "\
            @fn t(a: bool, b: bool) {\n  \
              if a {\n    \
                if b { return; }\n  \
              }\n  \
              return;\n\
            }\n\
        ";
        let ir = lower_str(src).expect("lower nested if");
        assert!(
            ir.contains("\nif.then.0:\n"),
            "missing outer then; got:\n{ir}"
        );
        assert!(
            ir.contains("\nif.then.1:\n"),
            "missing inner then; got:\n{ir}"
        );
        assert!(
            ir.contains("\nif.exit.0:\n"),
            "missing outer exit; got:\n{ir}"
        );
        assert!(
            ir.contains("\nif.exit.1:\n"),
            "missing inner exit; got:\n{ir}"
        );
    }

    #[test]
    fn s13_if_inside_sigma_body_works() {
        // `if` and sigma compose — the inner if uses ID 0 (it's
        // the first label in this fn), the sigma uses ID 1.
        // Actually order of allocation: sigma allocates ID 0
        // for header/body/exit, then the if inside body
        // allocates ID 1.
        let src = "\
            @fn t() {\n  \
              sigma i in 0u32..4u32 {\n    \
                if true { return; }\n  \
              }\n  \
              return;\n\
            }\n\
        ";
        let ir = lower_str(src).expect("lower if-inside-sigma");
        assert!(
            ir.contains("\nsigma.header.0:\n"),
            "missing sigma header; got:\n{ir}"
        );
        assert!(
            ir.contains("\nif.then.1:\n"),
            "missing if.then.1; got:\n{ir}"
        );
        assert!(
            ir.contains("\nif.exit.1:\n"),
            "missing if.exit.1; got:\n{ir}"
        );
    }

    // ─── Slice 12: local mutable re-assignment ───────────────────────────

    #[test]
    fn s12_let_mut_emits_alloca_and_initial_store() {
        // `let mut x: u32 = 5u32;` lowers to `alloca i32` + `store
        // i32 5, i32* %ptr`. Reads of `x` after this point load
        // through the pointer.
        let src = "@fn t() -> u32 { let mut x: u32 = 5u32; return x; }";
        let ir = lower_str(src).expect("lower let-mut");
        // Alloca for the stack slot.
        assert!(
            ir.contains("= alloca i32"),
            "expected alloca i32; got:\n{ir}"
        );
        // Initial store of 5.
        assert!(
            ir.contains("store i32 5, i32*"),
            "expected initial store; got:\n{ir}"
        );
        // Read on `return x` becomes a load.
        assert!(
            ir.contains("load i32, i32*"),
            "expected load on read; got:\n{ir}"
        );
    }

    #[test]
    fn s12_immutable_let_keeps_ssa_direct_lowering() {
        // `let x: u32 = 5u32;` (no `mut`) keeps the slice-1 path:
        // no alloca, no load. The slice-1 `bind_via_identity`
        // helper does emit an `add 0` to give the binding a stable
        // SSA name, so the literal `5` survives as a constant
        // operand of the add (not the literal of the return).
        let src = "@fn t() -> u32 { let x: u32 = 5u32; return x; }";
        let ir = lower_str(src).expect("lower immutable let");
        assert!(
            !ir.contains("alloca"),
            "immutable let should not allocate; got:\n{ir}"
        );
        assert!(
            !ir.contains("load i32"),
            "immutable let should not load; got:\n{ir}"
        );
        // The bind_via_identity SSA name is what `ret` uses; `5`
        // appears as the constant operand of the add.
        assert!(
            ir.contains("add i32 0, 5"),
            "expected bind_via_identity add of 5; got:\n{ir}"
        );
    }

    #[test]
    fn s12_assign_emits_store_to_alloca() {
        // `x = 7u32;` lowers to `store i32 7, i32* <ptr>`.
        let src = "\
            @fn t() -> u32 {\n  \
              let mut x: u32 = 5u32;\n  \
              x = 7u32;\n  \
              return x;\n\
            }\n\
        ";
        let ir = lower_str(src).expect("lower assign");
        // Two stores: initial and assigned. Both go to the same
        // alloca pointer.
        assert!(
            ir.contains("store i32 5, i32*"),
            "expected initial store; got:\n{ir}"
        );
        assert!(
            ir.contains("store i32 7, i32*"),
            "expected assignment store; got:\n{ir}"
        );
        assert_eq!(
            ir.matches("store i32").count(),
            2,
            "expected exactly 2 stores; got:\n{ir}"
        );
    }

    #[test]
    fn s12_accumulator_pattern_in_sigma_body() {
        // Slice 12 + slice 11 interaction: a `let mut` accumulator
        // updated inside a sigma body. This is the canonical
        // "sum of i for i in 0..N" pattern that wasn't expressible
        // pre-slice-12.
        let src = "\
            @fn sum_to_n(n: u32) -> u32 {\n  \
              let mut total: u32 = 0u32;\n  \
              sigma i in 0u32..n {\n    \
                total = total + i;\n  \
              }\n  \
              return total;\n\
            }\n\
        ";
        let ir = lower_str(src).expect("lower accumulator pattern");
        // Total has an alloca.
        assert!(
            ir.contains("alloca i32"),
            "expected alloca for total; got:\n{ir}"
        );
        // Inside the body, total is loaded, added to sigma.i, then
        // stored back.
        assert!(
            ir.contains("load i32, i32*"),
            "expected load of total in body; got:\n{ir}"
        );
        assert!(
            ir.contains("add i32"),
            "expected add for total + i; got:\n{ir}"
        );
        assert!(
            ir.contains("store i32"),
            "expected store back to total; got:\n{ir}"
        );
        // Loop var is referenced inside the body.
        assert!(
            ir.contains("%sigma.i.0"),
            "expected sigma loop var reference; got:\n{ir}"
        );
    }

    #[test]
    fn s12_assign_to_immutable_let_rejected_by_resolver() {
        // `let x = 5; x = 7;` — the resolver should reject this
        // with E0410 (AssignToImmutable).
        let src = "@fn t() { let x: u32 = 5u32; x = 7u32; return; }";
        let tokens = tokenize(src).expect("tokenize");
        let program = parse(&tokens).expect("parse");
        let result = resolve(&program);
        assert!(
            result.is_err(),
            "expected resolver to reject assign-to-immutable"
        );
        if let Err(errs) = result {
            let saw_e0410 = errs.iter().any(|e| {
                let s = format!("{e}");
                s.contains("E0410") && s.contains("immutable")
            });
            assert!(
                saw_e0410,
                "expected E0410 with 'immutable'; got {errs:?}"
            );
        }
    }

    #[test]
    fn s12_assign_to_let_short_rejected_by_resolver() {
        // `let x := 5; x = 7;` — short binding is always
        // immutable; resolver rejects re-assignment.
        let src = "@fn t() { let x := 5u32; x = 7u32; return; }";
        let tokens = tokenize(src).expect("tokenize");
        let program = parse(&tokens).expect("parse");
        let result = resolve(&program);
        assert!(
            result.is_err(),
            "expected resolver to reject assign-to-let-short"
        );
    }

    #[test]
    fn s12_assign_to_param_rejected_by_resolver() {
        // Parameters are immutable from the body's perspective.
        let src = "@fn t(x: u32) { x = 7u32; return; }";
        let tokens = tokenize(src).expect("tokenize");
        let program = parse(&tokens).expect("parse");
        let result = resolve(&program);
        assert!(
            result.is_err(),
            "expected resolver to reject assign-to-param"
        );
    }

    #[test]
    fn s12_assign_to_sigma_var_rejected_by_resolver() {
        // sigma loop variables are immutable.
        let src = "\
            @fn t() {\n  \
              sigma i in 0u32..4u32 {\n    \
                i = 7u32;\n  \
              }\n  \
              return;\n\
            }\n\
        ";
        let tokens = tokenize(src).expect("tokenize");
        let program = parse(&tokens).expect("parse");
        let result = resolve(&program);
        assert!(
            result.is_err(),
            "expected resolver to reject assign-to-sigma-var"
        );
    }

    #[test]
    fn s12_assign_to_undefined_local_rejected_by_resolver() {
        // `x = 5;` with no `x` in scope — UndefinedName.
        let src = "@fn t() { x = 5u32; return; }";
        let tokens = tokenize(src).expect("tokenize");
        let program = parse(&tokens).expect("parse");
        let result = resolve(&program);
        assert!(
            result.is_err(),
            "expected resolver to reject undefined local"
        );
        if let Err(errs) = result {
            let saw_undef = errs.iter().any(|e| {
                let s = format!("{e}");
                s.contains("E0402") && s.contains("undefined")
            });
            assert!(
                saw_undef,
                "expected E0402 undefined; got {errs:?}"
            );
        }
    }

    #[test]
    fn s12_multiple_assigns_to_same_local() {
        // Sequential reassignments emit one store per assign.
        let src = "\
            @fn t() -> u32 {\n  \
              let mut x: u32 = 1u32;\n  \
              x = 2u32;\n  \
              x = 3u32;\n  \
              return x;\n\
            }\n\
        ";
        let ir = lower_str(src).expect("lower multiple assigns");
        // Three stores: initial + two assigns.
        assert_eq!(
            ir.matches("store i32").count(),
            3,
            "expected 3 stores; got:\n{ir}"
        );
        // The final return loads through the alloca.
        assert!(
            ir.contains("load i32, i32*"),
            "expected load on return; got:\n{ir}"
        );
    }

    #[test]
    fn s12_let_mut_does_not_affect_sibling_immutable_let() {
        // A `let mut` doesn't change how a separate immutable
        // `let` is lowered — they're independent bindings.
        let src = "\
            @fn t() -> u32 {\n  \
              let a: u32 = 1u32;\n  \
              let mut b: u32 = 2u32;\n  \
              b = 3u32;\n  \
              return a;\n\
            }\n\
        ";
        let ir = lower_str(src).expect("lower mixed bindings");
        // Exactly one alloca (for `b`), even though both bindings
        // have the same IR type. `a` stays SSA-direct.
        assert_eq!(
            ir.matches("alloca i32").count(),
            1,
            "expected exactly one alloca; got:\n{ir}"
        );
        // Two stores (for b: initial + reassign).
        assert_eq!(
            ir.matches("store i32").count(),
            2,
            "expected exactly two stores (b only); got:\n{ir}"
        );
        // No load on the return path — `a` is SSA-direct so the
        // bind_via_identity SSA name is returned without a load.
        assert!(
            !ir.contains("load i32"),
            "immutable `a` must not need a load; got:\n{ir}"
        );
    }

    // ─── Slice 17: break / continue inside sigma loops ───────────────────

    #[test]
    fn break_emits_branch_to_sigma_exit_label() {
        // `break;` inside a sigma body emits `br label %sigma.exit.<id>`
        // and terminates the current basic block.
        let src = "\
            @fn t() {\n  \
              sigma i in 0u32..10u32 {\n    \
                break;\n  \
              }\n  \
              return;\n\
            }\n\
        ";
        let ir = lower_str(src).expect("lower break");
        // The sigma body must include a `br label %sigma.exit.0`
        // emitted by the break statement.
        assert!(
            ir.contains("br label %sigma.exit.0"),
            "expected break to branch to sigma.exit.0; got:\n{ir}"
        );
    }

    #[test]
    fn continue_emits_branch_to_sigma_continue_label() {
        // `continue;` inside a sigma body emits a `br label
        // %sigma.continue.<id>` so the increment runs.
        let src = "\
            @fn t() {\n  \
              sigma i in 0u32..10u32 {\n    \
                continue;\n  \
              }\n  \
              return;\n\
            }\n\
        ";
        let ir = lower_str(src).expect("lower continue");
        // We expect at least TWO branches to sigma.continue.0:
        // one from the (terminated) body via the explicit
        // `continue;` and one from the body's natural
        // fall-through-suppression (which doesn't fire because
        // the body terminated). So exactly one explicit branch
        // from the continue stmt.
        assert!(
            ir.contains("br label %sigma.continue.0"),
            "expected continue to branch to sigma.continue.0; got:\n{ir}"
        );
    }

    #[test]
    fn break_in_nested_sigma_targets_innermost_loop() {
        // Two nested sigma loops; the `break;` in the inner
        // body should target the INNER loop's exit label, not
        // the outer's.
        let src = "\
            @fn t() {\n  \
              sigma i in 0u32..4u32 {\n    \
                sigma j in 0u32..4u32 {\n      \
                  break;\n    \
                }\n  \
              }\n  \
              return;\n\
            }\n\
        ";
        let ir = lower_str(src).expect("lower nested-break");
        // The inner loop has id 1 (outer is 0). `break` should
        // target sigma.exit.1.
        assert!(
            ir.contains("\nsigma.exit.1:\n"),
            "expected inner exit label; got:\n{ir}"
        );
        // The break emits `br label %sigma.exit.1` (innermost).
        let break_count = ir.matches("br label %sigma.exit.1").count();
        assert!(
            break_count >= 1,
            "expected at least one branch to inner exit; got:\n{ir}"
        );
    }

    #[test]
    fn break_inside_if_inside_sigma_targets_loop_not_if() {
        // The `break;` is wrapped in an `if`. The if-block's
        // exit label is `if.exit.<id>`, but break should target
        // the SIGMA's exit, not the if's.
        let src = "\
            @fn t() {\n  \
              sigma i in 0u32..10u32 {\n    \
                if true { break; }\n  \
              }\n  \
              return;\n\
            }\n\
        ";
        let ir = lower_str(src).expect("lower break-in-if");
        // The break should branch to sigma.exit.<n>, not
        // if.exit.<n>. The exact ID depends on emission order
        // — sigma is allocated before the inner if.
        assert!(
            ir.contains("br label %sigma.exit.0"),
            "expected break to branch to sigma.exit.0 (not if.exit.*); got:\n{ir}"
        );
    }

    #[test]
    fn break_outside_sigma_rejected_by_resolver() {
        // `break;` at the top of an @fn body is invalid.
        // Resolver enforces with E0411; lower_str panics on
        // resolve errors so we exercise the resolver directly.
        let src = "@fn t() { break; }";
        let tokens = tokenize(src).expect("tokenize");
        let program = parse(&tokens).expect("parse");
        let result = resolve(&program);
        assert!(
            result.is_err(),
            "expected resolver to reject break outside sigma"
        );
        if let Err(errs) = result {
            let saw_e0411 = errs.iter().any(|e| {
                let s = format!("{e}");
                s.contains("E0411") && s.contains("break")
            });
            assert!(
                saw_e0411,
                "expected E0411 with `break` mentioned; got {errs:?}"
            );
        }
    }

    #[test]
    fn continue_outside_sigma_rejected_by_resolver() {
        let src = "@fn t() { continue; }";
        let tokens = tokenize(src).expect("tokenize");
        let program = parse(&tokens).expect("parse");
        let result = resolve(&program);
        assert!(result.is_err());
        if let Err(errs) = result {
            let saw_e0411 = errs.iter().any(|e| {
                let s = format!("{e}");
                s.contains("E0411") && s.contains("continue")
            });
            assert!(
                saw_e0411,
                "expected E0411 with `continue` mentioned; got {errs:?}"
            );
        }
    }

    #[test]
    fn break_with_local_mut_acc_can_early_exit() {
        // Realistic firmware shape: scan an array-typed field
        // for the first non-zero entry and break out. The
        // accumulator pattern combines slice-12 (`let mut`),
        // slice-13 (`if`), slice-11 (sigma), and slice-17
        // (break).
        let src = "\
            @fn first_nonzero_index(n: u32) -> u32 {\n  \
              let mut found: u32 = 0u32;\n  \
              sigma i in 0u32..n {\n    \
                if i > 3u32 {\n      \
                  found = i;\n      \
                  break;\n    \
                }\n  \
              }\n  \
              return found;\n\
            }\n\
        ";
        let ir = lower_str(src).expect("lower first_nonzero pattern");
        // Branch to the sigma exit lives somewhere after the
        // `if` then-branch.
        assert!(
            ir.contains("br label %sigma.exit.0"),
            "expected break branch; got:\n{ir}"
        );
        // The accumulator's alloca + final load on return.
        assert!(
            ir.contains("alloca i32"),
            "expected found's alloca; got:\n{ir}"
        );
        assert!(
            ir.contains("load i32, i32*"),
            "expected load on return; got:\n{ir}"
        );
    }

    #[test]
    fn body_after_break_is_dead_code() {
        // A statement after `break;` is dead. emit_block should
        // skip it (current_block_terminated is true after the
        // break's br).
        let src = "\
            @fn t() {\n  \
              sigma i in 0u32..10u32 {\n    \
                break;\n    \
                let _x: u32 = i;\n  \
              }\n  \
              return;\n\
            }\n\
        ";
        let ir = lower_str(src).expect("lower break+dead");
        // The dead `let _x` should NOT have lowered — no `add`
        // or alloca for it.
        assert!(
            !ir.contains("add i32 0, %sigma.i.0"),
            "expected dead let to be skipped; got:\n{ir}"
        );
    }

    // ─── Slice 11: sigma loops (Decision #14 / §5.8) ─────────────────────

    #[test]
    fn s11_sigma_basic_half_open_emits_loop_cfg() {
        // `sigma i in 0u32..4u32 { … }` — canonical counted loop.
        // Updated for slice-17 four-block CFG (header / body /
        // continue / exit). The continue block holds the
        // increment + back-edge so `continue;` statements have
        // a clean target; the phi reads from `%sigma.continue.0`
        // for the body-incoming edge.
        let src = "@fn loop_test() { sigma i in 0u32..4u32 { } return; }";
        let ir = lower_str(src).expect("lower sigma half-open");
        // Four labels, each at column 0.
        assert!(ir.contains("\nsigma.header.0:\n"), "missing header label; got:\n{ir}");
        assert!(ir.contains("\nsigma.body.0:\n"), "missing body label; got:\n{ir}");
        assert!(ir.contains("\nsigma.continue.0:\n"), "missing continue label; got:\n{ir}");
        assert!(ir.contains("\nsigma.exit.0:\n"), "missing exit label; got:\n{ir}");
        // Branch into the header from entry.
        assert!(
            ir.contains("br label %sigma.header.0"),
            "missing entry-to-header branch; got:\n{ir}"
        );
        // Phi: body-incoming label is `continue`, not `body`,
        // because the increment lives in the continue block now.
        assert!(
            ir.contains("%sigma.i.0 = phi i32 [ 0, %entry ], [ %sigma.i_next.0, %sigma.continue.0 ]"),
            "missing phi (slice-17 shape with continue label); got:\n{ir}"
        );
        // Unsigned half-open compare.
        assert!(
            ir.contains("%sigma.cond.0 = icmp ult i32 %sigma.i.0, 4"),
            "missing icmp ult; got:\n{ir}"
        );
        // Conditional branch into body or exit.
        assert!(
            ir.contains("br i1 %sigma.cond.0, label %sigma.body.0, label %sigma.exit.0"),
            "missing conditional branch; got:\n{ir}"
        );
        // Increment lives in the continue block.
        assert!(
            ir.contains("%sigma.i_next.0 = add nuw i32 %sigma.i.0, 1"),
            "missing add nuw increment; got:\n{ir}"
        );
        // Three branches into the header: entry pre-loop +
        // body-fall-through-to-continue + continue back-edge.
        // Actually: entry-to-header + continue-to-header. The
        // body-to-continue branch goes to continue, not header.
        assert!(
            ir.matches("br label %sigma.header.0").count() >= 2,
            "expected entry + back-edge branches into header; got:\n{ir}"
        );
        // Body falls through to continue.
        assert!(
            ir.contains("br label %sigma.continue.0"),
            "expected body fall-through to continue; got:\n{ir}"
        );
    }

    #[test]
    fn s11_sigma_inclusive_uses_ule_compare() {
        // `0u32..=10u32` — inclusive upper bound. Compare op is
        // `ule` instead of `ult` (`i <= 10` instead of `i < 10`).
        let src = "@fn t() { sigma i in 0u32..=10u32 { } return; }";
        let ir = lower_str(src).expect("lower sigma inclusive");
        assert!(
            ir.contains("icmp ule i32 %sigma.i.0, 10"),
            "expected icmp ule for inclusive range; got:\n{ir}"
        );
        // No `ult` should appear for an inclusive range.
        assert!(
            !ir.contains("icmp ult"),
            "inclusive range must not use ult; got:\n{ir}"
        );
    }

    #[test]
    fn s11_sigma_signed_range_uses_slt_and_nsw() {
        // Signed range source (`0i32..10i32`) — compare is `slt`
        // (signed less-than) and increment uses `add nsw` (no
        // signed wrap).
        let src = "@fn t() { sigma i in 0i32..10i32 { } return; }";
        let ir = lower_str(src).expect("lower signed sigma");
        assert!(
            ir.contains("icmp slt i32 %sigma.i.0, 10"),
            "expected icmp slt for signed range; got:\n{ir}"
        );
        assert!(
            ir.contains("add nsw i32 %sigma.i.0, 1"),
            "expected add nsw for signed increment; got:\n{ir}"
        );
        assert!(
            !ir.contains("add nuw"),
            "signed range must not use nuw; got:\n{ir}"
        );
    }

    #[test]
    fn s11_sigma_loop_var_bound_inside_body() {
        // The loop variable `i` should be visible inside the body
        // and resolve to the phi SSA name. We verify by using `i`
        // in a binary op inside the body.
        let src = "\
            @fn t() {\n  \
              sigma i in 0u32..4u32 {\n    \
                let _x: u32 = i + 1u32;\n  \
              }\n  \
              return;\n\
            }\n\
        ";
        let ir = lower_str(src).expect("lower sigma with body using i");
        // The loop var resolves to %sigma.i.0; the body must
        // reference it in an `add`.
        assert!(
            ir.contains("add i32 %sigma.i.0, 1"),
            "expected body to reference %sigma.i.0; got:\n{ir}"
        );
    }

    #[test]
    fn s11_sigma_ranges_with_dynamic_bounds() {
        // `lo..hi` where lo and hi are local-variable references —
        // the bounds are emitted as SSA values in the predecessor
        // block, not as constants in the phi.
        let src = "\
            @fn t(start: u32, end: u32) {\n  \
              sigma i in start..end { }\n  \
              return;\n\
            }\n\
        ";
        let ir = lower_str(src).expect("lower sigma with dynamic bounds");
        // Phi reads `%start` (the lo) on the entry edge.
        assert!(
            ir.contains("phi i32 [ %start, %entry ]"),
            "expected phi initial value to be %start; got:\n{ir}"
        );
        // Compare reads %end as the upper bound.
        assert!(
            ir.contains("icmp ult i32 %sigma.i.0, %end"),
            "expected compare to use %end; got:\n{ir}"
        );
    }

    #[test]
    fn s11_sigma_bounds_emitted_in_predecessor_block() {
        // Bounds are evaluated ONCE before the loop, not per-iter.
        // For a literal range this is hard to verify directly, but
        // we can confirm the phi's predecessor edge points back to
        // `%entry` (showing the bounds were captured there).
        let src = "@fn t() { sigma i in 0u32..4u32 { } return; }";
        let ir = lower_str(src).expect("lower sigma");
        // The phi's first incoming edge label must be `%entry`,
        // not the body label — otherwise the bound would be
        // recomputed on every back-edge.
        assert!(
            ir.contains("phi i32 [ 0, %entry ]"),
            "expected phi predecessor edge from %entry; got:\n{ir}"
        );
    }

    #[test]
    fn s11_sigma_followed_by_statements_emit_in_exit_block() {
        // Statements after the sigma loop emit into `sigma.exit.0`,
        // not into the body or header. We confirm by checking the
        // `ret void` comes AFTER the exit label.
        let src = "@fn t() { sigma i in 0u32..4u32 { } return; }";
        let ir = lower_str(src).expect("lower sigma");
        let exit_pos = ir.find("\nsigma.exit.0:\n").expect("exit label missing");
        let ret_pos = ir.find("ret void").expect("ret void missing");
        assert!(
            exit_pos < ret_pos,
            "ret void must come after exit label; positions {exit_pos} / {ret_pos} in:\n{ir}"
        );
    }

    #[test]
    fn s11_sigma_nested_uses_distinct_label_ids() {
        // Two nested sigmas should get distinct label IDs (0 and 1)
        // so their CFGs don't collide. We use the inner loop var
        // in the body to force its scope handling.
        let src = "\
            @fn t() {\n  \
              sigma i in 0u32..4u32 {\n    \
                sigma j in 0u32..4u32 {\n      \
                  let _x: u32 = i + j;\n    \
                }\n  \
              }\n  \
              return;\n\
            }\n\
        ";
        let ir = lower_str(src).expect("lower nested sigma");
        // Two sets of header/body/exit labels with distinct IDs.
        for label in [
            "\nsigma.header.0:\n",
            "\nsigma.body.0:\n",
            "\nsigma.exit.0:\n",
            "\nsigma.header.1:\n",
            "\nsigma.body.1:\n",
            "\nsigma.exit.1:\n",
        ] {
            assert!(
                ir.contains(label),
                "missing label `{}`; got:\n{ir}",
                label.trim()
            );
        }
        // Body of outer loop (i + j) uses both phis.
        assert!(
            ir.contains("%sigma.i.0"),
            "outer loop var missing; got:\n{ir}"
        );
        assert!(
            ir.contains("%sigma.i.1"),
            "inner loop var missing; got:\n{ir}"
        );
    }

    #[test]
    fn s11_sigma_loop_var_invisible_after_loop() {
        // After the loop ends, the loop variable is out of scope.
        // The resolver should reject `return i;` after a sigma
        // loop with `UndefinedName { name: "i" }`. This test
        // exercises the resolver side directly because lower_str
        // panics on resolve failures (it expects pre-validated
        // input).
        let src = "\
            @fn t() -> u32 {\n  \
              sigma i in 0u32..4u32 { }\n  \
              return i;\n\
            }\n\
        ";
        let tokens = tokenize(src).expect("tokenize");
        let program = parse(&tokens).expect("parse");
        let result = resolve(&program);
        assert!(
            result.is_err(),
            "expected resolver to reject out-of-scope loop var; got Ok"
        );
        // Verify the specific error shape — the loop var `i` is
        // undefined OUTSIDE the loop body.
        if let Err(errs) = result {
            let saw_i = errs.iter().any(|e| {
                let s = format!("{e}");
                s.contains("`i`") || s.contains("\"i\"")
            });
            assert!(
                saw_i,
                "expected resolver error to mention `i`; got {errs:?}"
            );
        }
    }

    #[test]
    fn s11_sigma_non_range_source_returns_e0810() {
        // `sigma i in some_var { }` where the source isn't a Range
        // expression — v0.1 supports range sources only. The
        // resolver/types accept this (it's syntactically a value
        // expression); codegen surfaces NotYetImplemented.
        // Use a literal as a stand-in for any non-range source.
        let src = "@fn t() { sigma i in 7u32 { } return; }";
        let result = lower_str(src);
        // Either typing fails (range-bound mismatch) or codegen
        // surfaces NYI for non-range source. Either is acceptable.
        assert!(
            result.is_err(),
            "expected error for non-range source; got:\n{:?}",
            result
        );
    }

    #[test]
    fn s11_sigma_with_mutate_short_inside_body() {
        // Real firmware shape: initialize a buffer slot per
        // iteration. Combines slice-9 multi-state field write with
        // the loop var.
        let src = "\
            #automaton T {\n  \
              count: u32;\n\
            }\n\
            #effect tally_n() #mutates: [T] {\n  \
              sigma i in 0u32..4u32 {\n    \
                T.count += 1u32;\n  \
              }\n  \
              return;\n\
            }\n\
        ";
        let ir = lower_str(src).expect("lower mutate-inside-sigma");
        // The mutate-short emits load+add+store inside the body
        // block; verify the body label is present and the store
        // happens between body and back-edge.
        assert!(
            ir.contains("\nsigma.body.0:\n"),
            "missing body label; got:\n{ir}"
        );
        let body_pos = ir.find("\nsigma.body.0:\n").unwrap();
        let back_edge_pos = ir.rfind("br label %sigma.header.0").unwrap();
        let store_pos = ir.find("store i32").expect("store missing");
        assert!(
            body_pos < store_pos && store_pos < back_edge_pos,
            "store must be between body label and back-edge; got:\n{ir}"
        );
    }

    // ─── v0.2-ζ: @snapshot Auto.field codegen (Decision #24 / ADR 0004) ─

    #[test]
    fn snapshot_lowers_to_same_load_as_field_access() {
        // `@snapshot Counter.value` produces the same IR as
        // `Counter.value` — a single GEP+load. The "snapshot"
        // semantic is upstream of codegen.
        let src = "\
            #automaton Counter { value: u32; }\n\
            #effect drain() -> u32 #mutates: [Counter] {\n  \
              return @snapshot Counter.value;\n\
            }\n\
        ";
        let ir = lower_str(src).expect("lower @snapshot");
        // The IR should contain a GEP + load on Counter.value
        // (LLVM idx 0 for monoid Counter).
        assert!(
            ir.contains("getelementptr %struct.Counter, %struct.Counter* @Counter.state, i32 0, i32 0"),
            "expected struct-field GEP for snapshot; got:\n{ir}"
        );
        assert!(
            ir.contains("load i32, i32*"),
            "expected i32 load for snapshot; got:\n{ir}"
        );
    }

    #[test]
    fn snapshot_on_register_block_field_emits_volatile_load() {
        // `@snapshot Mmio.field` on a register-block automaton
        // lowers to a volatile load at the absolute MMIO address —
        // same as a regular `Mmio.field` read on a register block.
        let src = "\
            #automaton Mmio { #address: 0x4000_0000; status: u32 #offset: 0x00; }\n\
            #effect probe() -> u32 #mutates: [Mmio] { return @snapshot Mmio.status; }\n\
        ";
        let ir = lower_str(src).expect("lower mmio snapshot");
        assert!(
            ir.contains("load volatile i32, i32* inttoptr (i64 1073741824 to i32*)"),
            "expected volatile load at MMIO address for snapshot; got:\n{ir}"
        );
    }

    #[test]
    fn snapshot_self_inside_transition_resolves_owner() {
        // `@snapshot Self.field` inside a transition resolves
        // to the enclosing automaton, same as `Self.field` does.
        let src = "\
            #automaton Counter { value: u32;\n  \
              #transition observe { let _x: u32 = @snapshot Self.value; return; }\n\
            }\n\
        ";
        let ir = lower_str(src).expect("lower @snapshot Self");
        assert!(
            ir.contains("getelementptr %struct.Counter, %struct.Counter* @Counter.state, i32 0, i32 0"),
            "expected GEP via Counter (Self resolution); got:\n{ir}"
        );
    }

    #[test]
    fn snapshot_compound_field_returns_e0810() {
        // `@snapshot` on a compound (array) field should be
        // rejected — multi-load lowering would tear under
        // concurrent write, breaking the snapshot guarantee.
        let src = "\
            #automaton Counter { buf: [u8; 64]; }\n\
            #effect drain() #mutates: [Counter] {\n  \
              let _x: u8 = @snapshot Counter.buf;\n  \
              return;\n\
            }\n\
        ";
        let errors = lower_str(src).expect_err("expected NotYetImplemented for compound snapshot");
        let saw = errors.iter().any(|e| matches!(
            e,
            CodegenError::NotYetImplemented { what }
                if what.contains("non-primitive")
        ));
        assert!(
            saw,
            "expected NotYetImplemented(non-primitive); got {errors:?}"
        );
    }

    #[test]
    fn snapshot_on_unknown_automaton_rejected_by_resolver() {
        // `@snapshot Nope.field` where `Nope` isn't an automaton
        // — the resolver catches it before codegen gets a chance.
        // We exercise the resolver directly because lower_str
        // panics on resolve failures.
        let src = "@fn t() -> u32 { return @snapshot Nope.x; }";
        let tokens = tokenize(src).expect("tokenize");
        let program = parse(&tokens).expect("parse");
        let result = resolve(&program);
        assert!(
            result.is_err(),
            "expected resolver to reject @snapshot on unknown automaton"
        );
    }

    #[test]
    fn snapshot_inside_arithmetic_composes() {
        // `@snapshot` is an expression; it should compose inside
        // arithmetic (the snapshot-and-decide pattern's core
        // use case).
        let src = "\
            #automaton T { a: u32; b: u32; }\n\
            #effect sum() -> u32 #mutates: [T] {\n  \
              return @snapshot T.a + @snapshot T.b;\n\
            }\n\
        ";
        let ir = lower_str(src).expect("lower snapshot in binary op");
        // Two loads (one per snapshot), one add of their
        // SSA results.
        assert_eq!(
            ir.matches("load i32").count(),
            2,
            "expected 2 loads (one per snapshot); got:\n{ir}"
        );
        assert!(
            ir.contains("add i32"),
            "expected add of snapshot results; got:\n{ir}"
        );
    }

    // ─── v0.2-ε: #atomic: interrupt_critical runtime wrapping ────────────

    #[test]
    fn atomic_interrupt_critical_emits_cpsid_at_body_start() {
        // `#atomic: interrupt_critical;` on an effect produces an
        // inline-asm `cpsid i` at the start of the body (after
        // entry: + any entry fence).
        let src = "\
            #automaton C { v: u32; }\n\
            #effect snapshot() #mutates: [C] #atomic: interrupt_critical; { return; }\n\
        ";
        let ir = lower_str(src).expect("lower atomic effect");
        assert!(
            ir.contains("call void asm sideeffect \"cpsid i\""),
            "expected cpsid i entry asm; got:\n{ir}"
        );
    }

    #[test]
    fn atomic_interrupt_critical_emits_cpsie_before_ret() {
        // The matching `cpsie i` lands before every `ret`, paired
        // 1:1 with the entry mask.
        let src = "\
            #automaton C { v: u32; }\n\
            #effect snapshot() #mutates: [C] #atomic: interrupt_critical; { return; }\n\
        ";
        let ir = lower_str(src).expect("lower atomic effect");
        assert!(
            ir.contains("call void asm sideeffect \"cpsie i\""),
            "expected cpsie i exit asm; got:\n{ir}"
        );
        // The unmask appears before the ret.
        let unmask_pos = ir
            .find("cpsie i")
            .expect("cpsie should appear");
        let ret_pos = ir.find("ret void").expect("ret void should appear");
        assert!(
            unmask_pos < ret_pos,
            "cpsie i must come before ret; positions {unmask_pos} / {ret_pos} in:\n{ir}"
        );
    }

    #[test]
    fn atomic_emits_balanced_pair_per_function() {
        // Exactly one cpsid + one cpsie per #atomic callable.
        // (Multiple ret paths would emit multiple cpsie's, but
        // this minimal body has one ret.)
        let src = "\
            #automaton C { v: u32; }\n\
            #effect a() #mutates: [C] #atomic: interrupt_critical; { return; }\n\
            #effect b() #mutates: [C] #atomic: interrupt_critical; { return; }\n\
            #effect plain() #mutates: [C] { return; }\n\
        ";
        let ir = lower_str(src).expect("lower mixed atomic + non-atomic");
        // Two atomic effects → 2 cpsid + 2 cpsie. The plain
        // effect contributes nothing.
        assert_eq!(
            ir.matches("cpsid i").count(),
            2,
            "expected exactly 2 cpsid emissions; got:\n{ir}"
        );
        assert_eq!(
            ir.matches("cpsie i").count(),
            2,
            "expected exactly 2 cpsie emissions; got:\n{ir}"
        );
    }

    #[test]
    fn atomic_interacts_correctly_with_release_fence() {
        // The exit order must be: tag write → release fence → cpsie → ret.
        // For an atomic effect with $ [Release], we verify the
        // fence appears before the cpsie (so the publication
        // completes before any pending IRQ can fire).
        let src = "\
            #automaton C { v: u32; }\n\
            #effect commit() #mutates: [C] #atomic: interrupt_critical; $ [Release] {\n  \
              C.v = 1u32;\n  \
              return;\n\
            }\n\
        ";
        let ir = lower_str(src).expect("lower atomic + release");
        let fence_pos = ir.find("fence release").expect("expected fence release");
        let unmask_pos = ir.find("cpsie i").expect("expected cpsie i");
        let ret_pos = ir.find("ret void").expect("expected ret void");
        assert!(
            fence_pos < unmask_pos && unmask_pos < ret_pos,
            "expected order: fence release < cpsie i < ret void; got positions {fence_pos} / {unmask_pos} / {ret_pos} in:\n{ir}"
        );
    }

    #[test]
    fn non_atomic_effect_emits_no_cpsid_or_cpsie() {
        // Sanity: an effect WITHOUT #atomic produces zero asm
        // emissions. Confirms we don't accidentally wrap every
        // body.
        let src = "\
            #automaton C { v: u32; }\n\
            #effect plain() #mutates: [C] { C.v = 1u32; }\n\
        ";
        let ir = lower_str(src).expect("lower plain effect");
        assert!(
            !ir.contains("cpsid"),
            "plain effect should not emit cpsid; got:\n{ir}"
        );
        assert!(
            !ir.contains("cpsie"),
            "plain effect should not emit cpsie; got:\n{ir}"
        );
    }

    #[test]
    fn atomic_on_interrupt_emits_wrapping_too() {
        // #atomic: interrupt_critical on an interrupt is unusual
        // (interrupts mask their own priority on entry already)
        // but legal — the wrapping still emits and asserts the
        // body masks ALL maskable interrupts during execution.
        let src = "\
            #automaton C { v: u32; }\n\
            #interrupt SysTick() #mutates: [C] #priority: HIGH #atomic: interrupt_critical; { C.v = 1u32; }\n\
        ";
        let ir = lower_str(src).expect("lower atomic interrupt");
        assert!(ir.contains("cpsid i"), "expected cpsid; got:\n{ir}");
        assert!(ir.contains("cpsie i"), "expected cpsie; got:\n{ir}");
    }

    #[test]
    fn atomic_multicore_critical_is_not_yet_implemented() {
        // Reserved for Decision #21 (v0.7+). Codegen should
        // surface a structured error rather than silently emit
        // wrong code.
        let src = "\
            #automaton C { v: u32; }\n\
            #effect e() #mutates: [C] #atomic: multicore_critical; { return; }\n\
        ";
        let errors = lower_str(src).expect_err("expected NotYetImplemented for multicore");
        let saw = errors.iter().any(|e| matches!(
            e,
            CodegenError::NotYetImplemented { what }
                if what.contains("multicore_critical")
        ));
        assert!(
            saw,
            "expected NotYetImplemented(multicore_critical); got {errors:?}"
        );
    }

    #[test]
    fn atomic_custom_kind_is_not_yet_implemented() {
        // Custom atomicity kinds are parser-accepted but codegen
        // doesn't know what masking semantics to emit; surface a
        // structured error.
        let src = "\
            #automaton C { v: u32; }\n\
            #effect e() #mutates: [C] #atomic: my_custom_lock; { return; }\n\
        ";
        let errors = lower_str(src).expect_err("expected NotYetImplemented for custom");
        let saw = errors.iter().any(|e| matches!(
            e,
            CodegenError::NotYetImplemented { what }
                if what.contains("custom")
        ));
        assert!(
            saw,
            "expected NotYetImplemented(custom); got {errors:?}"
        );
    }

    // ─── Decision #22 codegen: memory-ordering fences ────────────────────

    #[test]
    fn d22_acquire_emits_entry_fence() {
        let src = "\
            #automaton C { v: u32; }\n\
            #effect read() #mutates: [C] $ [Acquire] { return; }\n\
        ";
        let ir = lower_str(src).expect("lower acquire");
        // Entry fence appears immediately after the entry: label.
        assert!(
            ir.contains("entry:\n  fence acquire\n"),
            "expected entry fence acquire; got:\n{ir}"
        );
        // No exit fence — Acquire is entry-only.
        assert!(
            !ir.contains("fence release"),
            "should not have release fence for $ [Acquire]; got:\n{ir}"
        );
    }

    #[test]
    fn d22_release_emits_exit_fence_before_ret() {
        let src = "\
            #automaton C { v: u32; }\n\
            #effect commit() #mutates: [C] $ [Release] { return; }\n\
        ";
        let ir = lower_str(src).expect("lower release");
        // Release fence appears before the explicit ret void.
        assert!(
            ir.contains("  fence release\n  ret void\n"),
            "expected exit fence release before ret; got:\n{ir}"
        );
        // No `acquire` fence at all (Release is exit-only; the
        // release fence will appear after entry: when the body is
        // empty, but it's the exit fence). The test verifies
        // there's no `acquire` ordering anywhere.
        assert!(
            !ir.contains("fence acquire"),
            "should not have acquire fence for $ [Release]; got:\n{ir}"
        );
        // Verify there's exactly ONE fence (the exit one) — for an
        // empty body, the entry fence (if any) and the exit fence
        // would both be release-keyword if both were emitted.
        assert_eq!(
            ir.matches("fence release").count(),
            1,
            "expected exactly one release fence; got:\n{ir}"
        );
    }

    #[test]
    fn d22_acquire_release_combo_emits_both() {
        let src = "\
            #automaton C { v: u32; }\n\
            #effect roundtrip() #mutates: [C] $ [Acquire, Release] { return; }\n\
        ";
        let ir = lower_str(src).expect("lower acq+rel");
        assert!(
            ir.contains("entry:\n  fence acquire\n"),
            "expected entry acquire fence; got:\n{ir}"
        );
        assert!(
            ir.contains("  fence release\n  ret void\n"),
            "expected exit release fence; got:\n{ir}"
        );
    }

    #[test]
    fn d22_seqcst_uses_seqcst_at_both_ends() {
        let src = "\
            #automaton C { v: u32; }\n\
            #effect strict() #mutates: [C] $ [SeqCst] { return; }\n\
        ";
        let ir = lower_str(src).expect("lower seqcst");
        assert!(
            ir.contains("entry:\n  fence seq_cst\n"),
            "expected entry seq_cst fence; got:\n{ir}"
        );
        assert!(
            ir.contains("  fence seq_cst\n  ret void\n"),
            "expected exit seq_cst fence; got:\n{ir}"
        );
        // SeqCst supersedes — there should be NO `acquire` / `release`
        // fences emitted (only `seq_cst`).
        assert!(
            !ir.contains("fence acquire") && !ir.contains("fence release"),
            "SeqCst should subsume Acquire/Release; got:\n{ir}"
        );
    }

    #[test]
    fn d22_seqcst_supersedes_acquire_release() {
        // `$ [SeqCst, Acquire, Release]` — SeqCst is the strongest,
        // so we emit `seq_cst` fences (not `acquire` / `release`).
        let src = "\
            #automaton C { v: u32; }\n\
            #effect strict() #mutates: [C] $ [SeqCst, Acquire, Release] { return; }\n\
        ";
        let ir = lower_str(src).expect("lower seqcst+ subsumed");
        assert!(
            ir.contains("entry:\n  fence seq_cst\n"),
            "expected entry seq_cst (supersedes); got:\n{ir}"
        );
        assert!(
            !ir.contains("fence acquire"),
            "Acquire should be subsumed; got:\n{ir}"
        );
        assert!(
            !ir.contains("fence release"),
            "Release should be subsumed; got:\n{ir}"
        );
    }

    #[test]
    fn d22_no_ordering_trait_emits_no_fence() {
        let src = "\
            #automaton C { v: u32; }\n\
            #effect plain() #mutates: [C] { return; }\n\
        ";
        let ir = lower_str(src).expect("lower plain effect");
        assert!(
            !ir.contains("fence "),
            "no ordering trait → no fence; got:\n{ir}"
        );
    }

    #[test]
    fn d22_acquire_on_interrupt_emits_entry_fence() {
        // Interrupts get the same treatment as effects.
        let src = "\
            #automaton T { c: u32; }\n\
            #interrupt SysTick() #mutates: [T] #priority: HIGH $ [Acquire] { return; }\n\
        ";
        let ir = lower_str(src).expect("lower interrupt acquire");
        assert!(
            ir.contains("entry:\n  fence acquire\n"),
            "expected acquire fence on interrupt; got:\n{ir}"
        );
    }

    #[test]
    fn d22_release_on_transition_emits_exit_fence() {
        let src = "\
            #automaton Counter { value: u32;\n  \
              #transition tick $ [Release] { Counter.value = 1u32; }\n\
            }\n\
        ";
        let ir = lower_str(src).expect("lower transition release");
        assert!(
            ir.contains("  fence release\n  ret void\n"),
            "expected release fence on transition exit; got:\n{ir}"
        );
    }

    #[test]
    fn d22_release_emits_fence_at_each_explicit_ret() {
        // A fn with an explicit `return expr;` (not just falling through
        // to implicit ret void). The exit fence still goes before the
        // `ret`.
        let src = "\
            #automaton C { v: u32; }\n\
            #effect get() -> u32 #mutates: [C] $ [Release] { return 5u32; }\n\
        ";
        let ir = lower_str(src).expect("lower explicit-return release");
        // The exit fence sits between any tail-of-body code and the
        // `ret i32 5`.
        assert!(
            ir.contains("  fence release\n  ret i32 5\n"),
            "expected fence release before explicit ret; got:\n{ir}"
        );
    }

    #[test]
    fn d22_other_traits_dont_emit_fences() {
        // `Hardware`, `Realtime`, `LockingDiscipline`, `PureState`,
        // `Encapsulated` are codegen-side declarative-only — no
        // fences.
        for trait_name in [
            "Hardware",
            "Realtime",
            "LockingDiscipline",
            "PureState",
            "Encapsulated",
        ] {
            let src = format!(
                "#automaton C {{ v: u32; }}\n\
                 #effect e() #mutates: [C] $ [{trait_name}] {{ return; }}\n"
            );
            let ir = lower_str(&src).unwrap_or_else(|e| panic!("lower failed: {e:?}"));
            assert!(
                !ir.contains("fence "),
                "trait `{trait_name}` should not emit a fence; got:\n{ir}"
            );
        }
    }

    #[test]
    fn s3_full_counter_program_lowers_cleanly() {
        // Full v0.1 firmware shape: automaton + effect + transition,
        // with mutation sugar, compound assignment, and proc calls.
        // This is the canonical end-to-end smoke test.
        let src = "\
            #automaton Counter { value: u32; }\n\
            #effect bump() #mutates: [Counter] { Counter.value += 1u32; }\n\
            #effect reset() #mutates: [Counter] { Counter.value = 0u32; }\n\
            #effect main() #mutates: [Counter] { #> bump(); #> bump(); #> reset(); }\n\
        ";
        let ir = lower_str(src).expect("lower full program");
        for needle in [
            "%struct.Counter = type { i32 }",
            "@Counter.state = global %struct.Counter zeroinitializer",
            "define void @bump()",
            "define void @reset()",
            "define void @main()",
            "call void @bump()",
            "call void @reset()",
        ] {
            assert!(
                ir.contains(needle),
                "missing `{needle}` in IR; got:\n{ir}"
            );
        }
    }

    // ─── Slice 18: `#staged` automaton + `#flush` (Decision #12) ─────────

    #[test]
    fn s18_staged_automaton_emits_shadow_global() {
        // A `#staged #automaton` produces TWO global state instances:
        // `@<Name>.state` (live) and `@<Name>.shadow` (pending writes).
        // Both are zero-initialised so the program lifecycle starts
        // with the shadow == live invariant.
        let src = "#staged #automaton C { v: u32; }";
        let ir = lower_str(src).expect("lower staged automaton");
        assert!(
            ir.contains("@C.state = global %struct.C zeroinitializer"),
            "expected live global @C.state; got:\n{ir}"
        );
        assert!(
            ir.contains("@C.shadow = global %struct.C zeroinitializer"),
            "expected shadow global @C.shadow; got:\n{ir}"
        );
    }

    #[test]
    fn s18_unstaged_automaton_emits_no_shadow_global() {
        // Pre-slice-18 IR shape is preserved for non-staged
        // automata: only `@<Name>.state`, no shadow.
        let src = "#automaton C { v: u32; }";
        let ir = lower_str(src).expect("lower plain automaton");
        assert!(
            ir.contains("@C.state = global %struct.C zeroinitializer"),
            "expected live global @C.state; got:\n{ir}"
        );
        assert!(
            !ir.contains("@C.shadow"),
            "non-staged automaton must not emit a shadow; got:\n{ir}"
        );
    }

    #[test]
    fn s18_mutate_short_on_staged_writes_to_shadow() {
        // `Counter.value = 5u32;` against a `#staged` automaton must
        // GEP into `@Counter.shadow`, NOT `@Counter.state`. The
        // pre-slice-18 IR would have hit `.state` directly.
        let src = "\
            #staged #automaton Counter { value: u32; }\n\
            #effect set() #mutates: [Counter] { Counter.value = 5u32; }\n\
        ";
        let ir = lower_str(src).expect("lower staged write");
        assert!(
            ir.contains("@Counter.shadow"),
            "expected write to target @Counter.shadow; got:\n{ir}"
        );
        // The store itself must reference a pointer derived from
        // shadow, so the GEP appears in the effect body.
        assert!(
            ir.contains("getelementptr %struct.Counter, %struct.Counter* @Counter.shadow"),
            "expected GEP via @Counter.shadow; got:\n{ir}"
        );
    }

    #[test]
    fn s18_mutate_block_on_staged_writes_to_shadow() {
        // Bulk-form `#mutate Counter { value = 7u32 }` likewise
        // routes through the shadow global.
        let src = "\
            #staged #automaton Counter { value: u32; }\n\
            #effect set() #mutates: [Counter] { #mutate Counter { value = 7u32 }; }\n\
        ";
        let ir = lower_str(src).expect("lower staged #mutate");
        assert!(
            ir.contains("getelementptr %struct.Counter, %struct.Counter* @Counter.shadow"),
            "#mutate on staged automaton must GEP into shadow; got:\n{ir}"
        );
    }

    #[test]
    fn s18_field_read_on_staged_still_reads_live() {
        // Reads continue to come from `@<Name>.state` (live)
        // regardless of staged vs. non-staged. The shadow is for
        // pending WRITES only — readers see consistent committed
        // state.
        let src = "\
            #staged #automaton Counter { value: u32; }\n\
            @fn observe() -> u32 { return Counter.value; }\n\
        ";
        let ir = lower_str(src).expect("lower staged read");
        assert!(
            ir.contains("getelementptr %struct.Counter, %struct.Counter* @Counter.state"),
            "read of Counter.value must come from live @Counter.state; got:\n{ir}"
        );
        // The read path must NOT touch shadow.
        assert!(
            !ir.contains("@Counter.shadow, i32 0, i32 0"),
            "read path must not GEP into shadow; got:\n{ir}"
        );
    }

    #[test]
    fn s18_flush_emits_memcpy_shadow_to_state() {
        // `#flush Counter;` lowers to a memcpy from the shadow to
        // the live global. The intrinsic declaration appears at
        // module scope (emitted once for the whole module).
        let src = "\
            #staged #automaton Counter { value: u32; }\n\
            #effect commit() #mutates: [Counter] { #flush Counter; return; }\n\
        ";
        let ir = lower_str(src).expect("lower flush");
        // Module-level intrinsic decl.
        assert!(
            ir.contains("declare void @llvm.memcpy.p0.p0.i64(i8*, i8*, i64, i1)"),
            "expected llvm.memcpy decl; got:\n{ir}"
        );
        // Bitcasts of both globals to i8* (one for dest, one for src).
        assert!(
            ir.contains("bitcast %struct.Counter* @Counter.state to i8*"),
            "expected dest bitcast of @Counter.state; got:\n{ir}"
        );
        assert!(
            ir.contains("bitcast %struct.Counter* @Counter.shadow to i8*"),
            "expected src bitcast of @Counter.shadow; got:\n{ir}"
        );
        // The memcpy call itself.
        assert!(
            ir.contains("call void @llvm.memcpy.p0.p0.i64"),
            "expected memcpy call; got:\n{ir}"
        );
        // GEP-on-null size idiom for target-pointer-width
        // independence.
        assert!(
            ir.contains("getelementptr %struct.Counter, %struct.Counter* null, i32 1"),
            "expected GEP-on-null size idiom; got:\n{ir}"
        );
    }

    #[test]
    fn s18_no_flush_no_intrinsic_decl() {
        // Unsurprisingly: a program without any `#staged`
        // automaton must not pollute the IR with the memcpy
        // declaration. Keeps non-staged programs byte-identical
        // to pre-slice-18 output.
        let src = "\
            #automaton C { v: u32; }\n\
            #effect set() #mutates: [C] { C.v = 1u32; }\n\
        ";
        let ir = lower_str(src).expect("lower non-staged program");
        assert!(
            !ir.contains("@llvm.memcpy"),
            "non-staged program must not declare memcpy; got:\n{ir}"
        );
    }

    #[test]
    fn s18_multi_state_staged_tag_write_targets_shadow() {
        // For a multi-state `#staged` automaton, the destination-
        // state tag write at transition exit must also route
        // through the shadow so a flush commits both field
        // updates AND the new state tag atomically.
        let src = "\
            #staged #automaton M { #states: [A, B]; v: u32;\n\
              #transition flip -> B { M.v = 99u32; }\n\
            }\n\
        ";
        let ir = lower_str(src).expect("lower staged multi-state");
        // Tag write (`store i32 1, i32* %tmp.X`) must appear
        // after a GEP into `@M.shadow` at LLVM idx 0.
        assert!(
            ir.contains("getelementptr %struct.M, %struct.M* @M.shadow, i32 0, i32 0"),
            "expected tag-write GEP into @M.shadow; got:\n{ir}"
        );
        // The value write is at user idx 0 → LLVM idx 1
        // (multi-state shifts user fields by one).
        assert!(
            ir.contains("getelementptr %struct.M, %struct.M* @M.shadow, i32 0, i32 1"),
            "expected value-write GEP into @M.shadow at idx 1; got:\n{ir}"
        );
    }

    // ─── Slice 21: `#audit` codegen markers (Decision #18) ──────────────

    #[test]
    fn s21_unaudited_transition_emits_no_audit_marker() {
        // A non-`#audit` automaton's transitions emit byte-identical
        // IR to slice-20 output — no `; audit-wrap site` comments.
        let src = "\
            #automaton P { \n  \
              #transition tick { \n    \
                let p: &u32 = #unchecked_cast<u64, &u32>(\"mmio\", 0x4000u64); \n    \
                #unchecked_store<u32>(p, 1u32); \n  \
              } \n\
            }\n\
        ";
        let ir = lower_str(src).expect("lower non-audit");
        assert!(
            !ir.contains("audit-wrap site"),
            "non-audit transition must not emit audit markers; got:\n{ir}"
        );
    }

    #[test]
    fn s21_audited_transition_unchecked_store_emits_marker() {
        // `#audit #automaton` transition with an
        // `#unchecked_store` emits a `; audit-wrap site for P
        // (unchecked_store)` IR comment immediately before the
        // `store` instruction.
        let src = "\
            #audit #automaton P { \n  \
              #transition tick { \n    \
                let p: &u32 = #unchecked_cast<u64, &u32>(\"mmio\", 0x4000u64); \n    \
                #unchecked_store<u32>(p, 1u32); \n  \
              } \n\
            }\n\
        ";
        let ir = lower_str(src).expect("lower audit");
        assert!(
            ir.contains("; audit-wrap site for P (unchecked_store) ; Decision #18"),
            "expected audit marker for unchecked_store; got:\n{ir}"
        );
    }

    #[test]
    fn s21_audited_transition_unchecked_cast_emits_marker() {
        // The `#unchecked_cast` itself also gets a marker (it's
        // listed as one of the unsafe primitives in Decision #18).
        // `inttoptr` (u64 -> &u32) is a non-trivial cast so the
        // marker is emitted (same-IR-type casts are no-ops and
        // emit no marker — see emit_unchecked_cast).
        let src = "\
            #audit #automaton P { \n  \
              #transition tick { \n    \
                let p: &u32 = #unchecked_cast<u64, &u32>(\"mmio\", 0x4000u64); \n    \
                #unchecked_store<u32>(p, 1u32); \n  \
              } \n\
            }\n\
        ";
        let ir = lower_str(src).expect("lower audit");
        assert!(
            ir.contains("; audit-wrap site for P (unchecked_cast) ; Decision #18"),
            "expected audit marker for unchecked_cast; got:\n{ir}"
        );
    }

    #[test]
    fn s21_audited_transition_volatile_store_emits_marker() {
        // The volatile sibling lowers through the same emitter
        // and gets its own categorised marker (`volatile_store`).
        let src = "\
            #audit #automaton P { \n  \
              #transition tick { \n    \
                let p: &u32 = #unchecked_cast<u64, &u32>(\"mmio\", 0x4000u64); \n    \
                #volatile_store<u32>(p, 7u32); \n  \
              } \n\
            }\n\
        ";
        let ir = lower_str(src).expect("lower audit volatile");
        assert!(
            ir.contains("; audit-wrap site for P (volatile_store) ; Decision #18"),
            "expected audit marker for volatile_store; got:\n{ir}"
        );
    }

    #[test]
    fn s21_audited_transition_unchecked_offset_emits_marker() {
        // `#unchecked_offset` GEP also gets a marker.
        let src = "\
            #audit #automaton P { \n  \
              #transition tick { \n    \
                let p: &u32 = #unchecked_cast<u64, &u32>(\"mmio\", 0x4000u64); \n    \
                let _q: &u32 = #unchecked_offset<u32>(p, 4i32); \n  \
              } \n\
            }\n\
        ";
        let ir = lower_str(src).expect("lower audit offset");
        assert!(
            ir.contains("; audit-wrap site for P (unchecked_offset) ; Decision #18"),
            "expected audit marker for unchecked_offset; got:\n{ir}"
        );
    }

    #[test]
    fn s21_unchecked_load_emits_marker() {
        // Read primitive — `#unchecked_load`.
        let src = "\
            #audit #automaton P { \n  \
              #transition tick { \n    \
                let p: &u32 = #unchecked_cast<u64, &u32>(\"mmio\", 0x4000u64); \n    \
                let _v: u32 = #unchecked_load<u32>(p); \n  \
              } \n\
            }\n\
        ";
        let ir = lower_str(src).expect("lower audit load");
        assert!(
            ir.contains("; audit-wrap site for P (unchecked_load) ; Decision #18"),
            "expected audit marker for unchecked_load; got:\n{ir}"
        );
    }

    #[test]
    fn s21_audit_marker_does_not_leak_across_transitions() {
        // Two automatons in one program: one audited, one not.
        // Markers must appear ONLY in the audited transition's
        // body. Specifically: a `Q.bar` transition emitted AFTER
        // the audited `P.foo` must not inherit the marker (the
        // per-function reset clears `current_audited_owner`).
        let src = "\
            #audit #automaton P { \n  \
              #transition foo { \n    \
                let p: &u32 = #unchecked_cast<u64, &u32>(\"mmio\", 0x4000u64); \n    \
                #unchecked_store<u32>(p, 1u32); \n  \
              } \n\
            }\n\
            #automaton Q { \n  \
              #transition bar { \n    \
                let q: &u32 = #unchecked_cast<u64, &u32>(\"mmio\", 0x4004u64); \n    \
                #unchecked_store<u32>(q, 2u32); \n  \
              } \n\
            }\n\
        ";
        let ir = lower_str(src).expect("lower mixed");
        // Both functions live in the same module IR.
        let p_def = ir.find("define void @P_foo()").expect("P_foo defined");
        let q_def = ir.find("define void @Q_bar()").expect("Q_bar defined");
        assert!(p_def < q_def, "expected P_foo before Q_bar in IR");
        // P_foo body should contain the marker.
        let p_body = &ir[p_def..q_def];
        assert!(
            p_body.contains("audit-wrap site for P"),
            "expected marker in P_foo body; got:\n{p_body}"
        );
        // Q_bar body must NOT.
        let q_body = &ir[q_def..];
        assert!(
            !q_body.contains("audit-wrap site"),
            "non-audit Q_bar must not emit markers; got:\n{q_body}"
        );
    }

    #[test]
    fn s21_audit_marker_does_not_appear_in_effects() {
        // Slice 21 scope: only TRANSITION bodies of audit
        // automatons get markers. An effect that targets an
        // audit automaton via `#mutates: [...]` does NOT
        // produce markers — that wider semantics is a
        // documented future-slice extension.
        let src = "\
            #audit #automaton P { \n  \
              v: u32; \n\
            }\n\
            #effect tick() #mutates: [P] { \n  \
              let p: &u32 = #unchecked_cast<u64, &u32>(\"mmio\", 0x4000u64); \n  \
              #unchecked_store<u32>(p, 1u32); \n  \
              return; \n\
            }\n\
        ";
        let ir = lower_str(src).expect("lower effect");
        // The effect body has no marker because the marker is
        // transition-scoped in slice 21.
        assert!(
            !ir.contains("audit-wrap site"),
            "slice 21 effects must not emit markers; got:\n{ir}"
        );
    }

    #[test]
    fn s21_audit_marker_composes_with_staged_writes() {
        // Sanity: an `#audit #staged` automaton (both modifiers)
        // emits both the slice-18 shadow-write redirection AND
        // the slice-21 audit marker for any unsafe primitive in
        // its transitions. The two slices are orthogonal.
        let src = "\
            #audit #staged #automaton P { \n  \
              #transition tick { \n    \
                let p: &u32 = #unchecked_cast<u64, &u32>(\"mmio\", 0x4000u64); \n    \
                #unchecked_store<u32>(p, 1u32); \n  \
              } \n\
            }\n\
        ";
        let ir = lower_str(src).expect("lower audit+staged");
        assert!(
            ir.contains("@P.shadow"),
            "expected #staged shadow global; got:\n{ir}"
        );
        assert!(
            ir.contains("audit-wrap site for P"),
            "expected audit marker; got:\n{ir}"
        );
    }
}
