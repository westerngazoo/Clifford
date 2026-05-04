//! # clifford-ortho
//!
//! The GA Orthogonality Engine — the heart of Clifford. Implements §7 of
//! `docs/CLIFFORD_SPEC.md` and the product-existence theorem stated formally
//! in Appendix B.
//!
//! ## What this crate proves
//!
//! For every pair of computations (X, Y) where `can_concur(X, Y)`, the engine
//! verifies the wedge-product orthogonality condition:
//!
//! ```text
//! behavior(X) ∧ behavior(Y) ≠ 0  of grade  |behavior(X)| + |behavior(Y)|
//! ```
//!
//! Per Emergent Rule 6, this wedge-product check is the *constructive existence
//! proof* for the product category C_X × C_Y in Clifford's small-category
//! interpretation of automata (Decision #5 + Appendix B). It is not an
//! algorithmic shortcut; it is the theorem.
//!
//! ## Why this crate is special
//!
//! Per CLAUDE.md §4 ("The GA Orthogonality Engine — Special Standards"):
//!
//! - **100% line + branch coverage required** (track via `cargo-llvm-cov`).
//! - **Property tests required** for every public function (via `proptest`).
//! - **Two reviewers** for every PR — not just one.
//! - **Error messages name original source identifiers**, never raw `e_n`
//!   indices (unless `--verbose-basis` is on).
//! - **Every transformation preserves a documented invariant.** State the
//!   invariant in a comment, then test it.
//! - **No "optimisation" without a benchmark.** The XOR-bitmask representation
//!   is already O(1); resist cleverness.
//!
//! ## Implementation status
//!
//! **Slice 1 (this PR):** the v0.1 Cl(0,0,n) bitmask engine, end-to-end.
//! Public entry [`check_orthogonality`] consumes the [`MutationProfiles`]
//! produced by `clifford-effect` and verifies the §7.4 wedge-product
//! condition for every concurrent pair of callables. Basis-vector
//! assignment uses sequential integer indices (Decision #4
//! auto-assignment, no explicit `#basis` overrides yet); behavior
//! multivectors are constructed per §7.2 (one blade per callable, the
//! wedge of all `(automaton, field)` pairs the callable writes
//! transitively); concurrency inference is the §7.3 conservative
//! heuristic (every effect/interrupt pair can_concur unless excluded
//! by `@sequential`); error reporting (§7.5) names the conflicting
//! callables and shared fields by source identifier — never raw `e_n`
//! indices, per Emergent Rule 1.
//!
//! Read-write race detection (§7.2 v0.2 work), explicit `#basis`
//! annotation honouring (§7.6), trait basis vectors (§7.1 second half),
//! `--verbose-basis` debug rendering (§7.7), and the v0.7 mixed-metric
//! extension for shared automata (Decision #21, §7.0/§7.9) all arrive
//! in subsequent slices.

#![forbid(unsafe_code)]
#![warn(missing_docs)]

use std::collections::{BTreeMap, HashMap, HashSet};

use clifford_ast::{Item, Program, SequentialAttr};
use clifford_effect::{CallableId, FieldRef, MutationProfiles};
use clifford_lexer::Span;
use thiserror::Error;

/// Errors produced by the orthogonality engine.
///
/// Reserves the `E05xx` range. The defining error is `E0520: orthogonality
/// violation` — its message names the conflicting callables and the shared
/// fields by source identifier per §7.5 / Emergent Rule 1.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum OrthoError {
    /// Two concurrent callables share at least one `(automaton, field)`
    /// write — the wedge product collapses to zero.
    ///
    /// Per §7.5 the diagnostic must name (1) both callables, (2) the
    /// specific shared fields decoded from the bitmask back to source
    /// identifiers, never `e_n` indices.
    #[error("E0520: orthogonality violation between `{callable_a}` and `{callable_b}`: both write {shared_fields_display}")]
    OrthogonalityViolation {
        /// First callable's name.
        callable_a: String,
        /// Second callable's name.
        callable_b: String,
        /// The `(automaton, field)` pairs the two callables both write.
        shared_fields: Vec<FieldRef>,
        /// Pre-formatted display string for the shared fields (e.g.
        /// `"`Counter.value`"` or `"`Counter.value`, `Logger.last`"`).
        /// Pre-rendering avoids re-formatting at error-rendering time.
        shared_fields_display: String,
    },

    /// The program declares more than 64 distinct `(automaton, field)`
    /// pairs across all `actual_writes` sets. The v0.1 engine packs each
    /// basis vector into one bit of a `u64`; v0.2 will switch to a
    /// `Vec<u64>` arbitrary-width representation.
    ///
    /// Diagnostic includes the count so users know how much they're over.
    #[error("E0530: program exceeds the v0.1 basis-vector capacity (got {found}, max {max}); a future minor will lift this limit via wide-blade representation")]
    TooManyBasisVectors {
        /// Number of distinct fields encountered.
        found: usize,
        /// The current cap (64 for v0.1).
        max: usize,
    },
}

/// XOR-bitmask wedge-product on two blades.
///
/// Returns `Some(a | b)` when the wedge is non-zero (no shared basis vector),
/// `None` when the wedge is zero (some basis vector squared).
///
/// This is the algorithmic core of Clifford's concurrency safety proof. Per
/// Emergent Rule 6 it is the constructive existence test for the
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

// ─── Basis assignment (§7.1) ────────────────────────────────────────────────

/// The maximum number of `(automaton, field)` basis vectors a v0.1 program
/// may declare. Equals `u64::BITS` because each basis vector occupies one
/// bit of the bitmask blade representation. v0.2's wide-blade `Vec<u64>`
/// representation lifts this cap.
pub const MAX_BASIS_VECTORS_V1: usize = 64;

/// The basis-vector assignment for one program — bidirectional mapping
/// between `(automaton, field)` pairs and bit positions in the blade.
///
/// Per §7.1 / Decision #4: assignment is auto-by-the-compiler in source
/// order. Explicit `#basis: name` overrides (§7.6) are slice O2+ work.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct BasisAssignment {
    /// Forward map: `(automaton, field)` → bit position in the blade.
    by_field: HashMap<FieldRef, u32>,
    /// Reverse map: bit position → `(automaton, field)`. Indexed by bit
    /// position; `by_index[i]` is the field at bit `i`.
    by_index: Vec<FieldRef>,
}

impl BasisAssignment {
    /// Look up the bit position for a field.
    #[must_use]
    pub fn bit(&self, field: &FieldRef) -> Option<u32> {
        self.by_field.get(field).copied()
    }

    /// Look up the field at a bit position.
    #[must_use]
    pub fn field(&self, bit: u32) -> Option<&FieldRef> {
        self.by_index.get(bit as usize)
    }

    /// Total number of basis vectors assigned.
    #[must_use]
    pub fn len(&self) -> usize {
        self.by_index.len()
    }

    /// True if no basis vectors have been assigned.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.by_index.is_empty()
    }

    /// Iterate over `(field, bit)` pairs in bit order.
    pub fn all(&self) -> impl Iterator<Item = (&FieldRef, u32)> {
        self.by_index
            .iter()
            .enumerate()
            .map(|(i, fr)| (fr, i as u32))
    }
}

/// Build the basis assignment from a [`MutationProfiles`] table.
///
/// Sweeps every callable's `actual_writes`, collecting unique
/// `(automaton, field)` pairs into a deterministic order: sorted first by
/// automaton name, then by field name. This makes the assignment stable
/// across builds — important for reproducible diagnostics and golden-file
/// tests (CLAUDE.md §3.5).
///
/// # Errors
///
/// Returns `Err(OrthoError::TooManyBasisVectors)` when the unique-field
/// count exceeds [`MAX_BASIS_VECTORS_V1`] (currently 64).
pub fn assign_basis(profiles: &MutationProfiles) -> Result<BasisAssignment, OrthoError> {
    // Collect every unique field across every callable's writes.
    let mut all_fields: BTreeMap<(String, String), FieldRef> = BTreeMap::new();
    for (_, profile) in profiles.all() {
        for fr in &profile.actual_writes {
            all_fields
                .entry((fr.automaton.clone(), fr.field.clone()))
                .or_insert_with(|| fr.clone());
        }
    }

    if all_fields.len() > MAX_BASIS_VECTORS_V1 {
        return Err(OrthoError::TooManyBasisVectors {
            found: all_fields.len(),
            max: MAX_BASIS_VECTORS_V1,
        });
    }

    // BTreeMap iteration is in sorted key order — deterministic.
    let mut by_field: HashMap<FieldRef, u32> = HashMap::new();
    let mut by_index: Vec<FieldRef> = Vec::with_capacity(all_fields.len());
    for (idx, (_, fr)) in all_fields.into_iter().enumerate() {
        by_field.insert(fr.clone(), idx as u32);
        by_index.push(fr);
    }

    Ok(BasisAssignment { by_field, by_index })
}

// ─── Behavior multivectors (§7.2) ───────────────────────────────────────────

/// One blade — the outer product of basis vectors a single callable writes.
///
/// In the bitmask representation, a blade is just a `u64` whose set bits
/// correspond to the basis vectors in the wedge. The grade of the blade is
/// `bits.count_ones()`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Blade {
    /// Bitmask of basis vectors in this blade.
    pub bits: u64,
}

impl Blade {
    /// The grade of the blade — the number of basis vectors in the wedge.
    #[must_use]
    pub fn grade(self) -> u32 {
        self.bits.count_ones()
    }

    /// True if this is the empty blade (the multiplicative identity).
    #[must_use]
    pub fn is_empty(self) -> bool {
        self.bits == 0
    }
}

/// The behavior of one callable — its blade in the bitmask algebra.
///
/// Per §7.2: `behavior(E) = ∧ { basis(A, f) | (A, f) ∈ E.actual_writes }`.
/// We model each callable as a single blade (one wedge of all its writes);
/// the spec's "behavior multivector = sum of blades over an automaton's
/// effects" formulation is for the per-automaton aggregate, which the §7.4
/// pairwise check implicitly handles by checking each callable individually.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CallableBehavior {
    /// The blade summarising every field this callable writes (transitively
    /// per §6.2 / E2).
    pub blade: Blade,
}

/// Build the behavior multivector for every callable in the program.
///
/// One [`CallableBehavior`] per callable, derived from its [`MutationProfile`]
/// using the basis assignment.
///
/// [`MutationProfile`]: clifford_effect::MutationProfile
#[must_use]
pub fn build_behaviors(
    profiles: &MutationProfiles,
    basis: &BasisAssignment,
) -> HashMap<CallableId, CallableBehavior> {
    let mut out: HashMap<CallableId, CallableBehavior> = HashMap::new();
    for (id, profile) in profiles.all() {
        let mut bits: u64 = 0;
        for fr in &profile.actual_writes {
            // Every field in profile.actual_writes was seen by assign_basis,
            // so the bit must exist. Defensively skip on miss to avoid panic.
            if let Some(b) = basis.bit(fr) {
                bits |= 1u64 << b;
            }
        }
        out.insert(id.clone(), CallableBehavior { blade: Blade { bits } });
    }
    out
}

// ─── Concurrency inference (§7.3) ───────────────────────────────────────────

/// Which pairs of callables can concur, per §7.3's sound-conservative
/// heuristic.
///
/// v0.1 uses a *fully-conservative* baseline: every pair of effects /
/// interrupts can concur, *except* pairs declared non-concurrent via
/// `@sequential(A, B)` per Decision #11. Transitions are not considered
/// directly — their writes appear in their callers' transitive
/// `actual_writes` sets, so the per-callable check catches transition-level
/// races through their callers.
///
/// Refinements available in later slices:
/// - per-thread analysis (different threads → can_concur)
/// - interrupt-priority handling (higher pri preempts lower)
/// - same-`#mutates`-set effects called from one thread → not concurrent
///
/// For v0.1, "everything can race with everything unless told otherwise"
/// is the safe default.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ConcurrencyMatrix {
    /// Set of unordered pairs `{A, B}` where A and B can concur. Stored as
    /// canonical-ordered tuples (smaller key first) to avoid duplicate
    /// entries.
    pairs: HashSet<(CallableId, CallableId)>,
}

impl ConcurrencyMatrix {
    /// True if A and B can concur per the v0.1 heuristic.
    #[must_use]
    pub fn can_concur(&self, a: &CallableId, b: &CallableId) -> bool {
        self.pairs
            .contains(&canonical_pair(a.clone(), b.clone()))
    }

    /// Iterate over every concurrent pair (in canonical order).
    pub fn pairs(&self) -> impl Iterator<Item = &(CallableId, CallableId)> {
        self.pairs.iter()
    }

    /// Number of concurrent pairs recorded.
    #[must_use]
    pub fn len(&self) -> usize {
        self.pairs.len()
    }

    /// True if no concurrent pairs were recorded.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.pairs.is_empty()
    }
}

/// Build the v0.1 concurrency matrix.
///
/// Includes every effect-effect, effect-interrupt, and interrupt-interrupt
/// pair, *minus* pairs excluded by `@sequential(A, B)` attributes. The
/// `@sequential` attribute names two automata; we exclude pairs of effects
/// whose `actual_automata` sets are subsets of `{A}` and `{B}` respectively
/// (the precise rule: a pair (E1, E2) is excluded when there exist
/// automata X, Y with `@sequential(X, Y)` such that E1 mutates X and E2
/// mutates Y, or vice versa, and neither E1 nor E2 mutates anything
/// outside {X, Y}).
///
/// Slice O1's interpretation is the simplest sound one: exclude (E1, E2)
/// if the `@sequential` graph names a pair (X, Y) that exactly captures
/// the only automata E1 and E2 touch.
#[must_use]
pub fn build_concurrency_matrix(
    profiles: &MutationProfiles,
    sequential_pairs: &[(String, String)],
) -> ConcurrencyMatrix {
    let mut pairs: HashSet<(CallableId, CallableId)> = HashSet::new();
    let callables: Vec<&CallableId> = profiles
        .all()
        .map(|(id, _)| id)
        .filter(|id| matches!(id, CallableId::Effect(_) | CallableId::Interrupt(_)))
        .collect();

    for i in 0..callables.len() {
        for j in (i + 1)..callables.len() {
            let a = callables[i];
            let b = callables[j];
            if !is_excluded_by_sequential(a, b, profiles, sequential_pairs) {
                pairs.insert(canonical_pair(a.clone(), b.clone()));
            }
        }
    }

    ConcurrencyMatrix { pairs }
}

fn is_excluded_by_sequential(
    a: &CallableId,
    b: &CallableId,
    profiles: &MutationProfiles,
    sequential_pairs: &[(String, String)],
) -> bool {
    let Some(profile_a) = profiles.lookup(a) else {
        return false;
    };
    let Some(profile_b) = profiles.lookup(b) else {
        return false;
    };

    let a_set = &profile_a.actual_automata;
    let b_set = &profile_b.actual_automata;

    for (x, y) in sequential_pairs {
        // The strict v0.1 rule: (a, b) is excluded by `@sequential(X, Y)`
        // iff *each side* touches exactly one of {X, Y} and they touch
        // different sides. If either side touches more than just X-or-Y
        // (or both), the @sequential pair doesn't capture all the
        // automata involved, so we cannot exclude the pair safely — there
        // could be a race through some *third* automaton this attribute
        // says nothing about.
        let a_is_just_x = a_set.len() == 1 && a_set.contains(x);
        let a_is_just_y = a_set.len() == 1 && a_set.contains(y);
        let b_is_just_x = b_set.len() == 1 && b_set.contains(x);
        let b_is_just_y = b_set.len() == 1 && b_set.contains(y);

        if (a_is_just_x && b_is_just_y) || (a_is_just_y && b_is_just_x) {
            return true;
        }
    }
    false
}

fn canonical_pair(a: CallableId, b: CallableId) -> (CallableId, CallableId) {
    // Stable canonical order: by Debug formatting (works for any Eq type).
    // Cheap and deterministic; we only call it during set construction.
    if format!("{:?}", a) <= format!("{:?}", b) {
        (a, b)
    } else {
        (b, a)
    }
}

/// Extract every `@sequential(A, B);` attribute from the program as a list
/// of automaton-name pairs. Order within a pair is preserved; cross-pair
/// order matches source order.
#[must_use]
pub fn collect_sequential_pairs(program: &Program) -> Vec<(String, String)> {
    program
        .items
        .iter()
        .filter_map(|item| {
            if let Item::Sequential(SequentialAttr { a, b, .. }) = item {
                Some((a.clone(), b.clone()))
            } else {
                None
            }
        })
        .collect()
}

// ─── Orthogonality check (§7.4) ─────────────────────────────────────────────

/// The complete orthogonality report produced by [`check_orthogonality`].
///
/// Carries the basis assignment, per-callable behaviors, the concurrency
/// matrix, and any violations found. Returning a structured report (rather
/// than just a `Result<(), Vec<OrthoError>>`) lets downstream consumers
/// (the CLI driver, IDE integrations, `cliffordc audit`) inspect the
/// internal state without re-running the engine.
#[derive(Debug, Clone, Default)]
pub struct OrthoReport {
    /// The basis-vector assignment used.
    pub basis: BasisAssignment,
    /// Per-callable behavior blades.
    pub behaviors: HashMap<CallableId, CallableBehavior>,
    /// The concurrency matrix.
    pub concurrency: ConcurrencyMatrix,
    /// Violations found, if any. Empty on a clean program.
    pub errors: Vec<OrthoError>,
}

/// Run the v0.1 orthogonality check end-to-end.
///
/// Pipeline:
///
/// 1. Assign basis vectors to every `(automaton, field)` pair appearing in
///    any callable's `actual_writes` set ([`assign_basis`]). Returns
///    `E0530 TooManyBasisVectors` if the program exceeds 64 distinct fields.
/// 2. Build the per-callable behavior blade for every callable
///    ([`build_behaviors`]).
/// 3. Build the concurrency matrix ([`build_concurrency_matrix`]) using
///    every `@sequential(A, B)` attribute in the program for exclusion.
/// 4. For every concurrent pair, compute `outer_product(blade_a, blade_b)`.
///    On `None` (collapse to zero), record an `E0520 OrthogonalityViolation`
///    naming both callables and the shared fields by source identifier
///    (Emergent Rule 1).
///
/// The returned [`OrthoReport`] always carries the basis, behaviors, and
/// matrix — they're computed unconditionally and useful for downstream
/// consumers regardless of whether the program type-checked.
///
/// # Errors
///
/// Returns `Err(OrthoError::TooManyBasisVectors)` when assignment fails.
/// Per-pair `E0520` violations are returned in `report.errors`; callers
/// should treat any non-empty `errors` as a compile failure.
///
/// # Examples
///
/// ```
/// use clifford_lexer::tokenize;
/// use clifford_parser::parse;
/// use clifford_resolve::resolve;
/// use clifford_effect::extract_mutation_profiles;
/// use clifford_ortho::check_orthogonality;
///
/// // Two effects writing the same field — should fail orthogonality.
/// let src = "\
///   #automaton Counter { value: u32; }\n\
///   #effect bump() #mutates: [Counter] { Counter.value = 1u32; }\n\
///   #effect zap()  #mutates: [Counter] { Counter.value = 0u32; }\n\
/// ";
/// let tokens = tokenize(src).unwrap();
/// let program = parse(&tokens).unwrap();
/// let resolution = resolve(&program).unwrap();
/// let profiles = extract_mutation_profiles(&program, &resolution).unwrap();
/// let report = check_orthogonality(&program, &profiles).unwrap();
/// // Both bump and zap write Counter.value → pair conflicts.
/// assert!(!report.errors.is_empty());
/// ```
pub fn check_orthogonality(
    program: &Program,
    profiles: &MutationProfiles,
) -> Result<OrthoReport, OrthoError> {
    let basis = assign_basis(profiles)?;
    let behaviors = build_behaviors(profiles, &basis);
    let sequential_pairs = collect_sequential_pairs(program);
    let concurrency = build_concurrency_matrix(profiles, &sequential_pairs);

    let mut errors: Vec<OrthoError> = Vec::new();
    for (a, b) in concurrency.pairs() {
        let blade_a = behaviors
            .get(a)
            .map(|c| c.blade)
            .unwrap_or(Blade { bits: 0 });
        let blade_b = behaviors
            .get(b)
            .map(|c| c.blade)
            .unwrap_or(Blade { bits: 0 });
        if outer_product(blade_a.bits, blade_b.bits).is_none() {
            let shared_bits = blade_a.bits & blade_b.bits;
            let shared_fields = decode_shared_bits(shared_bits, &basis);
            let display = format_shared_fields(&shared_fields);
            errors.push(OrthoError::OrthogonalityViolation {
                callable_a: callable_display(a),
                callable_b: callable_display(b),
                shared_fields,
                shared_fields_display: display,
            });
        }
    }

    Ok(OrthoReport {
        basis,
        behaviors,
        concurrency,
        errors,
    })
}

/// Decode a bitmask of shared basis vectors back into source-identifier
/// `FieldRef`s (per Emergent Rule 1).
fn decode_shared_bits(shared: u64, basis: &BasisAssignment) -> Vec<FieldRef> {
    let mut out: Vec<FieldRef> = Vec::new();
    let mut bits = shared;
    while bits != 0 {
        let i = bits.trailing_zeros();
        if let Some(fr) = basis.field(i) {
            out.push(fr.clone());
        }
        bits &= bits - 1; // clear lowest set bit
    }
    out
}

fn format_shared_fields(fields: &[FieldRef]) -> String {
    let parts: Vec<String> = fields
        .iter()
        .map(|fr| format!("`{}.{}`", fr.automaton, fr.field))
        .collect();
    parts.join(", ")
}

fn callable_display(id: &CallableId) -> String {
    match id {
        CallableId::Effect(n) => format!("#effect {n}"),
        CallableId::Interrupt(n) => format!("#interrupt {n}"),
        CallableId::Transition { automaton, name } => {
            format!("#transition {automaton}::{name}")
        }
    }
}

// Suppress an unused-import warning when no per-callable spans are needed
// in this slice; placeholder for §7.5 diagnostics that will want them.
#[allow(dead_code)]
fn _span_unused(s: Span) -> usize {
    s.start
}

#[cfg(test)]
mod tests {
    use super::*;
    use clifford_effect::extract_mutation_profiles;
    use clifford_lexer::tokenize;
    use clifford_parser::parse;
    use clifford_resolve::resolve;

    // ── outer_product (kept from scaffolding) ───────────────────────────

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
    fn outer_product_invariant_on_specific_cases() {
        // Mandatory invariant per CLAUDE.md §4.1:
        //     outer_product(a, b).is_some() ⟺ a & b == 0
        // Spot-check a small sample (full proptest lands when the
        // toolchain bumps to permit `proptest`).
        for a in 0u64..16 {
            for b in 0u64..16 {
                assert_eq!(outer_product(a, b).is_some(), (a & b) == 0);
            }
        }
    }

    // ── End-to-end pipeline helper ──────────────────────────────────────

    fn check_str(src: &str) -> Result<OrthoReport, OrthoError> {
        let tokens = tokenize(src).expect("tokenize");
        let program = parse(&tokens).expect("parse");
        let resolution = resolve(&program).expect("resolve");
        let profiles = extract_mutation_profiles(&program, &resolution).expect("profiles");
        check_orthogonality(&program, &profiles)
    }

    // ── Empty programs ──────────────────────────────────────────────────

    #[test]
    fn empty_program_orthogonality_is_clean() {
        let report = check_str("").unwrap();
        assert!(report.basis.is_empty());
        assert!(report.behaviors.is_empty());
        assert!(report.concurrency.is_empty());
        assert!(report.errors.is_empty());
    }

    #[test]
    fn single_effect_no_concurrency() {
        let report = check_str(
            "#automaton C { v: u32; } \
             #effect e() #mutates: [C] { C.v = 1u32; }",
        )
        .unwrap();
        // Single callable has no pair to check.
        assert_eq!(report.basis.len(), 1);
        assert_eq!(report.behaviors.len(), 1);
        assert!(report.concurrency.is_empty());
        assert!(report.errors.is_empty());
    }

    // ── The headline check: write-write race detected ──────────────────

    #[test]
    fn two_effects_writing_same_field_is_e0520() {
        let report = check_str(
            "#automaton Counter { value: u32; } \
             #effect bump() #mutates: [Counter] { Counter.value = 1u32; } \
             #effect zap()  #mutates: [Counter] { Counter.value = 0u32; }",
        )
        .unwrap();
        assert_eq!(report.errors.len(), 1, "expected one violation, got {:?}", report.errors);
        match &report.errors[0] {
            OrthoError::OrthogonalityViolation {
                callable_a,
                callable_b,
                shared_fields,
                shared_fields_display,
            } => {
                assert!(
                    (callable_a.contains("bump") && callable_b.contains("zap"))
                        || (callable_a.contains("zap") && callable_b.contains("bump")),
                    "unexpected pair: {callable_a} / {callable_b}"
                );
                assert_eq!(shared_fields.len(), 1);
                assert_eq!(shared_fields[0].automaton, "Counter");
                assert_eq!(shared_fields[0].field, "value");
                assert!(shared_fields_display.contains("Counter.value"));
            }
            _ => panic!("wrong error variant"),
        }
    }

    // ── Disjoint writes pass ────────────────────────────────────────────

    #[test]
    fn two_effects_disjoint_fields_is_clean() {
        let report = check_str(
            "#automaton Counter { value: u32; flags: u8; } \
             #effect set_value() #mutates: [Counter] { Counter.value = 1u32; } \
             #effect set_flags() #mutates: [Counter] { Counter.flags = 0u8; }",
        )
        .unwrap();
        assert!(
            report.errors.is_empty(),
            "expected no violations; got {:?}",
            report.errors
        );
    }

    // ── Disjoint automata pass ──────────────────────────────────────────

    #[test]
    fn effects_on_different_automata_are_clean() {
        let report = check_str(
            "#automaton A { x: u32; } \
             #automaton B { y: u32; } \
             #effect bump_a() #mutates: [A] { A.x = 1u32; } \
             #effect bump_b() #mutates: [B] { B.y = 1u32; }",
        )
        .unwrap();
        assert!(report.errors.is_empty(), "got {:?}", report.errors);
    }

    // ── Interrupt vs effect race ────────────────────────────────────────

    #[test]
    fn interrupt_vs_effect_writing_same_field_is_e0520() {
        let report = check_str(
            "#automaton Counter { value: u32; } \
             #effect bump() #mutates: [Counter] { Counter.value = 1u32; } \
             #interrupt UART_RX() #mutates: [Counter] #priority: HIGH { \
               Counter.value = Counter.value + 1u32; \
             }",
        )
        .unwrap();
        assert_eq!(report.errors.len(), 1);
    }

    // ── Transitive write through proc-call detected ─────────────────────

    #[test]
    fn transitive_write_through_proc_call_caught() {
        // bumper() and zapper() both transitively touch Counter.value via
        // #> tick() and #> reset() respectively; both go through tick which
        // is the only place the field is directly written.
        let report = check_str(
            "#automaton Counter { value: u32; \
             #transition tick { Counter.value = Counter.value + 1u32; } \
             #transition reset { Counter.value = 0u32; } \
             } \
             #effect bumper() #mutates: [Counter] { #> tick(); } \
             #effect zapper() #mutates: [Counter] { #> reset(); }",
        )
        .unwrap();
        // Both bumper and zapper transitively write Counter.value → conflict.
        assert!(
            !report.errors.is_empty(),
            "expected race via transitive proc-call writes"
        );
        let v = &report.errors[0];
        match v {
            OrthoError::OrthogonalityViolation { shared_fields, .. } => {
                assert!(shared_fields.iter().any(|fr| fr.field == "value"));
            }
            _ => panic!("expected OrthogonalityViolation"),
        }
    }

    // ── @sequential exclusion ────────────────────────────────────────────

    #[test]
    fn sequential_attribute_excludes_pair_from_check() {
        // Without @sequential, bump_a and bump_b (each touching only their
        // own automaton) would still go through the can_concur pair set,
        // but they'd pass the orthogonality check (disjoint). Add a
        // @sequential to confirm exclusion mechanism is wired even when
        // no race exists.
        let report = check_str(
            "#automaton A { x: u32; } \
             #automaton B { y: u32; } \
             @sequential(A, B); \
             #effect ea() #mutates: [A] { A.x = 1u32; } \
             #effect eb() #mutates: [B] { B.y = 1u32; }",
        )
        .unwrap();
        // ea and eb are excluded → no can_concur pair → no errors.
        assert!(report.errors.is_empty());
        assert!(report.concurrency.is_empty(), "expected no concurrent pairs; got {:?}", report.concurrency.pairs);
    }

    #[test]
    fn sequential_attribute_does_not_mask_real_race() {
        // ea writes A.x AND B.y; eb writes B.y. Even if @sequential(A, B)
        // is declared, the (ea, eb) pair is NOT excluded because ea touches
        // both A and B — it doesn't fit the "ea touches only A, eb touches
        // only B" pattern that @sequential captures.
        let report = check_str(
            "#automaton A { x: u32; } \
             #automaton B { y: u32; } \
             @sequential(A, B); \
             #effect ea() #mutates: [A, B] { A.x = 1u32; B.y = 2u32; } \
             #effect eb() #mutates: [B] { B.y = 3u32; }",
        )
        .unwrap();
        // ea and eb both write B.y; @sequential doesn't apply (ea straddles
        // both automata) → race detected.
        assert!(
            !report.errors.is_empty(),
            "expected race; @sequential should not mask cross-touching effects"
        );
    }

    // ── Multiple races collected ────────────────────────────────────────

    #[test]
    fn multiple_pairs_all_reported() {
        let report = check_str(
            "#automaton C { value: u32; } \
             #effect a() #mutates: [C] { C.value = 1u32; } \
             #effect b() #mutates: [C] { C.value = 2u32; } \
             #effect c() #mutates: [C] { C.value = 3u32; }",
        )
        .unwrap();
        // Three pairs: (a,b), (a,c), (b,c). All three race.
        assert_eq!(report.errors.len(), 3);
    }

    // ── Realistic clean program ─────────────────────────────────────────

    #[test]
    fn realistic_clean_program() {
        // Each effect touches its own automaton; no concurrency conflict.
        let report = check_str(
            "#automaton SensorData { reading: u32; } \
             #automaton Pwm { duty: u8; } \
             #automaton Logger { count: u32; } \
             #effect sample()  #mutates: [SensorData] { SensorData.reading = 0u32; } \
             #effect actuate() #mutates: [Pwm] { Pwm.duty = 128u8; } \
             #effect log()     #mutates: [Logger] { Logger.count = Logger.count + 1u32; }",
        )
        .unwrap();
        assert!(report.errors.is_empty(), "got {:?}", report.errors);
    }

    // ── Basis assignment determinism ────────────────────────────────────

    #[test]
    fn basis_assignment_is_deterministic() {
        let src = "#automaton A { x: u32; y: u32; } \
                   #automaton B { z: u32; } \
                   #effect e1() #mutates: [A, B] { A.x = 1u32; A.y = 2u32; B.z = 3u32; }";
        let r1 = check_str(src).unwrap();
        let r2 = check_str(src).unwrap();
        // Each field's bit position must be identical across runs.
        for (fr, bit) in r1.basis.all() {
            assert_eq!(r2.basis.bit(fr), Some(bit), "bit mismatch for {fr:?}");
        }
    }

    #[test]
    fn basis_assignment_is_sorted() {
        // Basis assignment is sorted by (automaton, field). Verify by
        // checking the iteration order.
        let report = check_str(
            "#automaton Z { x: u32; } \
             #automaton A { y: u32; z: u32; } \
             #effect e() #mutates: [A, Z] { A.y = 1u32; A.z = 2u32; Z.x = 3u32; }",
        )
        .unwrap();
        let order: Vec<(String, String)> = report
            .basis
            .all()
            .map(|(fr, _)| (fr.automaton.clone(), fr.field.clone()))
            .collect();
        // Expected: A.y, A.z, Z.x (alphabetical by automaton, then field).
        assert_eq!(
            order,
            vec![
                ("A".to_owned(), "y".to_owned()),
                ("A".to_owned(), "z".to_owned()),
                ("Z".to_owned(), "x".to_owned()),
            ]
        );
    }

    // ── Behavior multivector grade matches write count ──────────────────

    #[test]
    fn behavior_blade_grade_equals_field_count() {
        let report = check_str(
            "#automaton C { x: u32; y: u32; z: u32; } \
             #effect three() #mutates: [C] { C.x = 1u32; C.y = 2u32; C.z = 3u32; }",
        )
        .unwrap();
        let id = CallableId::Effect("three".to_owned());
        let blade = report.behaviors.get(&id).unwrap().blade;
        assert_eq!(blade.grade(), 3);
    }

    // ── Capacity check ──────────────────────────────────────────────────

    #[test]
    fn small_program_within_capacity() {
        let report = check_str(
            "#automaton C { v: u32; } \
             #effect e() #mutates: [C] { C.v = 1u32; }",
        )
        .unwrap();
        assert!(report.basis.len() <= MAX_BASIS_VECTORS_V1);
    }
}
