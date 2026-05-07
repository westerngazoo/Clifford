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
    /// Slice 3: registry of every non-register-block `#automaton` in
    /// the program. Maps name → field-offset table so `#mutate` and
    /// `Auto.field` lowering can pick the right `getelementptr`
    /// index without re-walking the AST.
    automatons: HashMap<String, AutomatonInfo>,
    /// Slice 3: current owning automaton when emitting a
    /// `#transition` body. `None` outside of transitions. Used to
    /// resolve `Self.field` to the correct automaton.
    enclosing_owner: Option<String>,
    /// Errors collected across the whole program.
    errors: Vec<CodegenError>,
}

/// Per-automaton info captured in pass 1 (slice 3). The `fields` vec
/// preserves declaration order so `getelementptr` indices match LLVM
/// struct layout.
struct AutomatonInfo {
    /// Source name (matches `AutomatonDecl.name`).
    #[allow(dead_code)] // recorded for diagnostics; the map key is the canonical lookup
    name: String,
    /// `(field_name, ir_type)` pairs in declaration order. The index
    /// in this vec is the LLVM struct field index used by `getelementptr`.
    fields: Vec<(String, String)>,
    /// `true` if this automaton is a register block (`#address: 0x…`
    /// clause present). Slice 3 records the flag but does NOT lower
    /// register-block fields — that's slice 4 work via volatile
    /// loads/stores at `address + offset`. For slice 3, register-block
    /// automatons skip state-struct emission and any reference to them
    /// in lowered code surfaces as `NotYetImplemented`.
    is_register_block: bool,
}

/// One per-function local binding: source name + SSA-value ref + IR
/// type. Slice 2 tracks the IR type alongside the value so path-
/// position lookups don't need to re-walk the typing map.
struct LocalBinding {
    name: String,
    value: String,
    ir_type: String,
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
            enclosing_owner: None,
            errors: Vec::new(),
        }
    }

    /// Pass 1: build the [`AutomatonInfo`] registry from every
    /// `Item::Automaton` in the program. Records field-name → IR
    /// type and the index for each field's GEP offset.
    fn collect_automatons(&mut self, program: &Program) {
        for item in &program.items {
            if let Item::Automaton(decl) = item {
                let mut fields: Vec<(String, String)> = Vec::with_capacity(decl.fields.len());
                for f in &decl.fields {
                    let ir_ty = self.lower_type(&f.ty).unwrap_or_else(|e| {
                        self.errors.push(e);
                        "i32".to_owned()
                    });
                    fields.push((f.name.clone(), ir_ty));
                }
                self.automatons.insert(
                    decl.name.clone(),
                    AutomatonInfo {
                        name: decl.name.clone(),
                        fields,
                        is_register_block: decl.address.is_some(),
                    },
                );
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
            let parts: Vec<String> = info.fields.iter().map(|(_, ty)| ty.clone()).collect();
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
            writeln!(&mut self.out).ok();
        }
    }

    /// Emit one LLVM function per `#effect` declaration. Effects are
    /// lowered like `@fn` but with mutation access to the automatons
    /// listed in their `#mutates` clause; this slice's body walker
    /// handles `#mutate` / mutation-sugar / automaton-field reads.
    fn emit_effect(&mut self, decl: &EffectDecl) {
        // Reset per-function state.
        self.next_value_id = 0;
        self.locals.clear();
        self.enclosing_owner = None;

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

    /// Emit one LLVM function per `#interrupt` declaration. Slice 3
    /// emits the function with the linker symbol matching the source
    /// name (Decision #10); the target-specific calling convention,
    /// `.interrupts` section attribute, and disable-interrupts wrapper
    /// per Refinement #5e are slice-4 work.
    fn emit_interrupt(&mut self, decl: &InterruptDecl) {
        // Reset per-function state.
        self.next_value_id = 0;
        self.locals.clear();
        self.enclosing_owner = None;

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
            });
        }

        // Linker symbol = source name (Decision #10). Slice 4 will
        // add the `section ".interrupts"` and target-specific calling
        // convention (`cc 87` for ARM thumb-cc, etc.).
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

    /// Emit one LLVM function per `#transition` inside this
    /// `#automaton`. Transitions are named `<AutomatonName>_<transition>`
    /// in IR (`Counter_tick` for `#automaton Counter { #transition tick
    /// { … } }`) so there's no name clash across automatons. The
    /// owning-automaton context is set so `Self.field` reads resolve.
    fn emit_automaton_transitions(&mut self, decl: &AutomatonDecl) {
        if decl.address.is_some() {
            // Register-block automaton — slice 4. Transitions on register
            // blocks need the volatile-load/store machinery.
            return;
        }
        for tr in &decl.transitions {
            self.emit_transition(&decl.name, tr);
        }
    }

    fn emit_transition(&mut self, owner: &str, decl: &TransitionDecl) {
        // Reset per-function state.
        self.next_value_id = 0;
        self.locals.clear();
        self.enclosing_owner = Some(owner.to_owned());

        let fn_name = format!("{owner}_{tr}", tr = decl.name);

        // Slice 3: transitions take no value parameters at the AST
        // level (Decision #5 / Refinement #5b restricts transition
        // signatures). The generated IR fn signature is `void`.
        writeln!(&mut self.out, "define void @{fn_name}() {{").ok();
        writeln!(&mut self.out, "entry:").ok();
        self.emit_block(&decl.body, "void");
        writeln!(&mut self.out, "}}").ok();
        writeln!(&mut self.out).ok();

        self.enclosing_owner = None;
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

    fn emit_fn(&mut self, decl: &FnDecl) {
        // Reset per-function state.
        self.next_value_id = 0;
        self.locals.clear();

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
        let mut returned = false;
        for stmt in &block.stmts {
            if returned {
                // Statements after a return are dead — skip silently.
                // Future slice may want to warn; today we trust the
                // upstream check pass to flag truly suspicious code.
                break;
            }
            self.emit_stmt(stmt);
            if matches!(stmt.kind, StmtKind::Return(_)) {
                returned = true;
            }
        }
        if !returned {
            // No explicit return. Either emit `ret void` (if the fn
            // returns unit) or produce a synthetic `unreachable` and
            // surface a NotYetImplemented (since slice-1 doesn't
            // generate unit values from non-return paths yet).
            if ret_ty == "void" {
                writeln!(&mut self.out, "  ret void").ok();
            } else {
                writeln!(&mut self.out, "  unreachable").ok();
                self.errors.push(CodegenError::NotYetImplemented {
                    what: "non-unit @fn body without an explicit `return` statement",
                });
            }
        }
    }

    fn emit_stmt(&mut self, stmt: &Stmt) {
        match &stmt.kind {
            StmtKind::Return(Some(e)) => {
                // Compute the return-value IR type *before* emit_expr
                // takes a mutable borrow on self.
                let ret_ty = self.expr_ir_type(e);
                match self.emit_expr(e) {
                    Ok(v) => {
                        writeln!(&mut self.out, "  ret {ret_ty} {v}").ok();
                    }
                    Err(err) => self.errors.push(err),
                }
            }
            StmtKind::Return(None) => {
                writeln!(&mut self.out, "  ret void").ok();
            }
            StmtKind::Let { name, ty, value, .. } => {
                let ir_ty = match ty {
                    Some(annotated) => self.lower_type(annotated).unwrap_or_else(|e| {
                        self.errors.push(e);
                        "i32".to_owned() // best-effort fallback
                    }),
                    None => self.expr_ir_type(value),
                };
                match self.emit_expr(value) {
                    Ok(v) => {
                        let bind = self.bind_via_identity(&ir_ty, &v);
                        self.locals.push(LocalBinding {
                            name: name.clone(),
                            value: bind,
                            ir_type: ir_ty,
                        });
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
        // Snapshot the field info so we can release the borrow before
        // calling `emit_expr` (which needs `&mut self`).
        let (struct_name, field_data): (String, Vec<(usize, String)>) = {
            let info = self.automatons.get(automaton).ok_or_else(|| {
                CodegenError::UnresolvedName {
                    name: automaton.to_owned(),
                }
            })?;
            if info.is_register_block {
                return Err(CodegenError::NotYetImplemented {
                    what: "#mutate on register-block automaton (volatile-store lowering is slice 4)",
                });
            }
            let struct_name = format!("%struct.{automaton}");
            let mut entries = Vec::with_capacity(assigns.len());
            for fa in assigns {
                let (idx, ir_ty) = info
                    .fields
                    .iter()
                    .enumerate()
                    .find_map(|(i, (n, t))| if n == &fa.field { Some((i, t.clone())) } else { None })
                    .ok_or_else(|| CodegenError::UnresolvedName {
                        name: format!("{automaton}.{}", fa.field),
                    })?;
                entries.push((idx, ir_ty));
            }
            (struct_name, entries)
        };

        for (fa, (idx, ir_ty)) in assigns.iter().zip(field_data.iter()) {
            // Slice 3 punt: array-element index forms (`field[i] =
            // expr`) need a 2-level GEP and aren't part of this
            // slice's mutation surface.
            if fa.index.is_some() {
                return Err(CodegenError::NotYetImplemented {
                    what: "indexed field assignment (#mutate Auto { field[i] = … })",
                });
            }
            let v = self.emit_expr(&fa.value)?;
            let ptr = self.fresh_value();
            writeln!(
                &mut self.out,
                "  {ptr} = getelementptr {struct_name}, {struct_name}* @{automaton}.state, i32 0, i32 {idx}",
                struct_name = struct_name,
                automaton = automaton,
                idx = idx,
            )
            .ok();
            writeln!(
                &mut self.out,
                "  store {ir_ty} {v}, {ir_ty}* {ptr}",
                ir_ty = ir_ty,
                v = v,
                ptr = ptr,
            )
            .ok();
        }
        Ok(())
    }

    /// Slice 3: lower `Auto.field <op>= expr;` sugar.
    fn emit_mutate_short(
        &mut self,
        automaton: &str,
        field: &str,
        op: AssignOp,
        value: &Expr,
    ) -> Result<(), CodegenError> {
        let (struct_name, idx, ir_ty, is_register_block) = {
            let info = self.automatons.get(automaton).ok_or_else(|| {
                CodegenError::UnresolvedName {
                    name: automaton.to_owned(),
                }
            })?;
            let (idx, ir_ty) = info
                .fields
                .iter()
                .enumerate()
                .find_map(|(i, (n, t))| if n == field { Some((i, t.clone())) } else { None })
                .ok_or_else(|| CodegenError::UnresolvedName {
                    name: format!("{automaton}.{field}"),
                })?;
            (
                format!("%struct.{automaton}"),
                idx,
                ir_ty,
                info.is_register_block,
            )
        };
        if is_register_block {
            return Err(CodegenError::NotYetImplemented {
                what: "mutation-sugar on register-block field (volatile-store is slice 4)",
            });
        }

        let new_value = self.emit_expr(value)?;
        let ptr = self.fresh_value();
        writeln!(
            &mut self.out,
            "  {ptr} = getelementptr {struct_name}, {struct_name}* @{automaton}.state, i32 0, i32 {idx}",
            struct_name = struct_name,
            automaton = automaton,
            idx = idx,
        )
        .ok();

        // For `=`, emit a plain store. For `<op>=`, load the current
        // value, apply the op, store the result.
        let final_value = if matches!(op, AssignOp::Eq) {
            new_value
        } else {
            let cur = self.fresh_value();
            writeln!(
                &mut self.out,
                "  {cur} = load {ir_ty}, {ir_ty}* {ptr}",
                ir_ty = ir_ty,
                ptr = ptr,
            )
            .ok();
            let opcode = compound_assign_opcode(op, &ir_ty);
            let combined = self.fresh_value();
            writeln!(
                &mut self.out,
                "  {combined} = {opcode} {ir_ty} {cur}, {new_value}",
                opcode = opcode,
                ir_ty = ir_ty,
                cur = cur,
                new_value = new_value,
            )
            .ok();
            combined
        };
        writeln!(
            &mut self.out,
            "  store {ir_ty} {final_value}, {ir_ty}* {ptr}",
            ir_ty = ir_ty,
            final_value = final_value,
            ptr = ptr,
        )
        .ok();
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

        // Decide the call symbol: if the proc resolves as a transition
        // of the *current* enclosing owner, namespace it. Otherwise
        // emit the bare name (effect / interrupt — both use their
        // source name as the linker symbol).
        //
        // Slice-3 simplification: we don't yet round-trip through
        // `Resolution::BindingRef::Proc { ctx, … }` to pick effect-
        // vs-transition shape. Heuristic: if `enclosing_owner` is set
        // AND the name matches one of that automaton's transitions,
        // emit `<owner>_<name>`. Otherwise emit `<name>`. This is
        // correct for the canonical case of a transition calling
        // another transition of the same automaton, and for any
        // effect/interrupt call site.
        let mangled = match self.enclosing_owner.as_deref() {
            Some(owner) => {
                let owns_transition = self
                    .resolution
                    .lookup(stmt.span)
                    .and_then(|b| match b {
                        clifford_resolve::BindingRef::Proc { name: callee_name, ctx, .. } => {
                            if matches!(ctx, clifford_resolve::CallContext::Transition) {
                                Some(callee_name.clone())
                            } else {
                                None
                            }
                        }
                        _ => None,
                    })
                    .map(|callee| callee == name)
                    .unwrap_or(false);
                if owns_transition {
                    format!("{owner}_{name}")
                } else {
                    name.to_owned()
                }
            }
            None => name.to_owned(),
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
                    self.lookup_local(&segments[0])
                        .map(str::to_owned)
                        .ok_or_else(|| CodegenError::UnresolvedName {
                            name: segments[0].clone(),
                        })
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
            other => Err(CodegenError::NotYetImplemented {
                what: expr_kind_name(other),
            }),
        }
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
                    what: "non-path receiver in FieldAccess (slice 3 supports Auto.field / Self.field)",
                });
            }
        };

        let (struct_name, idx, ir_ty) = {
            let info = self.automatons.get(&auto_name).ok_or_else(|| {
                CodegenError::UnresolvedName {
                    name: auto_name.clone(),
                }
            })?;
            if info.is_register_block {
                return Err(CodegenError::NotYetImplemented {
                    what: "field read on register-block automaton (volatile-load is slice 4)",
                });
            }
            let (idx, ir_ty) = info
                .fields
                .iter()
                .enumerate()
                .find_map(|(i, (n, t))| if n == field { Some((i, t.clone())) } else { None })
                .ok_or_else(|| CodegenError::UnresolvedName {
                    name: format!("{auto_name}.{field}"),
                })?;
            (format!("%struct.{auto_name}"), idx, ir_ty)
        };

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
            other => {
                return Err(CodegenError::NotYetImplemented {
                    what: binary_op_name(other),
                });
            }
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
            ExprKind::Binary { lhs, .. } => self.expr_ir_type(lhs),
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

    fn lookup_local(&self, name: &str) -> Option<&str> {
        // Search in reverse so inner-scope shadowing wins.
        self.locals
            .iter()
            .rev()
            .find_map(|b| if b.name == name { Some(b.value.as_str()) } else { None })
    }

    fn lookup_local_ir_type(&self, name: &str) -> Option<String> {
        self.locals
            .iter()
            .rev()
            .find_map(|b| if b.name == name { Some(b.ir_type.clone()) } else { None })
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
    fn unsupported_expression_emits_e0810() {
        // Tuple expressions don't lower in slice 1.
        let src = "@fn t() { let _x := (1u32, 2u32); return; }";
        let errors = lower_str(src).expect_err("expected E0810 for tuple");
        let saw = errors.iter().any(|e| {
            matches!(e, CodegenError::NotYetImplemented { what }
                if *what == "tuple expression")
        });
        assert!(saw, "expected NotYetImplemented(tuple); got {errors:?}");
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
    fn s3_register_block_field_access_emits_e0810() {
        let src = "\
            #automaton Mmio { #address: 0x4000_0000; ctl: u32 #offset: 0x00; }\n\
            #effect r() #mutates: [Mmio] { let _x: u32 = Mmio.ctl; return; }\n\
        ";
        let errors = lower_str(src).expect_err("expected E0810 for register-block read");
        let saw = errors.iter().any(|e| matches!(
            e,
            CodegenError::NotYetImplemented { what }
                if what.contains("register-block")
        ));
        assert!(saw, "expected NotYetImplemented(register-block); got {errors:?}");
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
}
