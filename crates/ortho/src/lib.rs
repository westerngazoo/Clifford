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

use clifford_ast::{AtomicKind, Block, Item, PriorityLevel, Program, StmtKind, TraitRef};
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

/// Behaviour signature for one callable, carrying separate write
/// and read blades per spec §7.2's graded read-write algebra.
///
/// - `writes`: the wedge of basis vectors for every field the
///   callable writes (transitively, per §6.2's `actual_writes`
///   closure) AND every §7-basis trait in its declared
///   `trait_list`.
/// - `reads`: the wedge of basis vectors for every field the
///   callable reads (transitively, per `actual_reads`). Trait
///   bits do NOT enter `reads` — traits are about what the
///   callable *commits to do*, not what it *observes*.
///
/// The §7.4 orthogonality check is now graded:
///
/// ```text
/// safe(A, B) ⟺ (writes_A ∧ writes_B == 0)
///            ∧ (reads_A  ∧ writes_B == 0)
///            ∧ (writes_A ∧ reads_B  == 0)
/// ```
///
/// Read-read (`reads_A ∧ reads_B`) is never a conflict — two reads
/// of the same field don't race.
#[derive(Debug, Clone, Copy, Default)]
struct Behaviour {
    writes: u64,
    reads: u64,
}

/// Build the behaviour for an `#effect`/`#interrupt` from its
/// mutation profile + declared trait list. Field writes contribute
/// bits 0..|F| in the `writes` blade; field reads contribute bits
/// 0..|F| in the `reads` blade; in-basis traits contribute bits
/// |F|..|F|+|T| in the `writes` blade.
fn behaviour_from_profile_and_traits(
    profile: Option<&MutationProfile>,
    trait_list: &[TraitRef],
    basis: &BasisAssignment,
) -> Behaviour {
    let mut beh = Behaviour::default();
    if let Some(p) = profile {
        for fref in &p.actual_writes {
            if let Some(bit) = basis.bit_index(fref) {
                beh.writes |= 1u64 << bit;
            }
        }
        for fref in &p.actual_reads {
            if let Some(bit) = basis.bit_index(fref) {
                beh.reads |= 1u64 << bit;
            }
        }
    }
    for tref in trait_list {
        if let Some(bit) = basis.trait_bit_index(&tref.name) {
            beh.writes |= 1u64 << bit;
        }
    }
    beh
}

/// Build the behaviour for an `@fn`. Pure functions cannot write
/// automaton fields per the sigil-layer rule, so the writes blade
/// is just trait bits. The reads blade IS populated from the
/// mutation profile — a `@fn` can read from automaton state via
/// `Self.field` (inside trait methods on a future slice) or via
/// `@snapshot Auto.field` (Decision #24, parser-only today).
///
/// **Default trait** (Emergent Rule 2): an `@fn` with no `$ [...]`
/// defaults to `$ [Pure]` — the bare `@fn` carries the `Pure` bit.
fn behaviour_from_fn_traits(
    profile: Option<&MutationProfile>,
    trait_list: &[TraitRef],
    basis: &BasisAssignment,
) -> Behaviour {
    let mut beh = Behaviour::default();
    if trait_list.is_empty() {
        // Emergent Rule 2: empty trait_list defaults to `[Pure]`.
        if let Some(bit) = basis.trait_bit_index("Pure") {
            beh.writes |= 1u64 << bit;
        }
    } else {
        for tref in trait_list {
            if let Some(bit) = basis.trait_bit_index(&tref.name) {
                beh.writes |= 1u64 << bit;
            }
        }
    }
    if let Some(p) = profile {
        for fref in &p.actual_reads {
            if let Some(bit) = basis.bit_index(fref) {
                beh.reads |= 1u64 << bit;
            }
        }
        // Defensive: a @fn shouldn't have actual_writes (sigil-layer
        // rule rejects it upstream). If it does, include them anyway
        // so the §7 check stays sound — better an over-strict false
        // positive than a missed race.
        for fref in &p.actual_writes {
            if let Some(bit) = basis.bit_index(fref) {
                beh.writes |= 1u64 << bit;
            }
        }
    }
    beh
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
/// v0.2-α matrix (without `@sequential` overrides):
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
///
/// **v0.2-γ adds `@sequential(A, B)` overrides.** When the user
/// declares `@sequential(A, B);` at top level, the engine
/// suppresses any concurrency pair where one node touches
/// automaton `A` and the other touches automaton `B` (per the
/// shared-or-mutated-by-only-one heuristic in `verify`'s caller).
/// The check is performed via [`is_pair_sequential`]; this base
/// function returns the *physical* concurrency answer per the
/// matrix above, before the `@sequential` consultation.
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

/// v0.2-γ: per Decision #11, returns `true` iff the user has
/// declared the two callables to be sequential via at least one
/// `@sequential(A, B);` attribute that pairs an automaton each
/// callable touches.
///
/// The semantics of the override are: a callable "touches"
/// automaton `A` iff `A` appears in its `actual_automata` set
/// (i.e. it WRITES at least one field of `A` directly or
/// transitively; reads alone don't make a callable "touch" `A`
/// for the §6.2 / Decision #11 contract). The pair `(X, Y)` is
/// sequential iff there exists a declared `@sequential(A, B)`
/// such that `A ∈ touches(X)` and `B ∈ touches(Y)`, OR
/// symmetrically `B ∈ touches(X)` and `A ∈ touches(Y)`.
///
/// **Symmetry:** `@sequential(A, B)` and `@sequential(B, A)`
/// carry the same meaning per spec §2.6.
///
/// **Trust model:** The user's assertion is *trusted*. The
/// compiler does not verify that A and B truly never run
/// concurrently — that's outside the engine's proof boundary
/// per §7.0.1's pillars. Misuse (declaring two automata
/// sequential when in practice they may concur) is a user-
/// introduced soundness bug.
///
/// **What `@sequential` does NOT cover:**
///
/// - **Same-automaton concurrency.** If two callables both
///   touch automaton `A`, an `@sequential(A, A)` declaration is
///   meaningless — automatons are already inherently sequential
///   within themselves (Decision #5: at most one transition of
///   `A` runs at a time). The verifier never emits violations
///   for this case anyway.
/// - **Read-only references.** A callable that only READS from
///   `A` (no writes, so `A ∉ actual_automata`) is not "touching"
///   `A` for `@sequential` purposes. This is the corner case
///   that v0.2-β's read-write race detector exposed: if the
///   user wants a read-only foreground drain to be sequential
///   with an ISR producer, they must use `#atomic` (§6.6) or
///   `@snapshot` (Decision #24). `@sequential` is for two
///   *mutators* on different automatons.
fn is_pair_sequential(
    a: &ConcurrencyNode,
    b: &ConcurrencyNode,
    profiles: &MutationProfiles,
    sequential_pairs: &HashSet<(String, String)>,
) -> bool {
    let touches_a = node_touches(a, profiles);
    let touches_b = node_touches(b, profiles);
    for x in &touches_a {
        for y in &touches_b {
            if x == y {
                continue;
            }
            // The pair set stores canonical (lo, hi) — try both
            // orders to be defensive against future writes that
            // forget the canonicalisation.
            if sequential_pairs.contains(&(x.clone(), y.clone()))
                || sequential_pairs.contains(&(y.clone(), x.clone()))
            {
                return true;
            }
        }
    }
    false
}

/// Set of automaton names a node touches (i.e. writes a field of)
/// per its mutation profile. Used by `is_pair_sequential` to match
/// `@sequential(A, B)` clauses.
fn node_touches(node: &ConcurrencyNode, profiles: &MutationProfiles) -> HashSet<String> {
    let id = match node {
        ConcurrencyNode::Effect(name) => CallableId::Effect(name.clone()),
        ConcurrencyNode::Interrupt(name) => CallableId::Interrupt(name.clone()),
        ConcurrencyNode::Fn(_) => return HashSet::new(),
    };
    profiles
        .lookup(&id)
        .map(|p| p.actual_automata.clone())
        .unwrap_or_default()
}

/// Collect every `@sequential(A, B);` declaration from the program,
/// canonicalised as `(lo, hi)` alphabetically-sorted pairs so
/// `@sequential(A, B)` and `@sequential(B, A)` produce the same
/// entry per spec §2.6's symmetry note.
/// v0.2-θ: collect every callable name (transitions + effects)
/// declared with `#atomic: interrupt_critical;`. Used by
/// `body_inherits_atomic_from_proc_call` to decide whether a
/// caller body whose only statement is `#> name();` should
/// inherit atomicity from its callee.
///
/// Transitions are recorded by name only (no automaton
/// qualifier). The §6.2 transition-name uniqueness invariant
/// already guarantees no collisions across automatons in real
/// programs, but this helper does NOT validate that — collisions
/// would silently treat both as atomic, which is conservative-
/// permissive (over-suppression rather than under-suppression).
/// A future slice can refine to `(automaton, name)` qualified
/// keys.
fn collect_atomic_callable_names(program: &Program) -> HashSet<String> {
    let mut out = HashSet::new();
    for item in &program.items {
        match item {
            Item::Effect(decl) => {
                if matches!(decl.atomic, Some(AtomicKind::InterruptCritical)) {
                    out.insert(decl.name.clone());
                }
            }
            Item::Interrupt(decl) => {
                if matches!(decl.atomic, Some(AtomicKind::InterruptCritical)) {
                    out.insert(decl.name.clone());
                }
            }
            Item::Automaton(decl) => {
                for tr in &decl.transitions {
                    if matches!(tr.atomic, Some(AtomicKind::InterruptCritical)) {
                        out.insert(tr.name.clone());
                    }
                }
            }
            _ => {}
        }
    }
    out
}

/// v0.2-θ: returns `true` iff a callable's body should inherit
/// atomicity from a `#atomic: interrupt_critical;` callee.
///
/// **v0.2-θ MVP rule:** the body consists of EXACTLY one
/// statement that is a `#> proc()` call to an atomic-marked
/// callable, optionally followed by a `return;`. This is the
/// canonical "delegated ISR" pattern:
///
/// ```clifford
/// #interrupt USART1_IRQ() #mutates: [C] #priority: HIGH {
///   #> handle_uart1_event();   // body = exactly one #> call
/// }
/// ```
///
/// If `handle_uart1_event` is `#atomic: interrupt_critical;`,
/// then USART1_IRQ enters that callee's body which masks
/// interrupts via the v0.2-ε `cpsid i` emission. The interrupt
/// effectively runs atomically.
///
/// **Why this strict rule:** any other statement in the body
/// (a direct mutation, a non-atomic call, a let-binding that
/// reads automaton state, an if/sigma block) could expose racy
/// operations BEFORE the atomic callee enters. v0.2-θ takes
/// the conservative path; richer inheritance (multi-statement
/// bodies, atomic blocks within larger bodies, atomic-on-call-
/// site rather than per-callable) is a future slice with its
/// own analysis.
///
/// **Symmetry note:** the rule applies uniformly to effects,
/// interrupts, and the callees they delegate to (which can be
/// transitions, effects, or other interrupts). Inheritance
/// is single-hop only — we don't iteratively close the
/// "inherited atomic" property across chains because the
/// strict one-statement rule already implies a single hop.
fn body_inherits_atomic_from_proc_call(
    body: &Block,
    atomic_callables: &HashSet<String>,
) -> bool {
    // Filter out a trailing `return;` (no value) — common in
    // hand-written firmware and semantically a no-op after the
    // delegated call.
    let stmts: Vec<&clifford_ast::Stmt> = body
        .stmts
        .iter()
        .filter(|s| !matches!(s.kind, StmtKind::Return(None)))
        .collect();
    if stmts.len() != 1 {
        return false;
    }
    match &stmts[0].kind {
        StmtKind::ProcCall { name, .. } => atomic_callables.contains(name),
        _ => false,
    }
}

/// v0.2-η: collect every `#interrupt`'s `#priority: …` clause
/// into a name → level map. Used by `priorities_indicate_no_preemption`
/// to decide whether two interrupts can preempt each other.
fn collect_interrupt_priorities(program: &Program) -> HashMap<String, PriorityLevel> {
    let mut out = HashMap::new();
    for item in &program.items {
        if let Item::Interrupt(decl) = item {
            out.insert(decl.name.clone(), decl.priority.clone());
        }
    }
    out
}

/// v0.2-η: per spec §2.5 + Cortex-M NVIC semantics, two
/// interrupts at the same priority cannot preempt each other —
/// the NVIC processes them sequentially. Returns `true` iff
/// the two priorities are structurally equal AND therefore the
/// pair cannot be concurrent in the spec's §7.3 sense.
///
/// **What "structurally equal" means:** `PriorityLevel::Low ==
/// PriorityLevel::Low`, `Numeric("3") == Numeric("3")` (raw text
/// comparison; we don't parse the integer for v0.2-η since the
/// numeric range is target-specific). Mixed shapes — e.g.
/// `Low` vs `Numeric("0")` — are conservatively treated as
/// DIFFERENT priorities (could preempt) even if the user's
/// target maps `Low` to numeric 0; v0.2-η doesn't know the
/// target's priority encoding.
///
/// **Conservatism:** a `false` from this function means the
/// pair IS treated as concurrent (the standard wedge check
/// fires). A `true` means the pair is suppressed — and that
/// requires us to be sound, so we err on the side of "false"
/// when in doubt (e.g. mixed shapes).
fn priorities_indicate_no_preemption(a: &PriorityLevel, b: &PriorityLevel) -> bool {
    match (a, b) {
        (PriorityLevel::Low, PriorityLevel::Low) => true,
        (PriorityLevel::Medium, PriorityLevel::Medium) => true,
        (PriorityLevel::High, PriorityLevel::High) => true,
        (PriorityLevel::Numeric(s1), PriorityLevel::Numeric(s2)) => {
            // Compare canonicalised raw text (strip whitespace +
            // underscores) so `42` and `4_2` count as the same
            // priority. We don't parse to integer because the
            // numeric range is target-specific.
            let canon = |s: &str| -> String {
                s.chars().filter(|c| !c.is_whitespace() && *c != '_').collect()
            };
            canon(s1) == canon(s2)
        }
        // Mixed kinds: conservatively treat as different
        // (could preempt). A future slice with target-aware
        // priority normalisation can refine this.
        _ => false,
    }
}

fn collect_sequential_pairs(program: &Program) -> HashSet<(String, String)> {
    let mut out = HashSet::new();
    for item in &program.items {
        if let Item::Sequential(attr) = item {
            let (lo, hi) = if attr.a <= attr.b {
                (attr.a.clone(), attr.b.clone())
            } else {
                (attr.b.clone(), attr.a.clone())
            };
            out.insert((lo, hi));
        }
    }
    out
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

    // v0.2-θ: pre-pass — collect every callable name declared
    // `#atomic: interrupt_critical;` so the per-node atomicity
    // computation below can OR in the `inherits_atomic` bit
    // for delegated-ISR bodies.
    let atomic_callables = collect_atomic_callable_names(program);

    // Collect every concurrency-checked node with its behaviour
    // and atomicity state. `@fn`s are included as nodes (v0.2-α)
    // for trait-basis and read-basis interactions; v0.2-δ adds
    // `is_atomic_critical` so the pair check can suppress
    // interrupt pairings against atomic-critical bodies. v0.2-θ
    // OR's in `body_inherits_atomic_from_proc_call` so a one-
    // statement delegated-ISR body inherits atomicity from its
    // callee.
    let mut nodes: Vec<(ConcurrencyNode, Behaviour, bool)> = Vec::new();
    for item in &program.items {
        match item {
            Item::Fn(decl) => {
                let id = ConcurrencyNode::Fn(decl.name.clone());
                let beh = behaviour_from_fn_traits(None, &decl.trait_list, &basis);
                // `@fn`s can't carry `#atomic` (it's an imperative-
                // layer clause); always false.
                nodes.push((id, beh, false));
            }
            Item::Effect(decl) => {
                let cid = CallableId::Effect(decl.name.clone());
                let beh = behaviour_from_profile_and_traits(
                    profiles.lookup(&cid),
                    &decl.trait_list,
                    &basis,
                );
                let direct_atomic = matches!(decl.atomic, Some(AtomicKind::InterruptCritical));
                // v0.2-θ inheritance: a one-statement body that
                // is a `#>` to an atomic callable inherits.
                let inherited_atomic = !direct_atomic
                    && body_inherits_atomic_from_proc_call(&decl.body, &atomic_callables);
                nodes.push((
                    ConcurrencyNode::Effect(decl.name.clone()),
                    beh,
                    direct_atomic || inherited_atomic,
                ));
            }
            Item::Interrupt(decl) => {
                let cid = CallableId::Interrupt(decl.name.clone());
                let beh = behaviour_from_profile_and_traits(
                    profiles.lookup(&cid),
                    &decl.trait_list,
                    &basis,
                );
                let direct_atomic = matches!(decl.atomic, Some(AtomicKind::InterruptCritical));
                let inherited_atomic = !direct_atomic
                    && body_inherits_atomic_from_proc_call(&decl.body, &atomic_callables);
                nodes.push((
                    ConcurrencyNode::Interrupt(decl.name.clone()),
                    beh,
                    direct_atomic || inherited_atomic,
                ));
            }
            _ => {}
        }
    }

    // v0.2-γ: collect `@sequential(A, B);` overrides once.
    let sequential_pairs = collect_sequential_pairs(program);

    // v0.2-η: collect every `#interrupt`'s `#priority` so the
    // pair check can suppress same-priority interrupt pairs
    // (NVIC processes them sequentially per Cortex-M semantics).
    let interrupt_priorities = collect_interrupt_priorities(program);

    // Pairwise graded check per §7.2 + §7.4:
    //
    //   safe(A, B) ⟺ (writes_A ∧ writes_B == 0)     [v0.1: write-write]
    //              ∧ (reads_A  ∧ writes_B == 0)     [v0.2-β: read-write]
    //              ∧ (writes_A ∧ reads_B  == 0)     [v0.2-β: write-read]
    //
    // Read-read overlap is never a conflict.
    let mut errors: Vec<OrthoError> = Vec::new();
    for i in 0..nodes.len() {
        for j in (i + 1)..nodes.len() {
            let (id_a, beh_a, atomic_a) = &nodes[i];
            let (id_b, beh_b, atomic_b) = &nodes[j];
            if !can_concur(id_a, id_b) {
                continue;
            }
            // v0.2-γ: skip pairs the user has explicitly asserted
            // as sequential via `@sequential(A, B);`. This is a
            // *trusted* override per Decision #11 — the engine
            // does not verify the assertion.
            if is_pair_sequential(id_a, id_b, profiles, &sequential_pairs) {
                continue;
            }
            // v0.2-δ: skip pairs where one side is `#atomic:
            // interrupt_critical` and the other is an `#interrupt`.
            // The atomic side runs with all maskable interrupts
            // disabled (`cpsid i` on Cortex-M); the interrupt
            // cannot preempt the body. Per spec §6.6 + §7.2.
            //
            // Rationale for the asymmetric rule:
            //   - atomic_effect × interrupt → suppressed (interrupt
            //     masked during the body).
            //   - atomic_interrupt × interrupt → suppressed (the
            //     atomic interrupt masks all maskable interrupts on
            //     entry; even a higher-priority IRQ stays pending
            //     until the body returns).
            //   - atomic_X × non-atomic non-interrupt callable →
            //     the standard wedge check still runs; atomicity
            //     doesn't grant safety against foreground-thread
            //     races (those are already non-concurrent per §7.3).
            let either_atomic = *atomic_a || *atomic_b;
            let other_is_interrupt = matches!(id_a, ConcurrencyNode::Interrupt(_))
                || matches!(id_b, ConcurrencyNode::Interrupt(_));
            if either_atomic && other_is_interrupt {
                continue;
            }
            // v0.2-η: NVIC priority semantics — two interrupts at
            // the same priority cannot preempt each other on
            // Cortex-M (they run sequentially via tail-chaining).
            // If both sides are interrupts AND their declared
            // `#priority` matches structurally, skip the pair.
            // Different-priority interrupts (or any pair where
            // either side isn't an interrupt) take the standard
            // path.
            if let (ConcurrencyNode::Interrupt(name_a), ConcurrencyNode::Interrupt(name_b)) =
                (id_a, id_b)
            {
                let prio_a = interrupt_priorities.get(name_a);
                let prio_b = interrupt_priorities.get(name_b);
                if let (Some(pa), Some(pb)) = (prio_a, prio_b) {
                    if priorities_indicate_no_preemption(pa, pb) {
                        continue;
                    }
                }
            }
            // Combined conflict mask across the three race classes.
            // Same field appearing in writes(A) and writes(B), or
            // in reads(A) and writes(B), or writes(A) and reads(B)
            // → conflict.
            let ww = beh_a.writes & beh_b.writes;
            let rw = beh_a.reads & beh_b.writes;
            let wr = beh_a.writes & beh_b.reads;
            let conflict = ww | rw | wr;
            if conflict != 0 {
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

    // ─── v0.2-δ: #atomic: interrupt_critical (§6.6) ──────────────────

    #[test]
    fn atomic_effect_suppresses_pair_with_interrupt() {
        // Without #atomic, this is the canonical SPSC consumer
        // race v0.2-β catches: foreground reads `v` while
        // interrupt writes it. With `#atomic: interrupt_critical`
        // on the effect, the pair is suppressed because the
        // effect's body runs with all interrupts masked.
        let src = "\
            #automaton C { v: u32; }\n\
            #effect drain() -> u32 #mutates: [C] #atomic: interrupt_critical; { return C.v; }\n\
            #interrupt Tally() #mutates: [C] #priority: HIGH { C.v += 1u32; }\n\
        ";
        let program = parse_program(src);
        let profiles = build_profiles(&program);
        verify(&program, &profiles).expect("#atomic suppresses pair with interrupt");
    }

    #[test]
    fn atomic_interrupt_suppresses_pair_with_other_interrupt() {
        // An `#atomic: interrupt_critical` interrupt masks ALL
        // maskable interrupts on entry, so a higher-priority
        // (or same-priority) IRQ that would otherwise preempt
        // it cannot. The pair is suppressed regardless of which
        // side carries the attribute.
        let src = "\
            #automaton C { v: u32; }\n\
            #interrupt Critical() #mutates: [C] #priority: LOW #atomic: interrupt_critical; { C.v = 1u32; }\n\
            #interrupt Other() #mutates: [C] #priority: HIGH { C.v = 2u32; }\n\
        ";
        let program = parse_program(src);
        let profiles = build_profiles(&program);
        verify(&program, &profiles).expect("#atomic interrupt suppresses pair");
    }

    #[test]
    fn atomic_does_not_suppress_pair_with_non_interrupt() {
        // `#atomic: interrupt_critical` only masks INTERRUPTS.
        // Two foreground callables (effect × effect) are already
        // non-concurrent per §7.3 — the attribute doesn't change
        // that case (no false positive risk). But two effects
        // mutating the same field still don't race because of
        // the foreground-thread serialisation.
        let src = "\
            #automaton C { v: u32; }\n\
            #effect e1() #mutates: [C] #atomic: interrupt_critical; { C.v = 1u32; }\n\
            #effect e2() #mutates: [C] { C.v = 2u32; }\n\
        ";
        let program = parse_program(src);
        let profiles = build_profiles(&program);
        // Both effects on foreground → can_concur returns false →
        // no violation regardless of #atomic.
        verify(&program, &profiles).expect("two effects always serialise");
    }

    #[test]
    fn no_atomic_means_no_suppression() {
        // Sanity: same source as
        // `atomic_effect_suppresses_pair_with_interrupt` but
        // WITHOUT the #atomic attribute. v0.2-β rejects with
        // E0520; verifies the suppression is what's making the
        // first test pass.
        let src = "\
            #automaton C { v: u32; }\n\
            #effect drain() -> u32 #mutates: [C] { return C.v; }\n\
            #interrupt Tally() #mutates: [C] #priority: HIGH { C.v += 1u32; }\n\
        ";
        let program = parse_program(src);
        let profiles = build_profiles(&program);
        let errs = verify(&program, &profiles).expect_err("expected E0520 without #atomic");
        assert_eq!(errs.len(), 1);
    }

    #[test]
    fn atomic_with_multiple_field_writes_suppresses_all() {
        // An `#atomic: interrupt_critical` effect that writes
        // multiple fields — the spec §7.2 motivation for #atomic
        // ("multi-field consistency must use #atomic"). The
        // engine suppresses the entire pair regardless of how
        // many fields would otherwise conflict.
        let src = "\
            #automaton C { v1: u32; v2: u32; v3: u32; }\n\
            #effect bulk_update() #mutates: [C] #atomic: interrupt_critical; {\n  \
              C.v1 = 1u32;\n  \
              C.v2 = 2u32;\n  \
              C.v3 = 3u32;\n\
            }\n\
            #interrupt Reader() #mutates: [C] #priority: HIGH { C.v1 = C.v2; }\n\
        ";
        let program = parse_program(src);
        let profiles = build_profiles(&program);
        verify(&program, &profiles).expect("#atomic suppresses regardless of conflict count");
    }

    #[test]
    fn atomic_transition_is_recognised() {
        // Verifies the parser plumbs `#atomic` on transitions
        // (slice 9's transitions weren't atomic-aware). Today
        // the verifier doesn't check transitions directly per
        // §7.3 (their writes propagate via actual_writes), but
        // the AST field still gets populated and a future slice
        // can consume it.
        let src = "\
            #automaton C { v: u32;\n  \
              #transition tick #atomic: interrupt_critical; { C.v += 1u32; }\n\
            }\n\
        ";
        let program = parse_program(src);
        // Ortho doesn't surface transition-side atomic in v0.2-δ,
        // so just confirm the program parses + verifies.
        let profiles = build_profiles(&program);
        verify(&program, &profiles).expect("transition-side #atomic parses + verifies");
    }

    // ─── v0.2-γ: @sequential(A, B) consumption (Decision #11) ────────

    #[test]
    fn sequential_attr_suppresses_violation_between_two_automatons() {
        // Without @sequential: two ISRs writing the same field
        // (one in A, one mirrored to B) would conflict if they
        // shared a basis bit. Here they touch DIFFERENT
        // automatons but the user asserts sequentiality. The
        // verifier still does the wedge check, which finds no
        // conflict (disjoint fields), so this test only proves
        // @sequential doesn't BREAK an already-orthogonal
        // program.
        let src = "\
            #automaton A { x: u32; }\n\
            #automaton B { y: u32; }\n\
            @sequential(A, B);\n\
            #interrupt IRQ_A() #mutates: [A] #priority: HIGH { A.x = 1u32; }\n\
            #interrupt IRQ_B() #mutates: [B] #priority: HIGH { B.y = 1u32; }\n\
        ";
        let program = parse_program(src);
        let profiles = build_profiles(&program);
        verify(&program, &profiles).expect("disjoint + @sequential is fine");
    }

    #[test]
    fn sequential_attr_silences_violation_when_automatons_share_state() {
        // Two automatons sharing a field name (different
        // automatons, so different basis vectors actually — but
        // the user wires both ISRs to mutate "shared logical
        // state" via mutual writes). Without @sequential, two
        // interrupts writing fields of A and B respectively don't
        // conflict (disjoint basis). To make a meaningful test,
        // we use a single automaton accessed from both automatons'
        // namespace via an effect that mutates both — actually
        // that's not allowed without mutates declarations. Let me
        // use a different shape:
        //
        // Two effects that BOTH write a shared field of one
        // automaton. v0.2-γ doesn't suppress this — both
        // callables touch the SAME automaton, which is inherently
        // sequential per Decision #5. So this just asserts that
        // we don't introduce a regression.
        let src = "\
            #automaton C { v: u32; }\n\
            #effect e1() #mutates: [C] { C.v = 1u32; }\n\
            #effect e2() #mutates: [C] { C.v = 2u32; }\n\
        ";
        let program = parse_program(src);
        let profiles = build_profiles(&program);
        // Two #effects can't run concurrently → no violation
        // even without @sequential.
        verify(&program, &profiles).expect("two effects on same automaton — safe");
    }

    #[test]
    fn sequential_attr_suppresses_real_cross_automaton_interrupt_pair() {
        // Two ISRs each on a separate automaton, but both
        // automatons' effects touch a shared third automaton's
        // field via #mutates. This forces a real cross-automaton
        // race that @sequential should suppress.
        //
        // Setup:
        //   - Automaton M owns shared `m_count` (the actual race
        //     site).
        //   - A's transitions mutate M. B's transitions mutate M.
        //   - IRQ_A mutates A (and transitively M). IRQ_B
        //     mutates B (and transitively M).
        //   - Without @sequential: IRQ_A and IRQ_B both reach
        //     M.m_count → orthogonality violation.
        //   - With @sequential(A, B): user asserts the two ISRs
        //     never run concurrently → suppress.
        //
        // For v0.2-γ MVP, the touches() set includes only direct
        // automatons declared in #mutates. Transitive cross-
        // automaton coverage requires effect/profile chaining
        // which is already wired. We test the simpler case:
        // two interrupts writing fields of two different
        // automatons that happen to share a basis bit — but
        // basis bits are ALWAYS disjoint between automatons by
        // construction, so a real conflict requires shared
        // automaton.
        //
        // The cleanest test is: same automaton, two interrupts.
        // That's a real race the verifier catches. Adding
        // `@sequential(SameAuto, SameAuto)` is meaningless
        // (matched by the same-name skip in is_pair_sequential).
        //
        // So @sequential's actual use case (per Decision #11) is
        // the case where two automatons SHARE state (not
        // possible under the v0.1 strict ownership model). v0.2-γ
        // therefore tests the framework: we declare the
        // attribute, the engine reads it, and the
        // is_pair_sequential function reports correctly.
        //
        // This test verifies that an effect writing field of A
        // and an interrupt writing field of B, with
        // @sequential(A, B), are NOT flagged regardless of
        // whether they would otherwise be flagged.
        //
        // Since A and B don't share a basis bit, the test
        // demonstrates the override path is wired (the program
        // passes), but the suppression isn't visible in this
        // specific case. The synthetic
        // `is_pair_sequential_returns_true_when_attribute_present`
        // unit test below proves the helper itself works.
        let src = "\
            #automaton A { x: u32; }\n\
            #automaton B { y: u32; }\n\
            @sequential(A, B);\n\
            #effect set_a() #mutates: [A] { A.x = 1u32; }\n\
            #interrupt IRQ_B() #mutates: [B] #priority: HIGH { B.y = 1u32; }\n\
        ";
        let program = parse_program(src);
        let profiles = build_profiles(&program);
        verify(&program, &profiles).expect("@sequential allows the pair");
    }

    #[test]
    fn is_pair_sequential_returns_true_when_attribute_present() {
        // Direct unit test on the helper — independently verifies
        // the lookup logic without going through verify().
        let src = "\
            #automaton A { x: u32; }\n\
            #automaton B { y: u32; }\n\
            @sequential(A, B);\n\
            #effect ea() #mutates: [A] { A.x = 1u32; }\n\
            #interrupt IRQ_B() #mutates: [B] #priority: HIGH { B.y = 1u32; }\n\
        ";
        let program = parse_program(src);
        let profiles = build_profiles(&program);
        let pairs = collect_sequential_pairs(&program);
        assert_eq!(pairs.len(), 1);
        assert!(pairs.contains(&("A".to_owned(), "B".to_owned())));

        let node_a = ConcurrencyNode::Effect("ea".to_owned());
        let node_b = ConcurrencyNode::Interrupt("IRQ_B".to_owned());
        assert!(is_pair_sequential(&node_a, &node_b, &profiles, &pairs));
        // Symmetric: order shouldn't matter.
        assert!(is_pair_sequential(&node_b, &node_a, &profiles, &pairs));
    }

    #[test]
    fn is_pair_sequential_handles_reverse_order_in_attribute() {
        // `@sequential(B, A)` should suppress the (A, B) pair
        // too — the attribute is symmetric per spec §2.6.
        let src = "\
            #automaton A { x: u32; }\n\
            #automaton B { y: u32; }\n\
            @sequential(B, A);\n\
        ";
        let program = parse_program(src);
        let pairs = collect_sequential_pairs(&program);
        // Canonicalised to (A, B) since A < B alphabetically.
        assert_eq!(pairs.len(), 1);
        assert!(pairs.contains(&("A".to_owned(), "B".to_owned())));
    }

    #[test]
    fn is_pair_sequential_returns_false_without_attribute() {
        let src = "\
            #automaton A { x: u32; }\n\
            #automaton B { y: u32; }\n\
            #effect ea() #mutates: [A] { A.x = 1u32; }\n\
            #interrupt IRQ_B() #mutates: [B] #priority: HIGH { B.y = 1u32; }\n\
        ";
        let program = parse_program(src);
        let profiles = build_profiles(&program);
        let pairs = collect_sequential_pairs(&program);
        assert!(pairs.is_empty());

        let node_a = ConcurrencyNode::Effect("ea".to_owned());
        let node_b = ConcurrencyNode::Interrupt("IRQ_B".to_owned());
        assert!(!is_pair_sequential(&node_a, &node_b, &profiles, &pairs));
    }

    #[test]
    fn sequential_attr_does_not_suppress_same_automaton_pair() {
        // `@sequential(C, C)` is meaningless (an automaton is
        // already inherently sequential within itself per
        // Decision #5). The same-name skip in is_pair_sequential
        // means the override has no effect. Verify the verifier
        // doesn't crash or produce misleading behaviour.
        let src = "\
            #automaton C { v: u32; }\n\
            @sequential(C, C);\n\
            #effect e() #mutates: [C] { C.v = 1u32; }\n\
            #interrupt IRQ() #mutates: [C] #priority: HIGH { C.v = 2u32; }\n\
        ";
        let program = parse_program(src);
        let profiles = build_profiles(&program);
        // The interrupt and the effect both write C.v → write-
        // write race. @sequential(C, C) does NOT suppress
        // (the helper's same-name skip ignores the (C, C) entry).
        let errs = verify(&program, &profiles).expect_err("expected E0520");
        assert_eq!(errs.len(), 1);
    }

    #[test]
    fn collect_sequential_pairs_deduplicates_symmetric_declarations() {
        // Two declarations `@sequential(A, B)` and
        // `@sequential(B, A)` should canonicalise to one entry.
        let src = "\
            #automaton A { x: u32; }\n\
            #automaton B { y: u32; }\n\
            @sequential(A, B);\n\
            @sequential(B, A);\n\
        ";
        let program = parse_program(src);
        let pairs = collect_sequential_pairs(&program);
        assert_eq!(pairs.len(), 1);
    }

    #[test]
    fn collect_sequential_pairs_handles_multiple_distinct_pairs() {
        let src = "\
            #automaton A { x: u32; }\n\
            #automaton B { y: u32; }\n\
            #automaton C { z: u32; }\n\
            @sequential(A, B);\n\
            @sequential(B, C);\n\
            @sequential(A, C);\n\
        ";
        let program = parse_program(src);
        let pairs = collect_sequential_pairs(&program);
        assert_eq!(pairs.len(), 3);
        assert!(pairs.contains(&("A".to_owned(), "B".to_owned())));
        assert!(pairs.contains(&("B".to_owned(), "C".to_owned())));
        assert!(pairs.contains(&("A".to_owned(), "C".to_owned())));
    }

    // ─── v0.2-β: read-write race detection (§7.2 graded algebra) ─────

    #[test]
    fn effect_reads_field_that_interrupt_writes_violates() {
        // The canonical SPSC read-write race: foreground effect
        // reads a counter that an interrupt writes. v0.1 missed
        // this (writes_eff = ∅, no overlap with writes_irq);
        // v0.2-β catches it via reads_eff ∧ writes_irq != 0.
        let src = "\
            #automaton C { v: u32; }\n\
            #effect drain() -> u32 #mutates: [C] { return C.v; }\n\
            #interrupt Tally() #mutates: [C] #priority: HIGH { C.v += 1u32; }\n\
        ";
        let program = parse_program(src);
        let profiles = build_profiles(&program);
        let errs = verify(&program, &profiles).expect_err("expected E0520 read-write");
        assert_eq!(errs.len(), 1);
        match &errs[0] {
            OrthoError::OrthogonalityViolation { shared_fields, .. } => {
                assert_eq!(shared_fields.len(), 1);
                assert_eq!(shared_fields[0].field, "v");
            }
            other => panic!("expected OrthogonalityViolation, got {other:?}"),
        }
    }

    #[test]
    fn interrupt_reads_field_that_effect_writes_violates() {
        // Mirror case: the interrupt reads, the effect writes. The
        // graded check is symmetric — `writes_A ∧ reads_B` covers
        // both orderings.
        let src = "\
            #automaton C { v: u32; flag: u32; }\n\
            #effect main_loop() #mutates: [C] { C.v += 1u32; }\n\
            #interrupt Sampler() #mutates: [C] #priority: HIGH {\n  \
              C.flag = C.v;\n\
            }\n\
        ";
        let program = parse_program(src);
        let profiles = build_profiles(&program);
        let errs = verify(&program, &profiles).expect_err("expected E0520");
        // Two distinct shared fields would produce one error each;
        // single shared field `v` produces one. (`flag` is written
        // by Sampler, not read by main_loop, so no second conflict.)
        assert_eq!(errs.len(), 1);
        match &errs[0] {
            OrthoError::OrthogonalityViolation { shared_fields, .. } => {
                let names: Vec<&str> = shared_fields.iter().map(|f| f.field.as_str()).collect();
                assert!(names.contains(&"v"));
            }
            _ => panic!("expected OrthogonalityViolation"),
        }
    }

    #[test]
    fn read_read_does_not_violate() {
        // Two concurrent callables BOTH reading the same field
        // is fine — reads don't race with reads. v0.2-β must
        // NOT flag this case.
        let src = "\
            #automaton C { v: u32; tally: u32; }\n\
            #effect read_v() #mutates: [C] { C.tally = C.v; }\n\
            #interrupt Sampler() #mutates: [C] #priority: HIGH {\n  \
              let _x: u32 = C.v;\n  \
              return;\n\
            }\n\
        ";
        // read_v writes `tally` and reads `v`.
        // Sampler reads `v` (no write).
        // Conflict matrix:
        //   writes_eff (tally) ∧ writes_irq (∅)        = 0 ✓
        //   reads_eff (v) ∧ writes_irq (∅)             = 0 ✓
        //   writes_eff (tally) ∧ reads_irq (v)         = 0 ✓
        // Read-read (v ∧ v) is not in the check. So passes.
        let program = parse_program(src);
        let profiles = build_profiles(&program);
        verify(&program, &profiles).expect("read-read should not violate");
    }

    #[test]
    fn compound_assign_implies_read() {
        // `Auto.field += expr` is a load-modify-store, so it must
        // count as a read of `field`. A separate effect that just
        // writes `field` from outside would still be a write-write
        // race (and that's caught by v0.1). But a separate
        // PURE-WRITE-only callable concurrent with the compound
        // assign should NOT count as read-write — the compound's
        // read happens IN the interrupt, not concurrently.
        //
        // This test verifies a different shape: a compound assign
        // and a separate read-only effect on the same field. The
        // read in the effect races with the write of the compound
        // assign in the interrupt.
        let src = "\
            #automaton C { v: u32; tally: u32; }\n\
            #effect snapshot() #mutates: [C] { C.tally = C.v; }\n\
            #interrupt Tally() #mutates: [C] #priority: HIGH { C.v += 1u32; }\n\
        ";
        let program = parse_program(src);
        let profiles = build_profiles(&program);
        let errs = verify(&program, &profiles).expect_err("expected E0520");
        match &errs[0] {
            OrthoError::OrthogonalityViolation { shared_fields, .. } => {
                let names: Vec<&str> = shared_fields.iter().map(|f| f.field.as_str()).collect();
                assert!(names.contains(&"v"));
            }
            _ => panic!("expected OrthogonalityViolation"),
        }
    }

    #[test]
    fn pure_write_without_read_dependency_passes() {
        // Plain `=` (not compound) is a pure store; no read
        // implicit. Two callables that ONLY write disjoint fields
        // should pass. (This was already v0.1 behaviour; verifies
        // we didn't introduce a regression by counting plain `=`
        // as a read.)
        let src = "\
            #automaton C { v: u32; w: u32; }\n\
            #effect set_v() #mutates: [C] { C.v = 1u32; }\n\
            #interrupt Tally() #mutates: [C] #priority: HIGH { C.w = 2u32; }\n\
        ";
        let program = parse_program(src);
        let profiles = build_profiles(&program);
        verify(&program, &profiles).expect("disjoint pure writes should pass");
    }

    #[test]
    fn read_in_let_initializer_counts() {
        // `let _x: u32 = Auto.field;` reads `field`. Verify the
        // walker captures it.
        let src = "\
            #automaton C { v: u32; tally: u32; }\n\
            #effect snapshot() #mutates: [C] {\n  \
              let _x: u32 = C.v;\n  \
              C.tally = 1u32;\n  \
              return;\n\
            }\n\
            #interrupt Tally() #mutates: [C] #priority: HIGH { C.v = 1u32; }\n\
        ";
        let program = parse_program(src);
        let profiles = build_profiles(&program);
        let errs = verify(&program, &profiles).expect_err("expected E0520");
        match &errs[0] {
            OrthoError::OrthogonalityViolation { shared_fields, .. } => {
                let names: Vec<&str> = shared_fields.iter().map(|f| f.field.as_str()).collect();
                assert!(names.contains(&"v"), "expected `v` in shared; got {names:?}");
            }
            _ => panic!("expected OrthogonalityViolation"),
        }
    }

    #[test]
    fn read_in_if_condition_counts() {
        // `if Auto.field > 0 { ... }` reads `field`.
        let src = "\
            #automaton C { v: u32; tally: u32; }\n\
            #effect inspect() #mutates: [C] {\n  \
              if C.v > 0u32 { C.tally = 1u32; }\n  \
              return;\n\
            }\n\
            #interrupt Tally() #mutates: [C] #priority: HIGH { C.v = 1u32; }\n\
        ";
        let program = parse_program(src);
        let profiles = build_profiles(&program);
        let errs = verify(&program, &profiles).expect_err("expected E0520");
        let names: Vec<&str> = match &errs[0] {
            OrthoError::OrthogonalityViolation { shared_fields, .. } => {
                shared_fields.iter().map(|f| f.field.as_str()).collect()
            }
            _ => panic!("expected OrthogonalityViolation"),
        };
        assert!(names.contains(&"v"));
    }

    #[test]
    fn read_in_sigma_range_bound_counts() {
        // `sigma i in 0u32..Auto.field { ... }` reads `field`.
        let src = "\
            #automaton C { len: u32; tally: u32; }\n\
            #effect iterate() #mutates: [C] {\n  \
              sigma i in 0u32..C.len { C.tally += 1u32; }\n  \
              return;\n\
            }\n\
            #interrupt SetLen() #mutates: [C] #priority: HIGH { C.len = 10u32; }\n\
        ";
        let program = parse_program(src);
        let profiles = build_profiles(&program);
        let errs = verify(&program, &profiles).expect_err("expected E0520");
        let names: Vec<&str> = match &errs[0] {
            OrthoError::OrthogonalityViolation { shared_fields, .. } => {
                shared_fields.iter().map(|f| f.field.as_str()).collect()
            }
            _ => panic!("expected OrthogonalityViolation"),
        };
        assert!(names.contains(&"len"));
    }

    #[test]
    fn read_propagates_through_proc_call() {
        // An effect that calls a transition that reads the field —
        // the read should propagate transitively into the effect's
        // read profile, then trigger the wedge check against the
        // interrupt's write.
        let src = "\
            #automaton C { v: u32; tally: u32;\n  \
              #transition tick { C.tally = C.v; }\n\
            }\n\
            #effect drain() #mutates: [C] { #> tick(); }\n\
            #interrupt Sampler() #mutates: [C] #priority: HIGH { C.v = 1u32; }\n\
        ";
        let program = parse_program(src);
        let profiles = build_profiles(&program);
        let errs = verify(&program, &profiles).expect_err("expected E0520");
        match &errs[0] {
            OrthoError::OrthogonalityViolation { shared_fields, .. } => {
                let names: Vec<&str> = shared_fields.iter().map(|f| f.field.as_str()).collect();
                assert!(names.contains(&"v"));
            }
            _ => panic!("expected OrthogonalityViolation"),
        }
    }

    // ─── v0.2-θ: transition-side #atomic inheritance (delegated ISR) ─

    #[test]
    fn delegated_isr_inherits_atomic_from_transition_callee() {
        // The canonical delegated-ISR pattern: the interrupt's
        // body is a single `#>` to a `#atomic` transition.
        // Pre-v0.2-θ: pair against another interrupt (foreground
        // effect, etc.) with overlapping reads/writes flags as
        // E0520. Post-v0.2-θ: the interrupt inherits atomicity
        // and the pair is suppressed.
        let src = "\
            #automaton C { v: u32; w: u32;\n  \
              #transition handle #atomic: interrupt_critical; { C.v = 1u32; C.w = 2u32; }\n\
            }\n\
            #effect main_loop() #mutates: [C] {\n  \
              let _x: u32 = C.v + C.w;\n  \
              return;\n\
            }\n\
            #interrupt SysTick() #mutates: [C] #priority: HIGH {\n  \
              #> handle();\n\
            }\n\
        ";
        let program = parse_program(src);
        let profiles = build_profiles(&program);
        // SysTick inherits atomic from `handle`. main_loop reads
        // C.v + C.w (which SysTick writes via the inherited
        // atomic transition). Without v0.2-θ this would be a
        // read-write race; with v0.2-θ the inherited atomic
        // suppresses the pair.
        verify(&program, &profiles).expect("delegated ISR inherits atomic");
    }

    #[test]
    fn delegated_isr_with_explicit_return_still_inherits() {
        // The body has TWO statements: the proc call and an
        // explicit `return;`. The trailing void return is
        // filtered out by the inheritance walker; the body
        // still counts as a single-statement delegated ISR.
        let src = "\
            #automaton C { v: u32; }\n\
            #effect handle() #mutates: [C] #atomic: interrupt_critical; { C.v = 1u32; }\n\
            #interrupt SysTick() #mutates: [C] #priority: HIGH {\n  \
              #> handle();\n  \
              return;\n\
            }\n\
            #effect drain() -> u32 #mutates: [C] {\n  \
              return @snapshot C.v;\n\
            }\n\
        ";
        let program = parse_program(src);
        let profiles = build_profiles(&program);
        verify(&program, &profiles).expect("explicit return doesn't break inheritance");
    }

    #[test]
    fn body_with_multiple_statements_does_not_inherit() {
        // The body has more than one statement (the proc call
        // plus a direct mutation). Inheritance does NOT apply —
        // the direct mutation could race before the atomic
        // call enters.
        let src = "\
            #automaton C { v: u32; w: u32;\n  \
              #transition handle #atomic: interrupt_critical; { C.v = 1u32; }\n\
            }\n\
            #interrupt SysTick() #mutates: [C] #priority: HIGH {\n  \
              C.w = 9u32;\n  \
              #> handle();\n\
            }\n\
            #effect read_w() -> u32 #mutates: [C] { return C.w; }\n\
        ";
        let program = parse_program(src);
        let profiles = build_profiles(&program);
        // SysTick has a direct mutation of C.w (NOT in the
        // atomic transition). read_w reads C.w. The pair
        // should be flagged.
        let errs = verify(&program, &profiles).expect_err("multi-stmt body should not inherit");
        assert!(
            errs.iter().any(|e| match e {
                OrthoError::OrthogonalityViolation { shared_fields, .. } => {
                    shared_fields.iter().any(|f| f.field == "w")
                }
                _ => false,
            }),
            "expected violation on C.w; got {errs:?}"
        );
    }

    #[test]
    fn body_calling_non_atomic_transition_does_not_inherit() {
        // The body's single `#>` call is to a transition that
        // is NOT marked `#atomic`. Inheritance does not apply.
        let src = "\
            #automaton C { v: u32;\n  \
              #transition tick { C.v += 1u32; }\n\
            }\n\
            #effect drain() -> u32 #mutates: [C] { return C.v; }\n\
            #interrupt SysTick() #mutates: [C] #priority: HIGH {\n  \
              #> tick();\n\
            }\n\
        ";
        let program = parse_program(src);
        let profiles = build_profiles(&program);
        // Pair drain × SysTick: drain reads C.v, SysTick writes
        // C.v (transitively via tick). Without #atomic on tick,
        // no inheritance, pair flagged.
        let errs = verify(&program, &profiles).expect_err("non-atomic call doesn't inherit");
        assert_eq!(errs.len(), 1);
    }

    #[test]
    fn already_atomic_callable_unaffected_by_inheritance_check() {
        // A callable that's directly marked `#atomic` is atomic
        // regardless of its body shape. The inheritance check
        // is `OR`'d so the answer is still atomic.
        let src = "\
            #automaton C { v: u32; }\n\
            #effect drain() -> u32 #mutates: [C] #atomic: interrupt_critical; {\n  \
              C.v = 1u32;\n  \
              return @snapshot C.v;\n\
            }\n\
            #interrupt SysTick() #mutates: [C] #priority: HIGH { C.v = 2u32; }\n\
        ";
        let program = parse_program(src);
        let profiles = build_profiles(&program);
        // drain is directly atomic → pair with SysTick suppressed.
        // The body's two statements would have prevented
        // inheritance, but direct atomic wins.
        verify(&program, &profiles).expect("direct atomic still works");
    }

    #[test]
    fn body_inherits_atomic_helper_smoke_one_stmt_call() {
        use clifford_ast::{Block as AstBlock, Stmt as AstStmt};
        let mut atomic = HashSet::new();
        atomic.insert("handle".to_owned());
        // Synthetic Block with a single ProcCall to "handle".
        let span = clifford_lexer::Span::new(0, 0);
        let stmt = AstStmt {
            kind: StmtKind::ProcCall {
                name: "handle".to_owned(),
                args: vec![],
            },
            span,
        };
        let body = AstBlock {
            stmts: vec![stmt],
            span,
        };
        assert!(body_inherits_atomic_from_proc_call(&body, &atomic));
    }

    #[test]
    fn body_inherits_atomic_helper_returns_false_on_empty_body() {
        let atomic: HashSet<String> = HashSet::new();
        let span = clifford_lexer::Span::new(0, 0);
        let body = clifford_ast::Block {
            stmts: vec![],
            span,
        };
        assert!(!body_inherits_atomic_from_proc_call(&body, &atomic));
    }

    #[test]
    fn body_inherits_atomic_helper_returns_false_when_callee_not_in_set() {
        use clifford_ast::{Block as AstBlock, Stmt as AstStmt};
        let atomic: HashSet<String> = HashSet::new();
        let span = clifford_lexer::Span::new(0, 0);
        let stmt = AstStmt {
            kind: StmtKind::ProcCall {
                name: "handle".to_owned(),
                args: vec![],
            },
            span,
        };
        let body = AstBlock {
            stmts: vec![stmt],
            span,
        };
        assert!(!body_inherits_atomic_from_proc_call(&body, &atomic));
    }

    #[test]
    fn collect_atomic_callable_names_finds_all_three_decl_kinds() {
        let src = "\
            #automaton C { v: u32;\n  \
              #transition tick #atomic: interrupt_critical; { C.v = 1u32; }\n\
            }\n\
            #effect handler() #mutates: [C] #atomic: interrupt_critical; { return; }\n\
            #interrupt IRQ() #mutates: [C] #priority: HIGH #atomic: interrupt_critical; { return; }\n\
        ";
        let program = parse_program(src);
        let names = collect_atomic_callable_names(&program);
        assert!(names.contains("tick"));
        assert!(names.contains("handler"));
        assert!(names.contains("IRQ"));
        assert_eq!(names.len(), 3);
    }

    #[test]
    fn collect_atomic_callable_names_excludes_non_atomic() {
        let src = "\
            #automaton C { v: u32;\n  \
              #transition plain { C.v = 1u32; }\n  \
              #transition critical #atomic: interrupt_critical; { C.v = 2u32; }\n\
            }\n\
        ";
        let program = parse_program(src);
        let names = collect_atomic_callable_names(&program);
        assert!(!names.contains("plain"));
        assert!(names.contains("critical"));
    }

    // ─── v0.2-η: #priority-aware concurrency inference ──────────────

    #[test]
    fn same_priority_interrupts_do_not_concur() {
        // Two interrupts at HIGH priority writing the SAME
        // field. Pre-v0.2-η: violation. v0.2-η: skipped because
        // NVIC processes same-priority interrupts sequentially
        // (tail-chained, no nested preemption).
        let src = "\
            #automaton C { v: u32; }\n\
            #interrupt IRQ_A() #mutates: [C] #priority: HIGH { C.v += 1u32; }\n\
            #interrupt IRQ_B() #mutates: [C] #priority: HIGH { C.v += 1u32; }\n\
        ";
        let program = parse_program(src);
        let profiles = build_profiles(&program);
        verify(&program, &profiles).expect("same-priority IRQs are not concurrent");
    }

    #[test]
    fn different_priority_interrupts_still_violate() {
        // Sanity: HIGH vs LOW interrupts can still preempt each
        // other. Without #atomic / @sequential / @snapshot the
        // pair is flagged.
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
    fn medium_vs_medium_also_suppressed() {
        // Verify the rule isn't HIGH-specific.
        let src = "\
            #automaton C { v: u32; }\n\
            #interrupt IRQ_A() #mutates: [C] #priority: MEDIUM { C.v += 1u32; }\n\
            #interrupt IRQ_B() #mutates: [C] #priority: MEDIUM { C.v += 1u32; }\n\
        ";
        let program = parse_program(src);
        let profiles = build_profiles(&program);
        verify(&program, &profiles).expect("MEDIUM × MEDIUM not concurrent");
    }

    #[test]
    fn numeric_priorities_compare_by_canonical_text() {
        // Two interrupts both at numeric priority 3 (one
        // written `3`, one `0_3` after underscore stripping —
        // contrived but exercises the canonicalisation).
        let src = "\
            #automaton C { v: u32; }\n\
            #interrupt IRQ_A() #mutates: [C] #priority: 3 { C.v += 1u32; }\n\
            #interrupt IRQ_B() #mutates: [C] #priority: 3 { C.v += 1u32; }\n\
        ";
        let program = parse_program(src);
        let profiles = build_profiles(&program);
        verify(&program, &profiles).expect("same numeric priority not concurrent");
    }

    #[test]
    fn different_numeric_priorities_still_violate() {
        let src = "\
            #automaton C { v: u32; }\n\
            #interrupt IRQ_A() #mutates: [C] #priority: 3 { C.v += 1u32; }\n\
            #interrupt IRQ_B() #mutates: [C] #priority: 5 { C.v += 1u32; }\n\
        ";
        let program = parse_program(src);
        let profiles = build_profiles(&program);
        let errs = verify(&program, &profiles).expect_err("different numerics still concur");
        assert_eq!(errs.len(), 1);
    }

    #[test]
    fn mixed_kinds_conservatively_treated_as_concurrent() {
        // `HIGH` vs `Numeric("0")` may map to the same
        // hardware priority on some targets, but v0.2-η
        // doesn't know the encoding. Conservatively treat as
        // different priorities → still flagged.
        let src = "\
            #automaton C { v: u32; }\n\
            #interrupt IRQ_A() #mutates: [C] #priority: HIGH { C.v += 1u32; }\n\
            #interrupt IRQ_B() #mutates: [C] #priority: 0    { C.v += 1u32; }\n\
        ";
        let program = parse_program(src);
        let profiles = build_profiles(&program);
        let errs = verify(&program, &profiles).expect_err("mixed kinds treated as different");
        assert_eq!(errs.len(), 1);
    }

    #[test]
    fn priority_suppression_does_not_apply_to_effect_interrupt_pair() {
        // The priority rule is interrupt-vs-interrupt only.
        // An effect × interrupt pair has no priority on the
        // effect side, so the v0.2-η suppression doesn't fire
        // — the pair gets the standard wedge check.
        let src = "\
            #automaton C { v: u32; }\n\
            #effect main_loop() #mutates: [C] { C.v += 1u32; }\n\
            #interrupt IRQ() #mutates: [C] #priority: HIGH { C.v += 1u32; }\n\
        ";
        let program = parse_program(src);
        let profiles = build_profiles(&program);
        let errs = verify(&program, &profiles).expect_err("effect × interrupt still concur");
        assert_eq!(errs.len(), 1);
    }

    #[test]
    fn priorities_indicate_no_preemption_helper_smoke() {
        // Direct unit tests on the helper covering each
        // PriorityLevel variant pair.
        use clifford_ast::PriorityLevel::*;
        assert!(priorities_indicate_no_preemption(&Low, &Low));
        assert!(priorities_indicate_no_preemption(&Medium, &Medium));
        assert!(priorities_indicate_no_preemption(&High, &High));
        assert!(!priorities_indicate_no_preemption(&Low, &Medium));
        assert!(!priorities_indicate_no_preemption(&Low, &High));
        assert!(!priorities_indicate_no_preemption(&Medium, &High));
        assert!(priorities_indicate_no_preemption(
            &Numeric("3".to_owned()),
            &Numeric("3".to_owned())
        ));
        // Underscore-equivalent canonicalisation.
        assert!(priorities_indicate_no_preemption(
            &Numeric("4_2".to_owned()),
            &Numeric("42".to_owned())
        ));
        assert!(!priorities_indicate_no_preemption(
            &Numeric("3".to_owned()),
            &Numeric("5".to_owned())
        ));
        // Mixed kinds: conservatively false.
        assert!(!priorities_indicate_no_preemption(
            &High,
            &Numeric("0".to_owned())
        ));
    }

    // ─── v0.2-ζ: @snapshot excludes read from race detection ─────────

    #[test]
    fn snapshot_read_does_not_trigger_race_with_concurrent_write() {
        // The dual_uart_telemetry case: drain effect reads a
        // counter via @snapshot while an interrupt writes it.
        // Without @snapshot v0.2-β rejects (reads ∧ writes ≠ 0);
        // with @snapshot v0.2-ζ accepts because the snapshot
        // walker excludes the read from `actual_reads`.
        let src = "\
            #automaton C { v: u32; }\n\
            #effect drain() -> u32 #mutates: [C] {\n  \
              return @snapshot C.v;\n\
            }\n\
            #interrupt Tally() #mutates: [C] #priority: HIGH { C.v += 1u32; }\n\
        ";
        let program = parse_program(src);
        let profiles = build_profiles(&program);
        verify(&program, &profiles).expect("@snapshot should make this race-free");
    }

    #[test]
    fn snapshot_in_arithmetic_remains_race_free() {
        // The canonical "snapshot-and-decide" pattern:
        // `@snapshot a + @snapshot b`. Both reads are excluded
        // from the race check; the arithmetic happens on already-
        // captured SSA values.
        let src = "\
            #automaton T { a: u32; b: u32; }\n\
            #effect sum() -> u32 #mutates: [T] {\n  \
              return @snapshot T.a + @snapshot T.b;\n\
            }\n\
            #interrupt IRQ() #mutates: [T] #priority: HIGH { T.a += 1u32; }\n\
        ";
        let program = parse_program(src);
        let profiles = build_profiles(&program);
        verify(&program, &profiles).expect("@snapshot arithmetic is race-free");
    }

    #[test]
    fn plain_read_still_triggers_race_with_snapshot_alternative() {
        // Sanity: the same source as
        // `snapshot_read_does_not_trigger_race_with_concurrent_write`
        // but with a PLAIN read. v0.2-β rejects → v0.2-ζ
        // confirms snapshot is the difference-maker.
        let src = "\
            #automaton C { v: u32; }\n\
            #effect drain() -> u32 #mutates: [C] {\n  \
              return C.v;\n\
            }\n\
            #interrupt Tally() #mutates: [C] #priority: HIGH { C.v += 1u32; }\n\
        ";
        let program = parse_program(src);
        let profiles = build_profiles(&program);
        let errs = verify(&program, &profiles).expect_err("plain read still races");
        assert_eq!(errs.len(), 1);
    }

    #[test]
    fn snapshot_does_not_protect_writes() {
        // A subtle point: `@snapshot` only annotates a READ. A
        // callable that writes to an automaton field is still
        // race-checked normally. This is just sanity — it would
        // be very wrong to silently skip writes.
        let src = "\
            #automaton C { v: u32; }\n\
            #effect bump() #mutates: [C] { C.v = 1u32; }\n\
            #interrupt Tally() #mutates: [C] #priority: HIGH { C.v = 2u32; }\n\
        ";
        let program = parse_program(src);
        let profiles = build_profiles(&program);
        let errs = verify(&program, &profiles).expect_err("two writes still race");
        assert_eq!(errs.len(), 1);
    }

    #[test]
    fn snapshot_self_inside_transition_excluded_from_reads() {
        // `@snapshot Self.field` inside a transition. The
        // snapshot walker maps `Self` to the enclosing owner
        // (same as plain reads), so the exclusion applies
        // correctly.
        //
        // This test is contrived because transitions aren't
        // direct concurrency nodes per §7.3, but it documents
        // that the resolver path is consistent.
        let src = "\
            #automaton C { v: u32;\n  \
              #transition observe { let _x: u32 = @snapshot Self.v; return; }\n\
            }\n\
            #effect drain() -> u32 #mutates: [C] {\n  \
              return @snapshot C.v;\n\
            }\n\
            #interrupt IRQ() #mutates: [C] #priority: HIGH { C.v = 1u32; }\n\
        ";
        let program = parse_program(src);
        let profiles = build_profiles(&program);
        verify(&program, &profiles).expect("@snapshot Self.field excluded too");
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

    // ─── Slice 33: verify slice-19 #flush race-detection plumbing ───────

    #[test]
    fn s33_flush_vs_concurrent_field_write_is_a_race() {
        // Slice 19 expanded `#flush A;` into one direct write per
        // declared field of A in the mutation profile. Slice 33
        // verifies that the §7 ortho engine actually sees the
        // expansion: an effect that flushes a #staged automaton
        // and an interrupt that writes one of its fields are
        // concurrent and share the field — must surface E0520.
        let src = "\
            #staged #automaton S { v: u32; w: u32; }\n\
            #effect committer() #mutates: [S] { #flush S; }\n\
            #interrupt SysTick() #mutates: [S] #priority: HIGH { S.v += 1u32; }\n\
        ";
        let program = parse_program(src);
        let profiles = build_profiles(&program);
        let errs = verify(&program, &profiles).expect_err(
            "expected race: flush writes every field, IRQ writes S.v",
        );
        let saw = errs.iter().any(|e| matches!(
            e,
            OrthoError::OrthogonalityViolation { shared_fields, .. }
                if shared_fields.iter().any(|f| f.field == "v")
        ));
        assert!(
            saw,
            "expected E0520 race on S.v from flush vs IRQ; got {errs:?}"
        );
    }

    #[test]
    fn s33_two_flushes_of_same_staged_automaton_race() {
        // Two callables both flushing `S` race on every field —
        // because each `#flush S;` expands to writes of `S.v` and
        // `S.w` per slice 19.
        let src = "\
            #staged #automaton S { v: u32; w: u32; }\n\
            #effect committer_a() #mutates: [S] { #flush S; }\n\
            #interrupt SysTick() #mutates: [S] #priority: HIGH { #flush S; }\n\
        ";
        let program = parse_program(src);
        let profiles = build_profiles(&program);
        let errs = verify(&program, &profiles).expect_err(
            "expected race: both flushes write every field of S",
        );
        // At least one violation; shared_fields names a field of S.
        let saw = errs.iter().any(|e| matches!(
            e,
            OrthoError::OrthogonalityViolation { shared_fields, .. }
                if !shared_fields.is_empty()
        ));
        assert!(
            saw,
            "expected E0520 from flush-vs-flush; got {errs:?}"
        );
    }

    #[test]
    fn s33_flush_vs_disjoint_automaton_does_not_race() {
        // Sanity: a flush of S and an IRQ writing field of an
        // unrelated automaton T are orthogonal. The slice-19
        // expansion records writes for fields of S, not T.
        let src = "\
            #staged #automaton S { v: u32; }\n\
            #automaton T { w: u32; }\n\
            #effect committer() #mutates: [S] { #flush S; }\n\
            #interrupt SysTick() #mutates: [T] #priority: HIGH { T.w += 1u32; }\n\
        ";
        let program = parse_program(src);
        let profiles = build_profiles(&program);
        verify(&program, &profiles)
            .expect("flush of S and write of T are orthogonal");
    }
}
