//! # clifford-ortho
//!
//! The GA Orthogonality Engine — the heart of Clifford. Implements §7 of
//! `docs/CLIFFORD_SPEC.md` and the product-existence theorem stated formally
//! in Appendix B.
//!
//! ## What this crate proves
//!
//! For every pair of computations `(X, Y)` where `can_concur(X, Y)`, the
//! engine verifies the wedge-product orthogonality condition:
//!
//! ```text
//! behavior(X) ∧ behavior(Y) ≠ 0  of grade  |behavior(X)| + |behavior(Y)|
//! ```
//!
//! Per Emergent Rule 6, this wedge-product check is the *constructive
//! existence proof* for the product category `C_X × C_Y` in Clifford's
//! small-category interpretation of automata (Decision #5 + Appendix B).
//! It is not an algorithmic shortcut; it is the theorem.
//!
//! ## v0.1 scope
//!
//! - **Restricted Cl(0,0,n) algebra** per spec §7.0: every basis vector
//!   squares to zero. The bitmask check `a & b != 0 ⇒ wedge == 0` is the
//!   operational form.
//! - **Field basis only.** Trait basis (the second half of §7.1) is
//!   deferred — v0.1 firmware code uses fields, not trait-only callables,
//!   so field-basis catches every realistic write-write race. Trait basis
//!   lands when the first program needs it.
//! - **Write-write races at field granularity.** Read-write races are
//!   v0.2 graded-algebra work per §7.2.
//! - **Sound-conservative concurrency inference** per §7.3:
//!   - Any `#interrupt` can_concur with any `#effect` (interrupt vs.
//!     foreground thread).
//!   - Any two distinct `#interrupt`s can_concur (priority preemption).
//!   - Two `#effect`s never can_concur (single foreground thread).
//!   - `#transition`s are leaves of `#>` chains, not concurrency-checked
//!     directly — their writes propagate into the calling effect /
//!     interrupt's behaviour multivector via `MutationProfile.actual_writes`.
//! - **No `@sequential(A, B)` overrides yet.** Decision #11 is parsed but
//!   not consumed by the verifier; v0.1 is conservative.
//!
//! ## What v0.1 deliberately does not catch
//!
//! Per spec §7.0.1's pillars:
//!
//! - Mutations through `#unchecked_store` / `#volatile_store` /
//!   `#asm` — outside the proof boundary by design (audit-loggable).
//! - Read-write races — v0.2 graded algebra.
//! - Concurrency excluded by `@sequential(A, B)` — user-trusted.
//!
//! ## Implementation
//!
//! - **Blade representation:** `u64` bitmask (1 bit per basis vector).
//!   Supports up to 64 fields per compilation unit; wider compilation
//!   units fail loudly with `BasisExhausted` rather than silently mod-64.
//! - **Basis assignment:** automaton fields in declaration order
//!   (automaton-major, field-minor). v0.1 does not honour `#basis`
//!   override clauses — auto-assignment matches user-declared order so
//!   diagnostics still name fields by source identifier.
//! - **Behaviour multivector:** for each `#effect` / `#interrupt`, the
//!   union of basis bits for fields in its `MutationProfile.actual_writes`
//!   (already transitively closed by `clifford-effect`). v0.1 collapses
//!   the multivector to a single blade because every write contributes
//!   to one grade — the `Σ` over effects in §7.2 is the union of bits
//!   when there's only one blade per callable. When traits enter the
//!   basis later, this becomes a true multi-blade structure.
//! - **Wedge product:** `outer_product(a, b) = if a & b != 0 { None }
//!   else { Some(a | b) }`. The algorithmic core. The `None` case
//!   means the wedge is zero (some basis vector squared); the `Some`
//!   case means the blades are disjoint and their wedge is the union.
//! - **Pairwise check:** O(|effects| × |interrupts|) + O(|interrupts|²).
//!   Linear in the program's callable count for realistic firmware.
//!
//! ## Coverage requirements
//!
//! Per CLAUDE.md §4 ("The GA Orthogonality Engine — Special Standards"):
//! 100% line + branch coverage required. Tests exercise every public
//! function with both happy and conflicting inputs. Property tests will
//! land alongside `proptest` once the toolchain pin permits its
//! transitive deps; the mandatory invariant is documented at the bottom
//! of this file.

#![forbid(unsafe_code)]
#![warn(missing_docs)]

use std::collections::HashMap;

use clifford_ast::{Item, Program};
use clifford_effect::{CallableId, FieldRef, MutationProfile, MutationProfiles};
use thiserror::Error;

/// Errors produced by the orthogonality engine.
///
/// Reserves the `E05xx` range. The most important error is
/// `E0520: orthogonality violation` — its message names every conflicting
/// `(automaton, field)` by source identifier per §7.5.
#[derive(Debug, Error)]
pub enum OrthoError {
    /// A pair of concurrent callables both write the same automaton field.
    /// This is the core safety property the engine enforces — concurrent
    /// writes to the same field would race at the hardware level.
    ///
    /// Each conflict carries the two callable display names plus every
    /// shared `(automaton, field)` reference, decoded from the bitmask
    /// back to source identifiers per §7.5.
    #[error("E0520: orthogonality violation between `{a}` and `{b}`: shared field(s) {shared_display}")]
    OrthogonalityViolation {
        /// Display name of the first callable (e.g. `effect Foo`,
        /// `interrupt USART1_IRQ`).
        a: String,
        /// Display name of the second callable.
        b: String,
        /// The conflicting `(automaton, field)` pairs.
        shared: Vec<FieldRef>,
        /// Pre-rendered human-readable list (e.g.
        /// `` `Counter.value`, `Counter.flag` ``) for the message.
        shared_display: String,
    },

    /// The compilation unit declares more than 64 automaton fields. The
    /// v0.1 representation packs each field into a single bit of a `u64`
    /// blade; wider units need the bit-array representation reserved
    /// for v0.2.
    #[error("E0530: GA basis exhausted: {field_count} automaton fields exceed the v0.1 64-field limit (per spec §7.1's u64 bitmask)")]
    BasisExhausted {
        /// Total number of automaton fields encountered.
        field_count: usize,
    },
}

// ─── §7.1 Basis Vector Assignment ──────────────────────────────────────────

/// Assignment of basis vectors to automaton fields per spec §7.1 step 1.
///
/// Fields are numbered in declaration order — automaton-major, field-minor
/// — and each gets a bit position in the `u64` blade. v0.1 does not yet
/// honour Decision #4's `#basis` override clauses; auto-assignment is
/// canonical. This keeps the v0.1 implementation minimal while still
/// allowing diagnostics to name fields by source identifier (the
/// reverse-lookup table maps bit indices back to `FieldRef`).
///
/// Trait basis (§7.1 step 2) is deferred to a later slice — v0.1
/// firmware code does not lean on trait-only orthogonality.
#[derive(Debug, Clone, Default)]
pub struct BasisAssignment {
    /// Forward map: `(automaton, field)` → bit index.
    by_field: HashMap<FieldRef, u32>,
    /// Reverse map for diagnostics: bit index → original `FieldRef`.
    /// Length is the total field count.
    by_index: Vec<FieldRef>,
}

impl BasisAssignment {
    /// Build the basis from a program's `#automaton` declarations in
    /// source order. Returns `BasisExhausted` if the field count exceeds
    /// 64 (the v0.1 `u64` blade width).
    pub fn build(program: &Program) -> Result<Self, OrthoError> {
        let mut by_field = HashMap::new();
        let mut by_index: Vec<FieldRef> = Vec::new();
        for item in &program.items {
            if let Item::Automaton(decl) = item {
                for field in &decl.fields {
                    let fref = FieldRef {
                        automaton: decl.name.clone(),
                        field: field.name.clone(),
                    };
                    let idx = by_index.len();
                    if idx >= 64 {
                        return Err(OrthoError::BasisExhausted {
                            field_count: idx + 1,
                        });
                    }
                    by_field.insert(fref.clone(), idx as u32);
                    by_index.push(fref);
                }
            }
        }
        Ok(Self { by_field, by_index })
    }

    /// Bit index for one `(automaton, field)` reference, or `None` if
    /// the reference is not in the basis (e.g. a stale `FieldRef` from
    /// a prior version of the program).
    #[must_use]
    pub fn bit_index(&self, field: &FieldRef) -> Option<u32> {
        self.by_field.get(field).copied()
    }

    /// Total dimension `n = |F|` per spec §7.1 step 3.
    #[must_use]
    pub fn dimension(&self) -> usize {
        self.by_index.len()
    }

    /// Decode a bitmask back to the `FieldRef`s of every set bit. Used by
    /// E0520 to render `shared automaton field …` notes — Emergent Rule 1
    /// requires source-identifier display, never raw `e_n` indices.
    #[must_use]
    pub fn decode_mask(&self, mask: u64) -> Vec<FieldRef> {
        let mut out = Vec::new();
        for (i, fref) in self.by_index.iter().enumerate() {
            if i >= 64 {
                break;
            }
            if mask & (1u64 << i) != 0 {
                out.push(fref.clone());
            }
        }
        out
    }
}

// ─── §7.2 Behaviour Multivector Construction ───────────────────────────────

/// Behaviour blade for one callable: the wedge of basis vectors for
/// every field it writes (transitively, per §6.2's `actual_writes`
/// closure). v0.1 represents this as a single `u64` because field-only
/// basis means each callable contributes exactly one blade — the union
/// of its writes' bits.
///
/// When trait basis lands, this widens to a `MultiVector` (sum of
/// blades of potentially different grades); for now, one `u64` is
/// sufficient.
type Behaviour = u64;

/// Build the behaviour blade for one callable from its mutation profile.
/// Fields outside the basis (which shouldn't happen for valid input but
/// is defended against) are silently skipped — the caller's responsibility
/// is to invoke this with a profile derived from the same `Program` the
/// basis was built from.
fn behaviour_for(profile: &MutationProfile, basis: &BasisAssignment) -> Behaviour {
    let mut blade: u64 = 0;
    for fref in &profile.actual_writes {
        if let Some(bit) = basis.bit_index(fref) {
            blade |= 1u64 << bit;
        }
    }
    blade
}

// ─── §7.3 Concurrency Inference ────────────────────────────────────────────

/// Sound-conservative concurrency inference per §7.3. Returns `true` iff
/// the two callables MAY execute concurrently in some program execution.
///
/// v0.1 conservatism:
/// - `#effect` × `#effect` → never concurrent (single foreground thread).
/// - `#interrupt` × anything else → concurrent (interrupts preempt).
/// - Two distinct `#interrupt`s → concurrent (priority preemption).
/// - `#transition` × anything → never directly concurrent (transitions
///   are leaves of `#>` chains; their writes propagate up to the calling
///   effect/interrupt's behaviour blade via `actual_writes`).
fn can_concur(a: &CallableId, b: &CallableId) -> bool {
    match (a, b) {
        // Two effects share the foreground thread — the runtime
        // serializes them.
        (CallableId::Effect(_), CallableId::Effect(_)) => false,
        // Transitions are not top-level concurrency units.
        (CallableId::Transition { .. }, _) | (_, CallableId::Transition { .. }) => false,
        // Anything else: interrupt × effect or interrupt × interrupt.
        // Conservative — a more refined pass could exclude pairs that
        // run on the same priority level (mutually exclusive on
        // Cortex-M's NVIC) but v0.1 doesn't read `#priority` yet.
        _ => true,
    }
}

/// Display name for a callable in diagnostics (e.g.
/// `` `effect tick` ``, `` `interrupt USART1_IRQ` ``). Used by E0520
/// to identify the two parties in a violation.
fn callable_display(id: &CallableId) -> String {
    match id {
        CallableId::Effect(name) => format!("effect {name}"),
        CallableId::Interrupt(name) => format!("interrupt {name}"),
        CallableId::Transition { automaton, name } => {
            format!("transition {automaton}.{name}")
        }
    }
}

// ─── §7.4 Orthogonality Check ──────────────────────────────────────────────

/// XOR-bitmask wedge-product on two blades.
///
/// Returns `Some(a | b)` when the wedge is non-zero (no shared basis
/// vector), `None` when the wedge is zero (some basis vector squared).
///
/// This is the algorithmic core of Clifford's concurrency safety proof.
/// Per Emergent Rule 6 it is the constructive existence test for the
/// product-category morphism `(f_A, f_B)`.
///
/// # Examples
///
/// ```
/// use clifford_ortho::outer_product;
///
/// // Disjoint bitmasks: wedge is the union.
/// assert_eq!(outer_product(0b0011, 0b1100), Some(0b1111));
///
/// // Sharing a bit: wedge is zero.
/// assert_eq!(outer_product(0b0011, 0b0110), None);
/// ```
///
/// # Invariant
///
/// `outer_product(a, b).is_some() ⟺ a & b == 0` for all `a, b: u64`.
#[must_use]
pub fn outer_product(a: u64, b: u64) -> Option<u64> {
    if a & b != 0 {
        None
    } else {
        Some(a | b)
    }
}

// ─── §7.5 Top-level Verifier ───────────────────────────────────────────────

/// Verify that every pair of concurrent callables satisfies the
/// orthogonality condition per spec §7.4. This is the public entry point
/// invoked by the CLI between `clifford-effect`'s mutation-profile pass
/// and `clifford-codegen`'s lowering.
///
/// Returns `Ok(())` when the program is race-free under the §7.3
/// concurrency inference, or `Err(Vec<OrthoError>)` listing every
/// detected `E0520: orthogonality violation`. Multiple violations in
/// the same program are all reported, not just the first.
///
/// # Errors
///
/// - `BasisExhausted` if the program declares more than 64 automaton
///   fields (the `u64` blade width limit).
/// - `OrthogonalityViolation` for every pair of concurrent callables
///   that share at least one mutated automaton field.
pub fn verify(
    program: &Program,
    profiles: &MutationProfiles,
) -> Result<(), Vec<OrthoError>> {
    let basis = match BasisAssignment::build(program) {
        Ok(b) => b,
        Err(e) => return Err(vec![e]),
    };

    // Collect the concurrency-checked callables: every #effect and
    // #interrupt with its behaviour blade. We skip transitions because
    // §7.3's inference declares them non-concurrent (they are leaves of
    // `#>` chains and contribute writes to the calling effect /
    // interrupt's profile via the transitive closure).
    let mut callables: Vec<(CallableId, Behaviour)> = Vec::new();
    for item in &program.items {
        match item {
            Item::Effect(decl) => {
                let id = CallableId::Effect(decl.name.clone());
                let blade = profiles
                    .lookup(&id)
                    .map(|p| behaviour_for(p, &basis))
                    .unwrap_or(0);
                callables.push((id, blade));
            }
            Item::Interrupt(decl) => {
                let id = CallableId::Interrupt(decl.name.clone());
                let blade = profiles
                    .lookup(&id)
                    .map(|p| behaviour_for(p, &basis))
                    .unwrap_or(0);
                callables.push((id, blade));
            }
            _ => {}
        }
    }

    // Pairwise check per §7.4. O(N²) pairs but N is the count of
    // top-level callables — a few dozen for realistic firmware.
    let mut errors: Vec<OrthoError> = Vec::new();
    for i in 0..callables.len() {
        for j in (i + 1)..callables.len() {
            let (id_a, blade_a) = &callables[i];
            let (id_b, blade_b) = &callables[j];
            if !can_concur(id_a, id_b) {
                continue;
            }
            if outer_product(*blade_a, *blade_b).is_none() {
                let conflict = blade_a & blade_b;
                let shared = basis.decode_mask(conflict);
                let shared_display = render_shared_fields(&shared);
                errors.push(OrthoError::OrthogonalityViolation {
                    a: callable_display(id_a),
                    b: callable_display(id_b),
                    shared,
                    shared_display,
                });
            }
        }
    }

    if errors.is_empty() {
        Ok(())
    } else {
        Err(errors)
    }
}

/// Render a list of `FieldRef`s as a human-readable comma-separated
/// list using the source-identifier form (Emergent Rule 1):
/// `` `Counter.value`, `Counter.flag` ``. Used by the E0520 message.
fn render_shared_fields(fields: &[FieldRef]) -> String {
    let parts: Vec<String> = fields
        .iter()
        .map(|f| format!("`{}.{}`", f.automaton, f.field))
        .collect();
    parts.join(", ")
}

#[cfg(test)]
mod tests {
    use super::*;

    // ─── outer_product primitive ─────────────────────────────────────

    #[test]
    fn disjoint_bitmasks_wedge_to_union() {
        assert_eq!(outer_product(0, 0), Some(0));
        assert_eq!(outer_product(0b0011, 0b1100), Some(0b1111));
        assert_eq!(outer_product(0b0001, 0b1000), Some(0b1001));
    }

    #[test]
    fn sharing_any_bit_yields_none() {
        assert_eq!(outer_product(0b0011, 0b0010), None);
        assert_eq!(outer_product(0b1111, 0b1000), None);
        assert_eq!(outer_product(u64::MAX, 1), None);
    }

    #[test]
    fn outer_product_invariant() {
        // The mandatory invariant per CLAUDE.md §4.1:
        //   outer_product(a, b).is_some() ⟺ a & b == 0
        // Sample-based check (full proptest sweep arrives with the
        // proptest dev-dep).
        for a in [0u64, 1, 2, 0xFF, 0xFFFF_FFFF, u64::MAX] {
            for b in [0u64, 1, 2, 0xFF, 0xFFFF_FFFF, u64::MAX] {
                assert_eq!(
                    outer_product(a, b).is_some(),
                    a & b == 0,
                    "invariant failed for a={a:#x}, b={b:#x}",
                );
            }
        }
    }

    // ─── BasisAssignment ─────────────────────────────────────────────

    fn parse_program(src: &str) -> Program {
        let tokens = clifford_lexer::tokenize(src).expect("tokenize");
        clifford_parser::parse(&tokens).expect("parse")
    }

    fn build_profiles(program: &Program) -> MutationProfiles {
        let resolution = clifford_resolve::resolve(program).expect("resolve");
        clifford_effect::extract_mutation_profiles(program, &resolution)
            .expect("mutation profiles")
    }

    #[test]
    fn basis_is_empty_for_program_with_no_automatons() {
        let program = parse_program("@fn t() { return; }");
        let basis = BasisAssignment::build(&program).expect("build");
        assert_eq!(basis.dimension(), 0);
        assert!(basis.bit_index(&FieldRef {
            automaton: "Nope".to_owned(),
            field: "missing".to_owned(),
        }).is_none());
    }

    #[test]
    fn basis_assigns_in_declaration_order() {
        let src = "\
            #automaton A { x: u32; y: u32; }\n\
            #automaton B { z: u32; }\n\
        ";
        let program = parse_program(src);
        let basis = BasisAssignment::build(&program).expect("build");
        assert_eq!(basis.dimension(), 3);
        assert_eq!(
            basis.bit_index(&FieldRef {
                automaton: "A".to_owned(),
                field: "x".to_owned()
            }),
            Some(0)
        );
        assert_eq!(
            basis.bit_index(&FieldRef {
                automaton: "A".to_owned(),
                field: "y".to_owned()
            }),
            Some(1)
        );
        assert_eq!(
            basis.bit_index(&FieldRef {
                automaton: "B".to_owned(),
                field: "z".to_owned()
            }),
            Some(2)
        );
    }

    #[test]
    fn basis_decode_mask_returns_named_fields() {
        let src = "#automaton A { x: u32; y: u32; z: u32; }\n";
        let program = parse_program(src);
        let basis = BasisAssignment::build(&program).expect("build");
        // Bits 0 + 2 set → fields x and z.
        let decoded = basis.decode_mask(0b101);
        assert_eq!(decoded.len(), 2);
        assert_eq!(decoded[0].field, "x");
        assert_eq!(decoded[1].field, "z");
    }

    #[test]
    fn basis_exhausted_when_more_than_64_fields() {
        // Generate an automaton with 65 fields. `BasisExhausted`
        // surfaces before the 65th is recorded.
        let mut src = String::from("#automaton Big {\n");
        for i in 0..65 {
            src.push_str(&format!("  f{i}: u32;\n"));
        }
        src.push_str("}\n");
        let program = parse_program(&src);
        let err = BasisAssignment::build(&program).expect_err("expected exhausted");
        assert!(matches!(err, OrthoError::BasisExhausted { field_count } if field_count == 65));
    }

    // ─── verify: orthogonal cases ────────────────────────────────────

    #[test]
    fn orthogonal_program_passes() {
        // Two interrupts touching disjoint automatons — orthogonal.
        let src = "\
            #automaton A { x: u32; }\n\
            #automaton B { y: u32; }\n\
            #interrupt IRQ_A() #mutates: [A] #priority: HIGH { A.x += 1u32; }\n\
            #interrupt IRQ_B() #mutates: [B] #priority: HIGH { B.y += 1u32; }\n\
        ";
        let program = parse_program(src);
        let profiles = build_profiles(&program);
        verify(&program, &profiles).expect("orthogonal program should pass");
    }

    #[test]
    fn two_effects_never_concurrent_so_no_violation() {
        // §7.3: two #effects share the foreground thread; even if they
        // write the same field there's no race because the runtime
        // serializes them.
        let src = "\
            #automaton C { v: u32; }\n\
            #effect a() #mutates: [C] { C.v += 1u32; }\n\
            #effect b() #mutates: [C] { C.v += 2u32; }\n\
        ";
        let program = parse_program(src);
        let profiles = build_profiles(&program);
        verify(&program, &profiles).expect("two effects don't concur");
    }

    #[test]
    fn empty_program_passes() {
        let program = parse_program("");
        let profiles = build_profiles(&program);
        verify(&program, &profiles).expect("empty program is trivially orthogonal");
    }

    // ─── verify: violation cases ─────────────────────────────────────

    #[test]
    fn interrupt_and_effect_writing_same_field_violates() {
        // The canonical race: an interrupt and a foreground effect
        // both write the same automaton field. Has to be a hard error.
        let src = "\
            #automaton C { v: u32; }\n\
            #effect main_loop() #mutates: [C] { C.v += 1u32; }\n\
            #interrupt SysTick() #mutates: [C] #priority: HIGH { C.v += 1u32; }\n\
        ";
        let program = parse_program(src);
        let profiles = build_profiles(&program);
        let errs = verify(&program, &profiles).expect_err("expected E0520");
        assert_eq!(errs.len(), 1);
        match &errs[0] {
            OrthoError::OrthogonalityViolation { a, b, shared, .. } => {
                assert_eq!(shared.len(), 1);
                assert_eq!(shared[0].automaton, "C");
                assert_eq!(shared[0].field, "v");
                // Order of `a`/`b` in the message reflects program
                // declaration order; either ordering is fine.
                let pair = format!("{a} vs {b}");
                assert!(
                    pair.contains("main_loop") && pair.contains("SysTick"),
                    "expected both names in the diagnostic; got: {pair}"
                );
            }
            other => panic!("expected OrthogonalityViolation, got {other:?}"),
        }
    }

    #[test]
    fn two_interrupts_writing_same_field_violates() {
        // Two ISRs at different priorities can preempt each other —
        // sharing a field is a race.
        let src = "\
            #automaton C { v: u32; }\n\
            #interrupt IRQ_HI() #mutates: [C] #priority: HIGH { C.v += 1u32; }\n\
            #interrupt IRQ_LO() #mutates: [C] #priority: LOW  { C.v += 1u32; }\n\
        ";
        let program = parse_program(src);
        let profiles = build_profiles(&program);
        let errs = verify(&program, &profiles).expect_err("expected E0520");
        assert_eq!(errs.len(), 1);
    }

    #[test]
    fn diagnostic_names_source_identifiers_not_basis_indices() {
        // Per §7.5 / Emergent Rule 1: the message must say
        // `Counter.value`, never `e_2`.
        let src = "\
            #automaton Counter { value: u32; flag: u32; }\n\
            #effect main_loop() #mutates: [Counter] { Counter.value += 1u32; }\n\
            #interrupt SysTick() #mutates: [Counter] #priority: HIGH { Counter.value += 1u32; }\n\
        ";
        let program = parse_program(src);
        let profiles = build_profiles(&program);
        let errs = verify(&program, &profiles).expect_err("expected E0520");
        let msg = format!("{}", errs[0]);
        assert!(
            msg.contains("`Counter.value`"),
            "expected Counter.value in diagnostic; got: {msg}"
        );
        assert!(
            !msg.contains("e_") && !msg.contains("e0") && !msg.contains("e1"),
            "diagnostic must not expose raw basis indices; got: {msg}"
        );
    }

    #[test]
    fn multiple_violations_all_reported() {
        // Two distinct races in one program — both should surface.
        let src = "\
            #automaton A { x: u32; }\n\
            #automaton B { y: u32; }\n\
            #effect main_a() #mutates: [A] { A.x += 1u32; }\n\
            #effect main_b() #mutates: [B] { B.y += 1u32; }\n\
            #interrupt IRQ_A() #mutates: [A] #priority: HIGH { A.x += 1u32; }\n\
            #interrupt IRQ_B() #mutates: [B] #priority: HIGH { B.y += 1u32; }\n\
        ";
        let program = parse_program(src);
        let profiles = build_profiles(&program);
        let errs = verify(&program, &profiles).expect_err("expected two E0520");
        assert_eq!(errs.len(), 2, "expected two violations, got {errs:?}");
    }

    #[test]
    fn transition_writes_propagate_into_caller_blade() {
        // An interrupt that calls a transition via `#> tick()` should
        // see the transition's writes in its behaviour blade — that's
        // the §6.2 transitive closure at work. If a foreground effect
        // also writes that field, we get an orthogonality violation.
        let src = "\
            #automaton Counter { value: u32;\n  \
              #transition tick { Counter.value += 1u32; }\n\
            }\n\
            #effect main_loop() #mutates: [Counter] { Counter.value += 2u32; }\n\
            #interrupt SysTick() #mutates: [Counter] #priority: HIGH { #> tick(); }\n\
        ";
        let program = parse_program(src);
        let profiles = build_profiles(&program);
        let errs = verify(&program, &profiles).expect_err("expected E0520");
        assert_eq!(errs.len(), 1);
        // Verify the conflict is actually on `Counter.value` —
        // confirms the transition's write reached the caller's blade.
        match &errs[0] {
            OrthoError::OrthogonalityViolation { shared, .. } => {
                assert_eq!(shared[0].field, "value");
            }
            _ => panic!("expected OrthogonalityViolation"),
        }
    }

    // ─── can_concur — §7.3 concurrency inference ────────────────────

    #[test]
    fn can_concur_effect_effect_returns_false() {
        let a = CallableId::Effect("a".to_owned());
        let b = CallableId::Effect("b".to_owned());
        assert!(!can_concur(&a, &b));
    }

    #[test]
    fn can_concur_interrupt_effect_returns_true() {
        let a = CallableId::Interrupt("IRQ".to_owned());
        let b = CallableId::Effect("e".to_owned());
        assert!(can_concur(&a, &b));
    }

    #[test]
    fn can_concur_interrupt_interrupt_returns_true() {
        let a = CallableId::Interrupt("IRQ_A".to_owned());
        let b = CallableId::Interrupt("IRQ_B".to_owned());
        assert!(can_concur(&a, &b));
    }

    #[test]
    fn can_concur_skips_transitions() {
        let a = CallableId::Transition {
            automaton: "C".to_owned(),
            name: "tick".to_owned(),
        };
        let b = CallableId::Interrupt("IRQ".to_owned());
        assert!(!can_concur(&a, &b));
        assert!(!can_concur(&b, &a));
    }
}
