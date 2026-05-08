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
//! ## Versions
//!
//! - **v0.1 (shipped 2026-05-08):** field basis only. Catches write-write
//!   races on automaton fields between concurrent `#effect`s and
//!   `#interrupt`s.
//! - **v0.2-α (this version):** **trait basis** added. The §4.5 pure-side
//!   predeclared traits (`Pure` / `Readable` / `Observable` / `Opaque`) and
//!   user-defined `@trait`s now contribute basis vectors per spec §7.1
//!   step 2. `@fn`s become concurrency nodes whose behaviour blade is the
//!   wedge of their declared traits — letting the engine reason about
//!   pure-vs-mutating concurrent calls at the trait level (the §7.1
//!   prose example: a `$ [Readable]` `@fn` running concurrent with a
//!   mutating `#interrupt` is provably orthogonal).
//!
//!   The Decision #22 imperative-side traits (`Hardware`, `Realtime`,
//!   `Acquire`, `Release`, `SeqCst`, `LockingDiscipline`, `PureState`,
//!   `Encapsulated`) are **deliberately excluded** from the basis per
//!   spec §2.6's note: "the orthogonality engine ignores `trait_list`
//!   entirely" with respect to those traits. They serve different
//!   purposes (codegen fences, audit trail, certification) than
//!   concurrency safety.
//!
//! ## v0.1 / v0.2-α scope (matches spec §7.0's restricted form)
//!
//! - **Restricted Cl(0,0,n) algebra:** every basis vector squares to zero.
//!   Bitmask check `a & b != 0 ⇒ wedge == 0` is the operational form.
//! - **Write-write races at field granularity.** Read-write races are
//!   v0.2-β graded-algebra work per §7.2.
//! - **Sound-conservative concurrency inference** per §7.3:
//!   - `#effect` × `#effect` → never concurrent (single foreground
//!     thread).
//!   - `#interrupt` × anything else → concurrent (interrupts preempt).
//!   - `#interrupt` × `#interrupt` (distinct) → concurrent (priority
//!     preemption).
//!   - `@fn` × `#interrupt` → concurrent (the `@fn` may be running on
//!     the foreground thread when the IRQ fires; spec §7.3's "@fn
//!     inherits the caller's concurrency class" rule applied
//!     conservatively).
//!   - `@fn` × `@fn` → never concurrent (single foreground thread).
//!   - `@fn` × `#effect` → never concurrent (single foreground thread).
//!   - `#transition` × anything → never directly concurrent (transitions
//!     are leaves of `#>` chains; their writes propagate via the
//!     transitive closure of the calling effect/interrupt's blade).
//! - **No `@sequential(A, B)` overrides yet.** Decision #11 is parsed
//!   but not consumed by the verifier.
//! - **No `#basis` override clauses yet.** Decision #4 rule 2 parsed
//!   but not consumed; auto-assignment is canonical.
//!
//! ## What the engine deliberately does not catch
//!
//! Per spec §7.0.1's pillars:
//!
//! - Mutations through `#unchecked_store` / `#volatile_store` /
//!   `#asm` — outside the proof boundary by design (audit-loggable).
//! - Read-write races at field granularity — v0.2-β graded algebra.
//! - Concurrency excluded by `@sequential(A, B)` — user-trusted.
//!
//! ## Implementation
//!
//! - **Blade representation:** `u64` bitmask (1 bit per basis vector).
//!   Up to 64 combined dimensions per compilation unit; wider units fail
//!   loudly with `BasisExhausted`. Field bits at indices `0..|F|`,
//!   trait bits at `|F|..|F|+|T|`.
//! - **Basis assignment:** automaton fields in declaration order
//!   (automaton-major, field-minor) → bits 0, 1, 2, …; then traits
//!   in canonical order (predeclared §4.5 first, then user-defined
//!   in declaration / first-appearance order) → bits |F|, |F|+1, ….
//! - **Behaviour multivector:** for each `#effect`/`#interrupt`, the
//!   union of basis bits for fields in its `MutationProfile.actual_writes`
//!   (already transitively closed by `clifford-effect`) AND any §7-basis
//!   traits in its declared `trait_list`. For `@fn`, just the trait
//!   bits — no field writes possible per the sigil-layer rule.
//! - **Wedge product:** `outer_product(a, b)` — `Some(a | b)` if
//!   disjoint, `None` if overlap. The algorithmic core.
//! - **Pairwise check:** O(N²) over the concurrency-checked nodes.

#![forbid(unsafe_code)]
#![warn(missing_docs)]

use std::collections::{HashMap, HashSet};

use clifford_ast::{Item, Program, TraitRef};
use clifford_effect::{CallableId, FieldRef, MutationProfile, MutationProfiles};
use thiserror::Error;

/// Errors produced by the orthogonality engine.
///
/// Reserves the `E05xx` range. The most important error is
/// `E0520: orthogonality violation` — its message names every conflicting
/// `(automaton, field)` and / or trait by source identifier per §7.5.
#[derive(Debug, Error)]
pub enum OrthoError {
    /// A pair of concurrent callables both touch the same basis vector
    /// — either by writing the same automaton field (the v0.1 case)
    /// or by sharing a §7-basis trait (the v0.2-α case).
    ///
    /// Each conflict carries the two callable display names plus every
    /// shared `(automaton, field)` reference and every shared trait
    /// name, decoded from the bitmask back to source identifiers per
    /// §7.5.
    #[error("E0520: orthogonality violation between `{a}` and `{b}`: {shared_display}")]
    OrthogonalityViolation {
        /// Display name of the first callable (e.g. `effect Foo`,
        /// `interrupt USART1_IRQ`, `fn pure_helper`).
        a: String,
        /// Display name of the second callable.
        b: String,
        /// The conflicting `(automaton, field)` pairs.
        shared_fields: Vec<FieldRef>,
        /// The conflicting trait names (predeclared §4.5 or user-defined).
        shared_traits: Vec<String>,
        /// Pre-rendered human-readable list (e.g.
        /// `` shared field(s) `Counter.value`; shared trait(s) `Readable` ``)
        /// for the message body.
        shared_display: String,
    },

    /// The compilation unit declares more basis vectors (fields + traits
    /// in the §7 basis) than the v0.1/v0.2-α `u64` blade can hold.
    /// 65th basis vector overflows; wider units need the bit-array
    /// representation reserved for v0.7+.
    #[error("E0530: GA basis exhausted: {basis_count} basis vectors (fields + traits) exceed the v0.1 64-bit blade limit (per spec §7.1)")]
    BasisExhausted {
        /// Total number of basis vectors at the point of overflow.
        basis_count: usize,
    },
}

// ─── §4.5 + Decision #22 trait classification ──────────────────────────────

/// Spec §4.5 pure-side predeclared traits in *table order*. Per spec
/// §7.1 step 2, these populate the trait-basis range first, in this
/// canonical order, before user-defined traits.
const PREDECLARED_PURE_TRAITS: &[&str] = &["Pure", "Readable", "Observable", "Opaque"];

/// Decision #22 imperative-side predeclared traits. Per spec §2.6's
/// note ("the orthogonality engine ignores `trait_list` entirely"
/// with respect to these), they are EXCLUDED from the §7 basis.
/// They serve different purposes:
/// - `Acquire` / `Release` / `SeqCst`: codegen memory-ordering fences.
/// - `Hardware` / `Realtime`: certification + audit.
/// - `LockingDiscipline`: v0.7+ Decision #21 lock machinery.
/// - `PureState` / `Encapsulated`: documentary; consumed by
///   `cliffordc audit --traits`.
const IMPERATIVE_TRAITS_NOT_IN_BASIS: &[&str] = &[
    "Hardware",
    "Realtime",
    "Acquire",
    "Release",
    "SeqCst",
    "LockingDiscipline",
    "PureState",
    "Encapsulated",
];

/// True iff the trait name participates in the §7 basis (and thus in
/// behaviour multivectors). Decision #22 imperative traits are
/// excluded; everything else (predeclared §4.5 and user-defined) is
/// included.
fn trait_in_basis(name: &str) -> bool {
    !IMPERATIVE_TRAITS_NOT_IN_BASIS.contains(&name)
}

// ─── §7.1 Basis Vector Assignment ──────────────────────────────────────────

/// Assignment of basis vectors to automaton fields (§7.1 step 1) and
/// traits (§7.1 step 2).
///
/// The total dimension `n = |F| + |T|` per spec §7.1 step 3; field
/// bits occupy indices `0..|F|`, trait bits `|F|..|F|+|T|`. Both
/// ranges share the same `u64` blade.
///
/// Trait basis order:
/// 1. §4.5 predeclared pure-side traits in table order
///    (`Pure`, `Readable`, `Observable`, `Opaque`) — even if not
///    referenced anywhere in the program, they get the lowest trait
///    indices so that diagnostics are stable across PRs that add or
///    remove `@fn`s.
/// 2. Other traits referenced anywhere in the program (in any
///    `$ [TraitList]`), in first-appearance order.
///
/// Decision #22 imperative traits (`Hardware`, `Acquire`, `Release`,
/// …) are NOT included per spec §2.6.
#[derive(Debug, Clone, Default)]
pub struct BasisAssignment {
    /// Forward map: `(automaton, field)` → bit index.
    by_field: HashMap<FieldRef, u32>,
    /// Reverse map for diagnostics: bit index → original `FieldRef`.
    by_index: Vec<FieldRef>,
    /// Forward map: trait name → bit index (always `>= field_count`).
    by_trait: HashMap<String, u32>,
    /// Reverse map for diagnostics: trait bit index (relative to
    /// `field_count`) → trait name.
    by_trait_index: Vec<String>,
    /// Number of field basis vectors (= `by_index.len()`). Cached so
    /// `decode_mask` can split the bitmask into the field range
    /// `0..field_count` and the trait range `field_count..`.
    field_count: u32,
}

impl BasisAssignment {
    /// Build the basis from a program's declarations. Fields come
    /// first (declaration order); traits second (predeclared §4.5
    /// table order, then first-appearance order for everything else).
    /// Returns `BasisExhausted` if total exceeds 64.
    pub fn build(program: &Program) -> Result<Self, OrthoError> {
        // Step 1: assign field bits.
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
                            basis_count: idx + 1,
                        });
                    }
                    by_field.insert(fref.clone(), idx as u32);
                    by_index.push(fref);
                }
            }
        }
        let field_count = by_index.len() as u32;

        // Step 2: assign trait bits. Predeclared §4.5 traits first
        // (always assigned, even if unreferenced — keeps indices
        // stable across edits), then any other trait that appears
        // in a `$ [TraitList]` and isn't a Decision #22 imperative
        // trait.
        let mut by_trait = HashMap::new();
        let mut by_trait_index: Vec<String> = Vec::new();
        let mut next_trait_bit = field_count;

        for &name in PREDECLARED_PURE_TRAITS {
            if next_trait_bit >= 64 {
                return Err(OrthoError::BasisExhausted {
                    basis_count: (next_trait_bit + 1) as usize,
                });
            }
            by_trait.insert(name.to_owned(), next_trait_bit);
            by_trait_index.push(name.to_owned());
            next_trait_bit += 1;
        }

        // Walk the program collecting non-predeclared trait names in
        // first-appearance order. Skip imperative-side and
        // already-assigned predeclared.
        let mut seen: HashSet<String> = PREDECLARED_PURE_TRAITS
            .iter()
            .map(|s| (*s).to_owned())
            .collect();
        for item in &program.items {
            for trait_ref in trait_lists_of(item) {
                if !trait_in_basis(&trait_ref.name) {
                    continue;
                }
                if seen.insert(trait_ref.name.clone()) {
                    if next_trait_bit >= 64 {
                        return Err(OrthoError::BasisExhausted {
                            basis_count: (next_trait_bit + 1) as usize,
                        });
                    }
                    by_trait.insert(trait_ref.name.clone(), next_trait_bit);
                    by_trait_index.push(trait_ref.name.clone());
                    next_trait_bit += 1;
                }
            }
        }

        Ok(Self {
            by_field,
            by_index,
            by_trait,
            by_trait_index,
            field_count,
        })
    }

    /// Bit index for one `(automaton, field)` reference, or `None` if
    /// not in the basis.
    #[must_use]
    pub fn bit_index(&self, field: &FieldRef) -> Option<u32> {
        self.by_field.get(field).copied()
    }

    /// Bit index for a trait name, or `None` if the trait is not in
    /// the basis (e.g. a Decision #22 imperative trait).
    #[must_use]
    pub fn trait_bit_index(&self, name: &str) -> Option<u32> {
        self.by_trait.get(name).copied()
    }

    /// Total dimension `n = |F| + |T|` per spec §7.1 step 3.
    #[must_use]
    pub fn dimension(&self) -> usize {
        self.by_index.len() + self.by_trait_index.len()
    }

    /// Number of field basis vectors (the `|F|` half of the dimension).
    #[must_use]
    pub fn field_count(&self) -> usize {
        self.by_index.len()
    }

    /// Number of trait basis vectors.
    #[must_use]
    pub fn trait_count(&self) -> usize {
        self.by_trait_index.len()
    }

    /// Decode a bitmask back to its source identifiers. Returns
    /// `(shared_fields, shared_traits)` — used by E0520 to render
    /// human-readable diagnostics per Emergent Rule 1 (never raw
    /// `e_n` indices).
    #[must_use]
    pub fn decode_mask(&self, mask: u64) -> (Vec<FieldRef>, Vec<String>) {
        let mut fields = Vec::new();
        let mut traits = Vec::new();
        for (i, fref) in self.by_index.iter().enumerate() {
            if i >= 64 {
                break;
            }
            if mask & (1u64 << i) != 0 {
                fields.push(fref.clone());
            }
        }
        for (i, name) in self.by_trait_index.iter().enumerate() {
            let bit = self.field_count as usize + i;
            if bit >= 64 {
                break;
            }
            if mask & (1u64 << bit) != 0 {
                traits.push(name.clone());
            }
        }
        (fields, traits)
    }
}

/// Helper: enumerate all `TraitRef`s declared by a top-level item.
/// Used by `BasisAssignment::build` to find traits that need basis
/// bits beyond the §4.5 predeclared set.
fn trait_lists_of(item: &Item) -> &[TraitRef] {
    match item {
        Item::Fn(decl) => &decl.trait_list,
        Item::Effect(decl) => &decl.trait_list,
        Item::Interrupt(decl) => &decl.trait_list,
        Item::Automaton(decl) => {
            // Automatons themselves don't carry a trait_list; their
            // transitions do.
            let _ = decl;
            &[]
        }
        // Other item kinds (Type, Trait, Interface, Impl, Test,
        // Sequential) either don't have a trait_list field at the
        // top-level position or aren't exercised by the v0.2-α
        // verifier.
        _ => &[],
    }
}

// ─── §7.2 Behaviour Multivector Construction ───────────────────────────────

/// Behaviour blade for one callable: the wedge of basis vectors for
/// every field it writes (transitively, per §6.2's `actual_writes`
/// closure) AND every §7-basis trait in its declared `trait_list`.
/// v0.2-α represents this as a single `u64` since under the
/// restricted Cl(0,0,n) algebra each callable contributes exactly one
/// blade — the union of its writes' bits and its in-basis traits' bits.
type Behaviour = u64;

/// Build the behaviour blade for an `#effect`/`#interrupt` from its
/// mutation profile + declared trait list. Field writes contribute
/// bits 0..|F|; in-basis traits contribute bits |F|..|F|+|T|.
fn behaviour_from_profile_and_traits(
    profile: Option<&MutationProfile>,
    trait_list: &[TraitRef],
    basis: &BasisAssignment,
) -> Behaviour {
    let mut blade: u64 = 0;
    if let Some(p) = profile {
        for fref in &p.actual_writes {
            if let Some(bit) = basis.bit_index(fref) {
                blade |= 1u64 << bit;
            }
        }
    }
    for tref in trait_list {
        if let Some(bit) = basis.trait_bit_index(&tref.name) {
            blade |= 1u64 << bit;
        }
    }
    blade
}

/// Build the behaviour blade for an `@fn`. Pure functions cannot
/// write automaton fields per the sigil-layer rule, so the blade is
/// just the wedge of in-basis traits the function declares.
///
/// **Default trait** (Emergent Rule 2): an `@fn` with no `$ [...]`
/// defaults to `$ [Pure]`. We mirror that here so a bare `@fn` carries
/// the `Pure` bit and is therefore orthogonal to anything that doesn't.
fn behaviour_from_fn_traits(trait_list: &[TraitRef], basis: &BasisAssignment) -> Behaviour {
    let mut blade: u64 = 0;
    if trait_list.is_empty() {
        // Emergent Rule 2: empty trait_list defaults to `[Pure]`.
        if let Some(bit) = basis.trait_bit_index("Pure") {
            blade |= 1u64 << bit;
        }
        return blade;
    }
    for tref in trait_list {
        if let Some(bit) = basis.trait_bit_index(&tref.name) {
            blade |= 1u64 << bit;
        }
    }
    blade
}

// ─── §7.3 Concurrency Inference (extended for @fn nodes) ───────────────────

/// Internal concurrency-node identifier. Wider than
/// `clifford_effect::CallableId` because the §7 verifier needs to
/// model `@fn` as a concurrency node (per spec §7.3) even though
/// `@fn`s don't appear in `MutationProfiles` (they have no field
/// writes).
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
enum ConcurrencyNode {
    /// Top-level `@fn`.
    Fn(String),
    /// Top-level `#effect`.
    Effect(String),
    /// Top-level `#interrupt`.
    Interrupt(String),
}

impl ConcurrencyNode {
    /// Display name for diagnostics.
    fn display(&self) -> String {
        match self {
            Self::Fn(n) => format!("fn {n}"),
            Self::Effect(n) => format!("effect {n}"),
            Self::Interrupt(n) => format!("interrupt {n}"),
        }
    }
}

/// Sound-conservative concurrency inference per §7.3.
///
/// v0.2-α matrix:
///
/// |          | `@fn`     | `#effect` | `#interrupt` |
/// |----------|-----------|-----------|--------------|
/// | `@fn`    | false     | false     | true         |
/// | `#eff`   | false     | false     | true         |
/// | `#irq`   | true      | true      | true         |
///
/// Rationale: `@fn`s and `#effect`s share the foreground thread, so
/// the runtime serialises them. `#interrupt`s preempt the
/// foreground; two distinct interrupts can preempt each other at
/// different priority levels.
fn can_concur(a: &ConcurrencyNode, b: &ConcurrencyNode) -> bool {
    use ConcurrencyNode::*;
    match (a, b) {
        // Two foreground-thread callables cannot run concurrently.
        (Fn(_), Fn(_))
        | (Effect(_), Effect(_))
        | (Fn(_), Effect(_))
        | (Effect(_), Fn(_)) => false,
        // Anything involving an interrupt is potentially concurrent.
        _ => true,
    }
}

// ─── §7.4 Wedge product ────────────────────────────────────────────────────

/// XOR-bitmask wedge-product on two blades.
///
/// Returns `Some(a | b)` when the wedge is non-zero (no shared basis
/// vector), `None` when the wedge is zero (some basis vector squared).
///
/// This is the algorithmic core of Clifford's concurrency safety
/// proof. Per Emergent Rule 6 it is the constructive existence test
/// for the product-category morphism `(f_A, f_B)`.
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
/// orthogonality condition per spec §7.4. Public entry point invoked
/// by the CLI between `clifford-effect`'s mutation-profile pass and
/// `clifford-codegen`'s lowering.
///
/// Returns `Ok(())` when the program is race-free under the §7.3
/// concurrency inference, or `Err(Vec<OrthoError>)` listing every
/// detected `E0520: orthogonality violation`. Multiple violations in
/// the same program are all reported, not just the first.
///
/// # Errors
///
/// - `BasisExhausted` if fields + traits exceed 64 basis vectors
///   (the `u64` blade width).
/// - `OrthogonalityViolation` for every pair of concurrent callables
///   whose behaviour blades share at least one basis vector.
pub fn verify(
    program: &Program,
    profiles: &MutationProfiles,
) -> Result<(), Vec<OrthoError>> {
    let basis = match BasisAssignment::build(program) {
        Ok(b) => b,
        Err(e) => return Err(vec![e]),
    };

    // Collect every concurrency-checked node with its behaviour
    // blade. We include `@fn`s as nodes (v0.2-α addition) so trait-
    // basis interactions can be checked.
    let mut nodes: Vec<(ConcurrencyNode, Behaviour)> = Vec::new();
    for item in &program.items {
        match item {
            Item::Fn(decl) => {
                let id = ConcurrencyNode::Fn(decl.name.clone());
                let blade = behaviour_from_fn_traits(&decl.trait_list, &basis);
                nodes.push((id, blade));
            }
            Item::Effect(decl) => {
                let cid = CallableId::Effect(decl.name.clone());
                let blade = behaviour_from_profile_and_traits(
                    profiles.lookup(&cid),
                    &decl.trait_list,
                    &basis,
                );
                nodes.push((ConcurrencyNode::Effect(decl.name.clone()), blade));
            }
            Item::Interrupt(decl) => {
                let cid = CallableId::Interrupt(decl.name.clone());
                let blade = behaviour_from_profile_and_traits(
                    profiles.lookup(&cid),
                    &decl.trait_list,
                    &basis,
                );
                nodes.push((ConcurrencyNode::Interrupt(decl.name.clone()), blade));
            }
            _ => {}
        }
    }

    // Pairwise check per §7.4. O(N²) pairs but N is the count of
    // top-level callables — a few dozen for realistic firmware.
    let mut errors: Vec<OrthoError> = Vec::new();
    for i in 0..nodes.len() {
        for j in (i + 1)..nodes.len() {
            let (id_a, blade_a) = &nodes[i];
            let (id_b, blade_b) = &nodes[j];
            if !can_concur(id_a, id_b) {
                continue;
            }
            if outer_product(*blade_a, *blade_b).is_none() {
                let conflict = blade_a & blade_b;
                let (shared_fields, shared_traits) = basis.decode_mask(conflict);
                let shared_display = render_shared(&shared_fields, &shared_traits);
                errors.push(OrthoError::OrthogonalityViolation {
                    a: id_a.display(),
                    b: id_b.display(),
                    shared_fields,
                    shared_traits,
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

/// Render the shared-basis decode for the E0520 message body.
/// Combines field references and trait names into one human-readable
/// string. v0.1 used field-only; v0.2-α can split the diagnostic by
/// kind so users see "shared field(s) … and shared trait(s) …".
fn render_shared(fields: &[FieldRef], traits: &[String]) -> String {
    let mut parts: Vec<String> = Vec::new();
    if !fields.is_empty() {
        let list: Vec<String> = fields
            .iter()
            .map(|f| format!("`{}.{}`", f.automaton, f.field))
            .collect();
        parts.push(format!("shared field(s) {}", list.join(", ")));
    }
    if !traits.is_empty() {
        let list: Vec<String> = traits.iter().map(|t| format!("`{t}`")).collect();
        parts.push(format!("shared trait(s) {}", list.join(", ")));
    }
    parts.join("; ")
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
    fn basis_is_just_predeclared_traits_for_program_with_no_automatons() {
        // Empty program — but the four §4.5 predeclared traits are
        // still assigned (so diagnostics that mention `Pure` etc. are
        // stable across edits). Field count is 0; trait count is 4.
        let program = parse_program("@fn t() { return; }");
        let basis = BasisAssignment::build(&program).expect("build");
        assert_eq!(basis.field_count(), 0);
        assert_eq!(basis.trait_count(), 4);
        assert_eq!(basis.dimension(), 4);
        assert_eq!(basis.trait_bit_index("Pure"), Some(0));
        assert_eq!(basis.trait_bit_index("Readable"), Some(1));
        assert_eq!(basis.trait_bit_index("Observable"), Some(2));
        assert_eq!(basis.trait_bit_index("Opaque"), Some(3));
    }

    #[test]
    fn basis_assigns_fields_first_then_traits() {
        // 3 fields + 4 predeclared traits = 7 basis vectors. Field
        // bits are 0..3; trait bits are 3..7.
        let src = "\
            #automaton A { x: u32; y: u32; }\n\
            #automaton B { z: u32; }\n\
        ";
        let program = parse_program(src);
        let basis = BasisAssignment::build(&program).expect("build");
        assert_eq!(basis.field_count(), 3);
        assert_eq!(basis.trait_count(), 4);
        assert_eq!(
            basis.bit_index(&FieldRef {
                automaton: "A".to_owned(),
                field: "x".to_owned()
            }),
            Some(0)
        );
        // First trait (Pure) gets the field-count'th bit.
        assert_eq!(basis.trait_bit_index("Pure"), Some(3));
        assert_eq!(basis.trait_bit_index("Readable"), Some(4));
    }

    #[test]
    fn basis_user_traits_get_bits_after_predeclared() {
        // A `@fn` with `$ [Magic]` introduces `Magic` as a user trait;
        // it gets a bit AFTER the four predeclared traits.
        let src = "@fn t() $ [Magic] { return; }";
        let program = parse_program(src);
        let basis = BasisAssignment::build(&program).expect("build");
        assert_eq!(basis.trait_bit_index("Pure"), Some(0));
        assert_eq!(basis.trait_bit_index("Magic"), Some(4));
    }

    #[test]
    fn basis_imperative_traits_excluded_per_section_2_6() {
        // Decision #22 imperative traits don't enter the basis.
        let src = "\
            #automaton C { v: u32; }\n\
            #effect e() #mutates: [C] $ [Hardware, Acquire, Release] { return; }\n\
        ";
        let program = parse_program(src);
        let basis = BasisAssignment::build(&program).expect("build");
        assert_eq!(basis.trait_bit_index("Hardware"), None);
        assert_eq!(basis.trait_bit_index("Acquire"), None);
        assert_eq!(basis.trait_bit_index("Release"), None);
        // Total basis is 1 field + 4 predeclared = 5.
        assert_eq!(basis.dimension(), 5);
    }

    #[test]
    fn basis_decode_mask_returns_named_fields_and_traits() {
        // 1 field (bit 0), 4 predeclared traits (bits 1..4). Set
        // bits 0 and 1 → field A.x and trait Pure.
        let src = "#automaton A { x: u32; }\n";
        let program = parse_program(src);
        let basis = BasisAssignment::build(&program).expect("build");
        let (fields, traits) = basis.decode_mask(0b11);
        assert_eq!(fields.len(), 1);
        assert_eq!(fields[0].field, "x");
        assert_eq!(traits, vec!["Pure".to_owned()]);
    }

    #[test]
    fn basis_exhausted_when_more_than_64_basis_vectors() {
        // 61 fields + 4 predeclared traits = 65 basis vectors.
        let mut src = String::from("#automaton Big {\n");
        for i in 0..61 {
            src.push_str(&format!("  f{i}: u32;\n"));
        }
        src.push_str("}\n");
        let program = parse_program(&src);
        let err = BasisAssignment::build(&program).expect_err("expected exhausted");
        assert!(matches!(err, OrthoError::BasisExhausted { basis_count } if basis_count == 65));
    }

    // ─── verify: orthogonal cases (v0.1 still passing) ───────────────

    #[test]
    fn orthogonal_program_passes() {
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

    // ─── verify: violation cases (v0.1) ──────────────────────────────

    #[test]
    fn interrupt_and_effect_writing_same_field_violates() {
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
            OrthoError::OrthogonalityViolation {
                shared_fields,
                shared_traits,
                ..
            } => {
                assert_eq!(shared_fields.len(), 1);
                assert_eq!(shared_fields[0].field, "v");
                assert!(shared_traits.is_empty());
            }
            other => panic!("expected OrthogonalityViolation, got {other:?}"),
        }
    }

    #[test]
    fn diagnostic_names_source_identifiers_not_basis_indices() {
        let src = "\
            #automaton Counter { value: u32; flag: u32; }\n\
            #effect main_loop() #mutates: [Counter] { Counter.value += 1u32; }\n\
            #interrupt SysTick() #mutates: [Counter] #priority: HIGH { Counter.value += 1u32; }\n\
        ";
        let program = parse_program(src);
        let profiles = build_profiles(&program);
        let errs = verify(&program, &profiles).expect_err("expected E0520");
        let msg = format!("{}", errs[0]);
        assert!(msg.contains("`Counter.value`"));
        // No raw basis indices, even though traits added bits to the
        // basis. The mask-decode for this case has no trait bits set
        // because no trait is actually shared between the two
        // callables.
        assert!(!msg.contains("e0") && !msg.contains("e1") && !msg.contains("e_"));
    }

    // ─── v0.2-α: trait-basis interactions ────────────────────────────

    #[test]
    fn pure_fn_concurrent_with_mutating_interrupt_is_orthogonal() {
        // The §7.1 prose example — but with `Pure` instead of
        // `Readable` since `@fn` defaults to `[Pure]`. The pure fn's
        // blade is just the Pure bit; the interrupt's is just the
        // field bit. Disjoint → orthogonal.
        let src = "\
            #automaton C { v: u32; }\n\
            @fn pure_helper() -> u32 { return 42u32; }\n\
            #interrupt SysTick() #mutates: [C] #priority: HIGH { C.v += 1u32; }\n\
        ";
        let program = parse_program(src);
        let profiles = build_profiles(&program);
        verify(&program, &profiles).expect("pure fn vs mutating IRQ is orthogonal");
    }

    #[test]
    fn fn_with_user_trait_concurrent_with_unrelated_interrupt_is_orthogonal() {
        // A user trait gets its own basis bit. As long as no other
        // concurrent callable shares it, no violation.
        let src = "\
            #automaton C { v: u32; }\n\
            @fn my_helper() $ [MyMarker] { return; }\n\
            #interrupt SysTick() #mutates: [C] #priority: HIGH { C.v += 1u32; }\n\
        ";
        let program = parse_program(src);
        let profiles = build_profiles(&program);
        verify(&program, &profiles).expect("user-trait fn vs interrupt is orthogonal");
    }

    #[test]
    fn fn_and_interrupt_sharing_a_user_trait_violates() {
        // Both the @fn and the interrupt declare `$ [Shared]`. The
        // wedge of (Shared bit) ∧ (Shared bit | field bit) shares the
        // Shared bit → violation. The diagnostic names the trait.
        //
        // Note: in practice this wouldn't happen because Decision #22
        // imperative traits (the common shared marker on #-side
        // callables) are excluded from the basis, but a user-defined
        // trait could land on both sides since `@trait` is layer-
        // universal in v0.2-β. Tests this corner case explicitly.
        let src = "\
            #automaton C { v: u32; }\n\
            @fn helper() $ [Shared] { return; }\n\
            #interrupt SysTick() #mutates: [C] #priority: HIGH $ [Shared] { C.v += 1u32; }\n\
        ";
        let program = parse_program(src);
        let profiles = build_profiles(&program);
        let errs = verify(&program, &profiles).expect_err("expected E0520");
        assert_eq!(errs.len(), 1);
        match &errs[0] {
            OrthoError::OrthogonalityViolation {
                shared_fields,
                shared_traits,
                shared_display,
                ..
            } => {
                assert!(shared_fields.is_empty(), "no field overlap; got {shared_fields:?}");
                assert_eq!(shared_traits, &vec!["Shared".to_owned()]);
                assert!(
                    shared_display.contains("`Shared`"),
                    "diagnostic should name `Shared`; got: {shared_display}"
                );
            }
            other => panic!("expected OrthogonalityViolation, got {other:?}"),
        }
    }

    #[test]
    fn two_pure_fns_are_skipped_by_concurrency_inference() {
        // Two `@fn`s without explicit traits both default to `[Pure]`.
        // Their behaviour blades both have the Pure bit — which would
        // be a violation IF they were concurrency-checked. But §7.3
        // says `@fn × @fn = false` (single foreground thread), so the
        // pair is never checked.
        let src = "\
            @fn a() { return; }\n\
            @fn b() { return; }\n\
        ";
        let program = parse_program(src);
        let profiles = build_profiles(&program);
        verify(&program, &profiles).expect("two pure fns are not concurrent");
    }

    #[test]
    fn imperative_traits_dont_create_violations_between_interrupts() {
        // Two interrupts both carrying `$ [Hardware, Release]` —
        // those are Decision #22 traits, excluded from the basis,
        // so they don't show up in the wedge. The interrupts touch
        // disjoint fields → orthogonal. Without this exclusion, the
        // shared `Hardware` and `Release` would falsely violate.
        let src = "\
            #automaton A { x: u32; }\n\
            #automaton B { y: u32; }\n\
            #interrupt IRQ_A() #mutates: [A] #priority: HIGH $ [Hardware, Release] { A.x += 1u32; }\n\
            #interrupt IRQ_B() #mutates: [B] #priority: HIGH $ [Hardware, Release] { B.y += 1u32; }\n\
        ";
        let program = parse_program(src);
        let profiles = build_profiles(&program);
        verify(&program, &profiles).expect("imperative traits are not in the basis");
    }

    #[test]
    fn multiple_violations_all_reported() {
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
        assert_eq!(errs.len(), 2);
    }

    #[test]
    fn transition_writes_propagate_into_caller_blade() {
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
        match &errs[0] {
            OrthoError::OrthogonalityViolation { shared_fields, .. } => {
                assert_eq!(shared_fields[0].field, "value");
            }
            _ => panic!("expected OrthogonalityViolation"),
        }
    }

    // ─── can_concur ──────────────────────────────────────────────────

    #[test]
    fn can_concur_effect_effect_returns_false() {
        let a = ConcurrencyNode::Effect("a".to_owned());
        let b = ConcurrencyNode::Effect("b".to_owned());
        assert!(!can_concur(&a, &b));
    }

    #[test]
    fn can_concur_interrupt_effect_returns_true() {
        let a = ConcurrencyNode::Interrupt("IRQ".to_owned());
        let b = ConcurrencyNode::Effect("e".to_owned());
        assert!(can_concur(&a, &b));
    }

    #[test]
    fn can_concur_interrupt_interrupt_returns_true() {
        let a = ConcurrencyNode::Interrupt("IRQ_A".to_owned());
        let b = ConcurrencyNode::Interrupt("IRQ_B".to_owned());
        assert!(can_concur(&a, &b));
    }

    #[test]
    fn can_concur_fn_fn_returns_false() {
        let a = ConcurrencyNode::Fn("a".to_owned());
        let b = ConcurrencyNode::Fn("b".to_owned());
        assert!(!can_concur(&a, &b));
    }

    #[test]
    fn can_concur_fn_effect_returns_false() {
        // Both run on the foreground thread.
        let a = ConcurrencyNode::Fn("helper".to_owned());
        let b = ConcurrencyNode::Effect("main_loop".to_owned());
        assert!(!can_concur(&a, &b));
        assert!(!can_concur(&b, &a));
    }

    #[test]
    fn can_concur_fn_interrupt_returns_true() {
        // The @fn could be running on the foreground thread when
        // the IRQ fires.
        let a = ConcurrencyNode::Fn("helper".to_owned());
        let b = ConcurrencyNode::Interrupt("SysTick".to_owned());
        assert!(can_concur(&a, &b));
        assert!(can_concur(&b, &a));
    }
}
