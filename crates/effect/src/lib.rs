//! # clifford-effect
//!
//! Effect & FSM extraction for the Clifford compiler. Implements §6 of
//! `docs/CLIFFORD_SPEC.md` and the categorical foundation formalised in
//! Appendix B.
//!
//! ## Per-automaton outputs (final scope)
//!
//! For each `#automaton A` declaration, this phase constructs:
//!
//! 1. **The category C_A** (§6.1): objects = `A.#states` (or synthetic
//!    `[Ready]` for monoid automata per Decision #5 Rule 4); morphisms =
//!    `#transition` declarations + implicit identities. Standard reachability
//!    and deadlock analysis run uniformly across multi-state and monoid forms.
//! 2. **Per-effect mutation profile** (§6.2): the set of `(automaton, field)`
//!    pairs each effect actually writes via `#mutate` statements (canonical or
//!    sugar form), unioned with the transitive `#mutates` of `#>`-called
//!    callees. Verified to be a subset of the declared `#mutates` clause.
//! 3. **Per-effect read profile** (§6.2 inferred): fields and `static` paths
//!    each effect reads.
//! 4. **Effect-procedure call graph** (§6.3): edges are `#> name(args)` calls;
//!    each edge labelled with `CallContext` (Transition, Identity, Generic per
//!    Refinement #5b's generalisation).
//! 5. **State-tag update points** (§6.4): the body-completion location at
//!    which each `#transition`'s state-tag write fires.
//! 6. **Interrupt-overlap set R(A)** (Refinement #5e): `{ I | I is a
//!    #interrupt and I.#mutates names A transitively }`. Drives §8.4's
//!    transition-atomicity wrapping decision.
//!
//! ## Output
//!
//! The phase produces structured values consumed by `clifford-ortho`
//! (§7 GA orthogonality engine) and `clifford-codegen` (§8 lowering).
//!
//! ## Implementation status
//!
//! **Slice 1:** §6.1 category construction — [`extract_categories`].
//!
//! **Slice 2:** §6.2 mutation profile extraction — [`extract_mutation_profiles`].
//!
//! **Slice 3:** §6.3 proc-call graph + cycle detection —
//! [`extract_call_graph`].
//!
//! **Slice 4 (this PR):** Refinement #5e interrupt-overlap set —
//! [`compute_interrupt_overlap`]. For each `#automaton A`, computes
//! `R(A)` = the set of `#interrupt` declarations that mutate `A`
//! transitively (per their `#mutates` clause expanded through `#>`
//! proc-calls). Drives `clifford-codegen`'s §8.4 transition-atomicity
//! wrapping decision: any `#transition` of `A` that runs in user code
//! must be wrapped in a critical section (CLI/STI on Cortex-M; SIE
//! disable on RISC-V) iff `R(A)` is non-empty for that automaton.
//! §6.4 state-tag update points, §6.5 invariant verification, and
//! §6.6 atomic-annotation lowering hints arrive in subsequent slices.

#![forbid(unsafe_code)]
#![warn(missing_docs)]

use std::collections::{HashMap, HashSet};

use clifford_ast::{
    AssignOp, AutomatonDecl, Block, Expr, ExprKind, FieldAssign, Item, Program, Stmt, StmtKind,
    TransitionDecl,
};
use clifford_lexer::Span;
use clifford_resolve::{BindingRef, CallContext, Resolution, SymbolKind};
use thiserror::Error;

/// Errors produced during effect / FSM extraction.
///
/// Reserves the `E06xx` range. Per `docs/CLIFFORD_SPEC.md`:
///
/// - `E0410`/`E0411` — mutation-profile mismatches (§6.2; slice E2).
/// - `E0420`–`E0422` — proc-call resolution (§6.3; slice E3).
/// - `E0430`–`E0439` — state-tag / transition validity (§6.1, §6.4; this slice).
/// - `E0610`–`E0613` — register-block field validity (separate phase).
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum EffectError {
    /// A `#transition Source -> Target` (or `#transition T -> Target`)
    /// references `Target` that isn't in the enclosing automaton's
    /// `#states` declaration.
    ///
    /// For monoid automata (no `#states` clause), every `#transition T ->
    /// Target` is an error because monoid automata have no destinations
    /// (their transitions stay in the synthetic `[Ready]` state). Use
    /// `#transition T { … }` (no `-> Target`) for monoid transitions.
    #[error("E0430: unknown state `{target}` in transition `{transition}` of automaton `{automaton}` (at byte {at})")]
    UnknownState {
        /// The automaton owning the transition.
        automaton: String,
        /// The transition name.
        transition: String,
        /// The bad target state.
        target: String,
        /// Byte offset of the transition declaration.
        at: usize,
    },

    /// A monoid automaton (no `#states` clause) has a `#transition` with
    /// an explicit destination. Monoid automata have only the synthetic
    /// `[Ready]` state; transitions inside them must omit the `-> Target`
    /// clause.
    #[error("E0431: monoid automaton `{automaton}` has transition `{transition}` with destination `{target}` (at byte {at}); monoid transitions cannot specify a destination")]
    MonoidTransitionWithDestination {
        /// The automaton owning the transition.
        automaton: String,
        /// The transition name.
        transition: String,
        /// The destination the user wrote.
        target: String,
        /// Byte offset of the transition.
        at: usize,
    },

    /// Two `#transition`s in the same automaton share a name.
    #[error("E0432: duplicate transition `{transition}` in automaton `{automaton}` (at byte {duplicate_at}; first declared at byte {original_at})")]
    DuplicateTransition {
        /// The automaton.
        automaton: String,
        /// The duplicated transition name.
        transition: String,
        /// Byte offset of the first declaration.
        original_at: usize,
        /// Byte offset of the duplicate declaration.
        duplicate_at: usize,
    },

    /// Two state names in the same automaton's `#states` clause collide.
    #[error("E0433: duplicate state `{state}` in automaton `{automaton}` (at byte {duplicate_at}; first declared at byte {original_at})")]
    DuplicateState {
        /// The automaton.
        automaton: String,
        /// The duplicated state name.
        state: String,
        /// Byte offset of the first occurrence.
        original_at: usize,
        /// Byte offset of the duplicate.
        duplicate_at: usize,
    },

    /// An effect or interrupt mutates an automaton not listed in its
    /// `#mutates: [...]` clause (transitively through `#> proc()` calls).
    #[error("E0410: effect `{callable}` mutates undeclared automaton `{automaton}` (at byte {at})")]
    EffectMutatesUndeclaredAutomaton {
        /// The effect / interrupt name.
        callable: String,
        /// The undeclared automaton name.
        automaton: String,
        /// Byte offset of the callable's declaration.
        at: usize,
    },

    /// An effect or interrupt mutates an automaton listed in its
    /// `#cannot_mutate: [...]` clause (transitively through `#> proc()` calls).
    #[error("E0411: effect `{callable}` mutates excluded automaton `{automaton}` (at byte {at})")]
    EffectMutatesExcludedAutomaton {
        /// The effect / interrupt name.
        callable: String,
        /// The excluded automaton name.
        automaton: String,
        /// Byte offset of the callable's declaration.
        at: usize,
    },

    /// The proc-call graph contains a cycle (e.g. effect a calls b,
    /// b calls a). Per spec §6.3 step 6, cycles are rejected unless
    /// explicitly marked recursive — and v0.1 has no recursion-marker
    /// syntax, so all cycles are rejected.
    ///
    /// The diagnostic names every callable on the cycle, in some stable
    /// order (the order they appear in the cycle traversal); a renderer
    /// can use this to draw the dependency loop for the user.
    #[error("E0422: proc-call cycle detected: {cycle_display}")]
    ProcCallCycle {
        /// Members of the cycle in the order encountered. Always at
        /// least one element. A self-loop has one member; a 2-cycle
        /// has two; etc.
        cycle: Vec<String>,
        /// Pre-formatted display string for the cycle, e.g.
        /// `` "`a` → `b` → `c` → `a`" ``. Pre-rendering avoids
        /// re-formatting at error-rendering time.
        cycle_display: String,
    },
}

/// The category `C_A` of an automaton — its objects (states), its
/// morphisms (transitions + implicit identities), and metadata about
/// the initial state.
///
/// Per Appendix B and Decision #5: for monoid automata (no `#states`
/// clause) the category has one object `Ready` and one explicit
/// transition family of morphisms `Ready -> Ready` (each `#transition`
/// inside the body), plus the implicit identity. For multi-state automata
/// the category has the declared states as objects and one morphism per
/// `#transition` plus an identity at every state.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AutomatonCategory {
    /// The automaton's name.
    pub name: String,
    /// The automaton's index into `Program.items` (stable for the lifetime
    /// of the AST).
    pub item_index: usize,
    /// Whether this is a monoid automaton (no `#states` clause). Monoids
    /// have a single synthetic `Ready` object and every transition is
    /// implicitly `Ready -> Ready`.
    pub is_monoid: bool,
    /// State names — the objects of the category. For monoid automata,
    /// the single entry is `"Ready"`. For multi-state automata, the
    /// entries are the declared `#states` in source order.
    pub states: Vec<StateInfo>,
    /// Transitions — the explicit morphisms of the category. Each
    /// transition has a source state (the implicit predecessor — slice E1
    /// does not yet pin this; for monoid automata it's `Ready`; for
    /// multi-state automata the source must be inferred from the
    /// transition's body or from per-transition `#from:` clauses in a
    /// future slice — currently set to `None`), an optional destination
    /// (the explicit `-> Target`), and a body-span pointer back to the
    /// AST.
    pub transitions: Vec<TransitionInfo>,
    /// The initial state's index into `states`. For monoid automata it's 0
    /// (the synthetic `Ready`). For multi-state automata it's the first
    /// state in declaration order (per §6.1 step 1; the `@initial` marker
    /// is honored when implemented in a future slice).
    pub initial: usize,
}

/// One state in an automaton — an object of the category C_A.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StateInfo {
    /// State name.
    pub name: String,
    /// Source span of the state-name declaration. For the synthetic
    /// monoid `Ready` state, this is the automaton's own span.
    pub span: Span,
}

/// One transition in an automaton — a morphism of C_A.
///
/// Carries enough information for downstream phases to (a) name the
/// transition for diagnostics, (b) navigate to its body via
/// `body_span`, and (c) reason about the state-tag update at exit.
/// The implicit-identity morphisms at every state are not enumerated
/// here; they are universally present per §6.1.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TransitionInfo {
    /// Transition name (as written after `#transition`).
    pub name: String,
    /// Index into the automaton's `transitions` list in the AST. Stable
    /// for the lifetime of the AST.
    pub transition_index: usize,
    /// Optional destination state (the explicit `-> Target`). `None`
    /// means the transition stays in the same state — used for monoid
    /// automata and same-state transitions in multi-state automata.
    pub destination: Option<String>,
    /// Source span of the entire transition declaration.
    pub span: Span,
}

/// The collection of all per-automaton categories produced by [`extract_categories`].
///
/// Indexed by automaton name. The map is built fresh on each call; consumers
/// may reuse it across passes provided the AST has not changed.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Categories {
    map: HashMap<String, AutomatonCategory>,
}

impl Categories {
    /// Look up the category for an automaton by name.
    #[must_use]
    pub fn lookup(&self, name: &str) -> Option<&AutomatonCategory> {
        self.map.get(name)
    }

    /// Iterate over every (name, category) pair. Order is unspecified.
    pub fn all(&self) -> impl Iterator<Item = (&String, &AutomatonCategory)> {
        self.map.iter()
    }

    /// Number of automaton categories recorded.
    #[must_use]
    pub fn len(&self) -> usize {
        self.map.len()
    }

    /// True if no categories were recorded.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.map.is_empty()
    }
}

/// Extract category `C_A` for every `#automaton A` in the program.
///
/// Per §6.1:
///
/// 1. Each automaton becomes a small category whose objects are its
///    `#states` (or the synthetic `[Ready]` for monoid automata).
/// 2. Each `#transition` becomes a morphism. Implicit identity morphisms
///    at every state are not enumerated (they are universally present).
/// 3. Validates that every `#transition T -> Target`'s `Target` is in the
///    automaton's `#states` (`E0430`). Monoid automata reject any
///    transition with an explicit destination (`E0431`).
/// 4. Validates that no two states or two transitions in the same
///    automaton share a name (`E0433`, `E0432`).
///
/// Reachability, stuck-state, and deadlock-SCC analyses (warnings W6101–W6103)
/// land in a future slice.
///
/// # Errors
///
/// Returns `Err(Vec<EffectError>)` when any validation fails. The error
/// vector is non-empty and ordered by source position. On success, returns
/// `Ok(Categories)` with one entry per `#automaton` declaration.
///
/// # Examples
///
/// ```
/// use clifford_lexer::tokenize;
/// use clifford_parser::parse;
/// use clifford_effect::extract_categories;
///
/// let src = "#automaton Counter {\n  \
///              #states: [Idle, Counting];\n  \
///              value: u32;\n  \
///              #transition start -> Counting { Counter.value = 0u32; }\n  \
///              #transition tick { Counter.value = Counter.value + 1u32; }\n\
///            }";
/// let tokens = tokenize(src).unwrap();
/// let program = parse(&tokens).unwrap();
/// let cats = extract_categories(&program).unwrap();
///
/// let counter = cats.lookup("Counter").unwrap();
/// assert!(!counter.is_monoid);
/// assert_eq!(counter.states.len(), 2);
/// assert_eq!(counter.transitions.len(), 2);
/// ```
pub fn extract_categories(program: &Program) -> Result<Categories, Vec<EffectError>> {
    let mut map: HashMap<String, AutomatonCategory> = HashMap::new();
    let mut errors: Vec<EffectError> = Vec::new();

    for (index, item) in program.items.iter().enumerate() {
        if let Item::Automaton(decl) = item {
            let cat = build_category(decl, index, &mut errors);
            map.insert(decl.name.clone(), cat);
        }
    }

    if errors.is_empty() {
        Ok(Categories { map })
    } else {
        Err(errors)
    }
}

/// Build the category for one automaton, accumulating any errors into the
/// shared error vector.
fn build_category(
    decl: &AutomatonDecl,
    item_index: usize,
    errors: &mut Vec<EffectError>,
) -> AutomatonCategory {
    let (states, is_monoid) = build_states(decl, errors);
    let transitions = build_transitions(decl, &states, is_monoid, errors);
    AutomatonCategory {
        name: decl.name.clone(),
        item_index,
        is_monoid,
        states,
        transitions,
        initial: 0,
    }
}

/// Build the state list, with duplicate-name detection.
///
/// For monoid automata (no `#states` clause), returns the synthetic
/// `[Ready]` state with `is_monoid = true`. For multi-state automata,
/// preserves source order; duplicates are reported but only the first
/// occurrence appears in the returned list (matching the resolver's
/// "first-wins" convention).
fn build_states(
    decl: &AutomatonDecl,
    errors: &mut Vec<EffectError>,
) -> (Vec<StateInfo>, bool) {
    match &decl.states {
        None => (
            vec![StateInfo {
                name: "Ready".to_owned(),
                span: decl.span,
            }],
            true,
        ),
        Some(states) => {
            let mut out: Vec<StateInfo> = Vec::with_capacity(states.len());
            let mut seen: HashMap<String, Span> = HashMap::new();
            for s in states {
                if let Some(original_span) = seen.get(&s.name) {
                    errors.push(EffectError::DuplicateState {
                        automaton: decl.name.clone(),
                        state: s.name.clone(),
                        original_at: original_span.start,
                        duplicate_at: s.span.start,
                    });
                } else {
                    seen.insert(s.name.clone(), s.span);
                    out.push(StateInfo {
                        name: s.name.clone(),
                        span: s.span,
                    });
                }
            }
            (out, false)
        }
    }
}

/// Build the transition list, validating destinations and detecting duplicates.
fn build_transitions(
    decl: &AutomatonDecl,
    states: &[StateInfo],
    is_monoid: bool,
    errors: &mut Vec<EffectError>,
) -> Vec<TransitionInfo> {
    let state_set: HashSet<&str> = states.iter().map(|s| s.name.as_str()).collect();
    let mut out: Vec<TransitionInfo> = Vec::with_capacity(decl.transitions.len());
    let mut seen_names: HashMap<String, Span> = HashMap::new();

    for (idx, t) in decl.transitions.iter().enumerate() {
        // Duplicate-name check. First-wins: only push the first; emit error
        // for subsequent ones.
        if let Some(original_span) = seen_names.get(&t.name) {
            errors.push(EffectError::DuplicateTransition {
                automaton: decl.name.clone(),
                transition: t.name.clone(),
                original_at: original_span.start,
                duplicate_at: t.span.start,
            });
            continue;
        }
        seen_names.insert(t.name.clone(), t.span);

        validate_transition_destination(decl, t, &state_set, is_monoid, errors);

        out.push(TransitionInfo {
            name: t.name.clone(),
            transition_index: idx,
            destination: t.destination.clone(),
            span: t.span,
        });
    }

    out
}

/// Per-transition destination validation.
///
/// - Monoid automata: `-> Target` clauses are forbidden (`E0431`); the only
///   morphism direction is the implicit `Ready -> Ready`.
/// - Multi-state automata: every `-> Target` must reference a declared state
///   (`E0430`). A transition without a destination clause is permitted —
///   it represents a same-state morphism.
fn validate_transition_destination(
    decl: &AutomatonDecl,
    t: &TransitionDecl,
    state_set: &HashSet<&str>,
    is_monoid: bool,
    errors: &mut Vec<EffectError>,
) {
    let Some(dest) = &t.destination else {
        return; // No `-> Target` clause; nothing to validate.
    };

    if is_monoid {
        errors.push(EffectError::MonoidTransitionWithDestination {
            automaton: decl.name.clone(),
            transition: t.name.clone(),
            target: dest.clone(),
            at: t.span.start,
        });
        return;
    }

    if !state_set.contains(dest.as_str()) {
        errors.push(EffectError::UnknownState {
            automaton: decl.name.clone(),
            transition: t.name.clone(),
            target: dest.clone(),
            at: t.span.start,
        });
    }
}

// ─── Slice 2: mutation profile extraction (§6.2) ────────────────────────────

/// Identifier of a callable thing in the program.
///
/// Effects and interrupts are top-level (named uniquely in the symbol
/// table). Transitions live inside automata; their identity needs both
/// the enclosing automaton's name and the transition name to be unique.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum CallableId {
    /// A top-level `#effect`.
    Effect(String),
    /// A top-level `#interrupt`.
    Interrupt(String),
    /// A `#transition` inside an `#automaton`.
    Transition {
        /// The enclosing automaton's name.
        automaton: String,
        /// The transition's name.
        name: String,
    },
}

impl CallableId {
    /// Display name for diagnostics — the callable's identifier without
    /// the kind prefix.
    #[must_use]
    pub fn name(&self) -> &str {
        match self {
            Self::Effect(n) | Self::Interrupt(n) => n.as_str(),
            Self::Transition { name, .. } => name.as_str(),
        }
    }
}

/// One `(automaton, field)` write reference. The unit of the per-effect
/// mutation profile and the input to the §7 GA orthogonality engine's
/// behavior-multivector construction.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct FieldRef {
    /// The automaton owning the field.
    pub automaton: String,
    /// The field name.
    pub field: String,
}

/// The mutation profile of one callable.
///
/// `actual_writes` is the set of `(automaton, field)` pairs the callable
/// writes — including writes propagated transitively through `#> proc()`
/// calls per Decision #3. This is what `crates/ortho` consumes for
/// write-write race detection.
///
/// `actual_reads` (v0.2-β) is the set of `(automaton, field)` pairs
/// the callable reads from in expression positions. Like writes, it
/// includes transitive reads propagated through proc-calls. Spec §7.2's
/// graded read-write algebra uses this to detect read-write races at
/// field granularity.
///
/// `actual_automata` is the set of automaton names appearing in
/// `actual_writes`; pre-computed for the §6.2 subset / disjointness
/// checks against the declared `#mutates` and `#cannot_mutate` clauses.
/// Read-only access does NOT contribute to this set — the §6.2 check
/// gates writes only.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct MutationProfile {
    /// Every `(automaton, field)` pair the callable writes (direct +
    /// transitive through proc-calls).
    pub actual_writes: HashSet<FieldRef>,
    /// Every `(automaton, field)` pair the callable reads (direct +
    /// transitive). Added in v0.2-β for spec §7.2's read-write race
    /// detection. Does NOT include the write-implies-read overlap
    /// for compound assignments — those are recorded explicitly.
    pub actual_reads: HashSet<FieldRef>,
    /// Every automaton name appearing in `actual_writes`. Derived
    /// upstream and cached here for fast subset / disjointness checks.
    /// Reads do not contribute (§6.2 gates writes only).
    pub actual_automata: HashSet<String>,
}

/// The collection of all per-callable mutation profiles produced by
/// [`extract_mutation_profiles`].
///
/// Indexed by [`CallableId`]. Effects, interrupts, and transitions all
/// appear in the same map.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct MutationProfiles {
    profiles: HashMap<CallableId, MutationProfile>,
}

impl MutationProfiles {
    /// Look up the mutation profile for a callable.
    #[must_use]
    pub fn lookup(&self, id: &CallableId) -> Option<&MutationProfile> {
        self.profiles.get(id)
    }

    /// Iterate over every (id, profile) pair. Order is unspecified.
    pub fn all(&self) -> impl Iterator<Item = (&CallableId, &MutationProfile)> {
        self.profiles.iter()
    }

    /// Number of callables recorded.
    #[must_use]
    pub fn len(&self) -> usize {
        self.profiles.len()
    }

    /// True if no callables were recorded.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.profiles.is_empty()
    }
}

/// Extract the mutation profile of every callable in the program.
///
/// Per §6.2:
///
/// 1. For each `#effect`, `#interrupt`, and `#transition`: walk the body
///    and collect every direct `(automaton, field)` write. Direct writes
///    come from `#mutate Auto { field = expr }` and `Auto.field <op>= expr`
///    statements.
/// 2. For each `#> proc()` call site: union the callee's *transitive*
///    mutation profile into the caller's profile. The transitive closure
///    is computed via fixed-point iteration (worklist).
/// 3. For each `#effect` and `#interrupt`: validate that
///    `actual_automata ⊆ declared_mutates` (`E0410`) and
///    `actual_automata ∩ declared_cannot_mutate = ∅` (`E0411`).
///    Transitions are not validated here (they implicitly mutate their
///    enclosing automaton; cross-automaton transition writes are caught
///    elsewhere).
///
/// Cycles in the proc-call graph are detected defensively (the worklist
/// terminates) but not yet rejected as `E0422` — that's slice E3 work
/// alongside full proc-call resolution and CallContext propagation.
///
/// # Errors
///
/// Returns `Err(Vec<EffectError>)` when any subset / disjointness check
/// fails. The error vector is non-empty and ordered by source position.
/// On success, returns `Ok(MutationProfiles)` with one entry per
/// callable in the program.
pub fn extract_mutation_profiles(
    program: &Program,
    resolution: &Resolution,
) -> Result<MutationProfiles, Vec<EffectError>> {
    // Phase 1: collect direct writes + direct proc-calls per callable.
    let direct = collect_direct_profiles(program, resolution);

    // Phase 2: compute transitive closure via worklist.
    let profiles = transitively_close(&direct);

    // Phase 3: validate against declared #mutates / #cannot_mutate.
    let mut errors: Vec<EffectError> = Vec::new();
    for item in &program.items {
        match item {
            Item::Effect(decl) => {
                let id = CallableId::Effect(decl.name.clone());
                if let Some(profile) = profiles.get(&id) {
                    validate_declared_mutates(
                        &decl.name,
                        decl.span,
                        &decl.mutates,
                        &decl.cannot_mutate,
                        profile,
                        &mut errors,
                    );
                }
            }
            Item::Interrupt(decl) => {
                let id = CallableId::Interrupt(decl.name.clone());
                if let Some(profile) = profiles.get(&id) {
                    validate_declared_mutates(
                        &decl.name,
                        decl.span,
                        &decl.mutates,
                        &[],
                        profile,
                        &mut errors,
                    );
                }
            }
            _ => {}
        }
    }

    if errors.is_empty() {
        Ok(MutationProfiles { profiles })
    } else {
        Err(errors)
    }
}

/// Internal: per-callable record of direct writes, direct reads,
/// and direct proc-call targets. The transitive-closure pass runs
/// over this to produce the final [`MutationProfile`]s.
///
/// Reads (added in v0.2-β for spec §7.2's graded read-write
/// algebra) are collected by walking expression positions for
/// `Auto.field` references — these include:
/// - Field accesses in `let` / `let mut` initialisers.
/// - Field accesses in `return expr;`.
/// - Field accesses in conditions (`if expr`, sigma `range`).
/// - Field accesses on the RHS of `Auto.field <op>= expr` (the
///   LHS is added to writes; the RHS is walked for nested reads).
/// - Field accesses on the LHS of compound `Auto.field <op>= expr`
///   when `op != Eq` (load-modify-store implies a read).
/// - Field accesses on the RHS of `local = expr;`.
/// - Field accesses passed as arguments to `#> proc()` calls.
#[derive(Debug, Clone, Default)]
struct DirectProfile {
    writes: HashSet<FieldRef>,
    reads: HashSet<FieldRef>,
    proc_calls: HashSet<CallableId>,
}

fn collect_direct_profiles(
    program: &Program,
    resolution: &Resolution,
) -> HashMap<CallableId, DirectProfile> {
    // Slice 19: build the automaton → field-names map once so the
    // `Flush` arm in `walk_stmts` can expand `#flush A;` into one
    // synthetic write per declared field of `A` (see
    // [`flush_writes_all_fields`] for why).
    let automaton_fields = build_automaton_fields(program);

    let mut map: HashMap<CallableId, DirectProfile> = HashMap::new();

    for item in &program.items {
        match item {
            Item::Effect(decl) => {
                let id = CallableId::Effect(decl.name.clone());
                let profile =
                    walk_body_for_direct(&decl.body, resolution, &automaton_fields);
                map.insert(id, profile);
            }
            Item::Interrupt(decl) => {
                let id = CallableId::Interrupt(decl.name.clone());
                let profile =
                    walk_body_for_direct(&decl.body, resolution, &automaton_fields);
                map.insert(id, profile);
            }
            Item::Automaton(decl) => {
                for trans in &decl.transitions {
                    let id = CallableId::Transition {
                        automaton: decl.name.clone(),
                        name: trans.name.clone(),
                    };
                    let profile = walk_transition_for_direct(
                        decl,
                        trans,
                        resolution,
                        &automaton_fields,
                    );
                    map.insert(id, profile);
                }
            }
            _ => {}
        }
    }

    map
}

/// Slice 19: index every `#automaton`'s declared field names so the
/// `#flush A;` profile expansion can record one write per field of
/// `A` (see [`walk_stmts`]'s `Flush` arm). Returns a map from
/// automaton name to its declared field names in source order.
///
/// **Why expand:** semantically `#flush A;` commits the entire
/// shadow struct into live state — it is a write to *every* field
/// of `A`. Recording one synthetic field would (a) under-report
/// the write set for the orthogonality engine (a flush + a field
/// write to `A` would not appear to race) and (b) introduce a
/// spurious sentinel field name into the profile API. Iterating
/// the actual field set keeps both downstream consumers
/// (orthogonality + diagnostics) consistent with the live-state
/// post-condition of a flush.
fn build_automaton_fields(program: &Program) -> HashMap<String, Vec<String>> {
    let mut map: HashMap<String, Vec<String>> = HashMap::new();
    for item in &program.items {
        if let Item::Automaton(decl) = item {
            map.insert(
                decl.name.clone(),
                decl.fields.iter().map(|f| f.name.clone()).collect(),
            );
        }
    }
    map
}

fn walk_body_for_direct(
    body: &Block,
    resolution: &Resolution,
    automaton_fields: &HashMap<String, Vec<String>>,
) -> DirectProfile {
    let mut profile = DirectProfile::default();
    // Top-level `#effect` / `#interrupt` bodies have no `Self` —
    // pass `None` for the enclosing owner. Any `Self.field`
    // appearing here is invalid (resolver already emitted an error),
    // and we silently skip it during read collection.
    walk_stmts(&body.stmts, resolution, None, automaton_fields, &mut profile);
    profile
}

fn walk_transition_for_direct(
    automaton: &AutomatonDecl,
    trans: &TransitionDecl,
    resolution: &Resolution,
    automaton_fields: &HashMap<String, Vec<String>>,
) -> DirectProfile {
    let mut profile = DirectProfile::default();
    // Inside a `#transition` body, `Self.field` resolves to the
    // owning automaton's field. Pass the owner so the read walker
    // can attribute `Self.field` reads correctly.
    walk_stmts(
        &trans.body.stmts,
        resolution,
        Some(automaton.name.as_str()),
        automaton_fields,
        &mut profile,
    );
    profile
}

fn walk_stmts(
    stmts: &[Stmt],
    resolution: &Resolution,
    enclosing_owner: Option<&str>,
    automaton_fields: &HashMap<String, Vec<String>>,
    out: &mut DirectProfile,
) {
    for stmt in stmts {
        match &stmt.kind {
            StmtKind::Mutate { automaton, assigns } => {
                for FieldAssign { field, index, value, .. } in assigns {
                    out.writes.insert(FieldRef {
                        automaton: automaton.clone(),
                        field: field.clone(),
                    });
                    // Walk the index (if any) and value for nested
                    // automaton-field reads.
                    if let Some(idx_expr) = index {
                        walk_expr_for_reads(idx_expr, resolution, enclosing_owner, out);
                    }
                    walk_expr_for_reads(value, resolution, enclosing_owner, out);
                }
            }
            StmtKind::MutateShort {
                automaton,
                field,
                op,
                value,
                ..
            } => {
                out.writes.insert(FieldRef {
                    automaton: automaton.clone(),
                    field: field.clone(),
                });
                // v0.2-β: compound assignment (`+=`, `*=`, etc.)
                // lowers to load-modify-store, so the LHS is also
                // read. Plain `=` is a pure store, no read.
                if !matches!(op, AssignOp::Eq) {
                    out.reads.insert(FieldRef {
                        automaton: automaton.clone(),
                        field: field.clone(),
                    });
                }
                walk_expr_for_reads(value, resolution, enclosing_owner, out);
            }
            StmtKind::ProcCall { name, args } => {
                // The resolver tagged the call site with a Proc binding
                // carrying CallContext. Resolve to the right CallableId.
                if let Some(BindingRef::Proc { name: pn, ctx, .. }) =
                    resolution.lookup(stmt.span)
                {
                    let id = match ctx {
                        CallContext::Identity => CallableId::Effect(pn.clone()),
                        CallContext::Transition => {
                            // For Transition-context calls, we don't yet
                            // know the enclosing automaton from the
                            // binding alone; the transitive-closure pass
                            // resolves this by trying every automaton.
                            // For now, we record the name without
                            // automaton qualification and let the
                            // closure pass disambiguate.
                            //
                            // A cleaner future approach: enrich
                            // BindingRef::Proc with the resolved
                            // automaton name (Resolver R3 has the info;
                            // it's just not exposed yet).
                            CallableId::Transition {
                                automaton: String::new(),
                                name: name.clone(),
                            }
                        }
                    };
                    out.proc_calls.insert(id);
                } else {
                    // Unresolved proc-call. The resolver would have
                    // emitted E0404; we silently ignore here so we don't
                    // double-report.
                }
                // v0.2-β: walk arguments for field reads.
                for a in args {
                    walk_expr_for_reads(a, resolution, enclosing_owner, out);
                }
            }
            // v0.2-β: walk every expression-bearing statement so
            // automaton-field reads in any position are captured.
            StmtKind::Let { value, .. } | StmtKind::LetShort { value, .. } => {
                walk_expr_for_reads(value, resolution, enclosing_owner, out);
            }
            StmtKind::Expr(e) | StmtKind::Return(Some(e)) => {
                walk_expr_for_reads(e, resolution, enclosing_owner, out);
            }
            StmtKind::Return(None) => {}
            StmtKind::UncheckedStore { ptr, value, .. }
            | StmtKind::VolatileStore { ptr, value, .. } => {
                walk_expr_for_reads(ptr, resolution, enclosing_owner, out);
                walk_expr_for_reads(value, resolution, enclosing_owner, out);
            }
            StmtKind::Assign { value, .. } => {
                // The LHS is a local, not an automaton field. The RHS
                // is the only expression that can contain field reads.
                walk_expr_for_reads(value, resolution, enclosing_owner, out);
            }
            StmtKind::If {
                cond,
                then_block,
                else_block,
            } => {
                walk_expr_for_reads(cond, resolution, enclosing_owner, out);
                walk_stmts(
                    &then_block.stmts,
                    resolution,
                    enclosing_owner,
                    automaton_fields,
                    out,
                );
                if let Some(blk) = else_block {
                    walk_stmts(
                        &blk.stmts,
                        resolution,
                        enclosing_owner,
                        automaton_fields,
                        out,
                    );
                }
            }
            StmtKind::Sigma { source, body, .. } => {
                walk_expr_for_reads(source, resolution, enclosing_owner, out);
                walk_stmts(
                    &body.stmts,
                    resolution,
                    enclosing_owner,
                    automaton_fields,
                    out,
                );
            }
            // Slice 19 (Decision #12 follow-up): `#flush A;`
            // commits the shadow struct of `A` into live state in
            // one memcpy — semantically a write to *every* field
            // of `A`. Record one direct write per declared field
            // so:
            //   - the §6.2 mutation-profile check fires
            //     **E0410 EffectMutatesUndeclaredAutomaton** when
            //     the enclosing callable's `#mutates: [...]` list
            //     omits `A`, and
            //   - the §7 orthogonality engine sees a flush race
            //     against any other writer of any field of `A`,
            //     not just other flushes.
            // If `A` doesn't resolve to any automaton (the
            // resolver already emitted E0413), the lookup misses
            // and we record nothing — the resolver-side error is
            // the user's signal.
            StmtKind::Flush { automaton } => {
                if let Some(field_names) = automaton_fields.get(automaton) {
                    for field in field_names {
                        out.writes.insert(FieldRef {
                            automaton: automaton.clone(),
                            field: field.clone(),
                        });
                    }
                }
            }
            // Forward-compat for new statement kinds.
            _ => {}
        }
    }
}

/// v0.2-β: recursively walk an expression tree and record every
/// `Auto.field` (or `Self.field` inside a transition) as a read in
/// the profile. Other expression shapes recurse into their
/// children but contribute no reads themselves.
///
/// Resolution: the receiver of a `FieldAccess` must be a single-
/// segment `Path` whose first segment either:
/// - is a single ident `X` where `X` resolves to an automaton in
///   the symbol table, OR
/// - is the literal `"Self"` and `enclosing_owner` is `Some(_)`
///   (we're inside a `#transition` body of that owner).
///
/// Field-of-local accesses (e.g. `x.foo` where `x` is a let-bound
/// tuple) don't constitute automaton reads; we recurse into `obj`
/// regardless to catch nested reads inside.
fn walk_expr_for_reads(
    expr: &Expr,
    resolution: &Resolution,
    enclosing_owner: Option<&str>,
    out: &mut DirectProfile,
) {
    match &expr.kind {
        ExprKind::FieldAccess { obj, field } => {
            // Try to resolve the receiver as an automaton name.
            if let ExprKind::Path(segs) = &obj.kind {
                if segs.len() == 1 {
                    let head = segs[0].as_str();
                    let resolved_owner = if head == "Self" {
                        enclosing_owner
                    } else if resolution
                        .symbols
                        .lookup(head)
                        .is_some_and(|s| matches!(s.kind, SymbolKind::Automaton))
                    {
                        Some(head)
                    } else {
                        None
                    };
                    if let Some(owner) = resolved_owner {
                        out.reads.insert(FieldRef {
                            automaton: owner.to_owned(),
                            field: field.clone(),
                        });
                    }
                }
            }
            // Always recurse into the receiver to catch nested
            // reads (e.g. `Auto.field.subfield` in a future
            // tuple-field slice; harmless today).
            walk_expr_for_reads(obj, resolution, enclosing_owner, out);
        }
        ExprKind::Index { obj, index } => {
            walk_expr_for_reads(obj, resolution, enclosing_owner, out);
            walk_expr_for_reads(index, resolution, enclosing_owner, out);
        }
        ExprKind::Binary { lhs, rhs, .. } => {
            walk_expr_for_reads(lhs, resolution, enclosing_owner, out);
            walk_expr_for_reads(rhs, resolution, enclosing_owner, out);
        }
        ExprKind::Unary { operand, .. } | ExprKind::Ref { operand, .. } => {
            walk_expr_for_reads(operand, resolution, enclosing_owner, out);
        }
        ExprKind::Paren(inner) => {
            walk_expr_for_reads(inner, resolution, enclosing_owner, out);
        }
        ExprKind::Cast { value, .. } => {
            walk_expr_for_reads(value, resolution, enclosing_owner, out);
        }
        ExprKind::Call { callee, args } => {
            walk_expr_for_reads(callee, resolution, enclosing_owner, out);
            for a in args {
                walk_expr_for_reads(a, resolution, enclosing_owner, out);
            }
        }
        ExprKind::MethodCall { obj, args, .. } => {
            walk_expr_for_reads(obj, resolution, enclosing_owner, out);
            for a in args {
                walk_expr_for_reads(a, resolution, enclosing_owner, out);
            }
        }
        ExprKind::Range { lo, hi, .. } => {
            walk_expr_for_reads(lo, resolution, enclosing_owner, out);
            walk_expr_for_reads(hi, resolution, enclosing_owner, out);
        }
        ExprKind::Tuple(elems) | ExprKind::Array(elems) => {
            for e in elems {
                walk_expr_for_reads(e, resolution, enclosing_owner, out);
            }
        }
        ExprKind::ArrayRepeat { value, count } => {
            walk_expr_for_reads(value, resolution, enclosing_owner, out);
            walk_expr_for_reads(count, resolution, enclosing_owner, out);
        }
        ExprKind::UncheckedLoad { ptr, .. } | ExprKind::VolatileLoad { ptr, .. } => {
            walk_expr_for_reads(ptr, resolution, enclosing_owner, out);
        }
        ExprKind::UncheckedCast { value, .. } => {
            walk_expr_for_reads(value, resolution, enclosing_owner, out);
        }
        ExprKind::UncheckedOffset { ptr, n, .. } => {
            walk_expr_for_reads(ptr, resolution, enclosing_owner, out);
            walk_expr_for_reads(n, resolution, enclosing_owner, out);
        }
        // `Snapshot { automaton, field }` is `@snapshot Auto.field`
        // per Decision #24 / ADR 0004.
        //
        // **v0.2-ζ change**: snapshot reads are NOT recorded in
        // `actual_reads`. The snapshot-and-decide pattern (spec
        // §7.2 closing note 3) makes the read race-free at the
        // hardware level — for primitive single-word fields on
        // aligned 32-bit targets, the load is a single
        // instruction; the racer either reads the old or the new
        // value, never torn. Codegen restricts `@snapshot` to
        // primitive fields (compound types would tear and surface
        // a structured error), so by the time a snapshot reaches
        // the verifier we know the read is atomic at the
        // instruction level.
        //
        // Excluding snapshots from `actual_reads` is the missing
        // half of the §7.2 graded check: writers `actual_writes`
        // still get the full race check against ordinary reads,
        // but the user's `@snapshot` annotation tells the engine
        // "I've taken responsibility for the load atomicity at
        // this site."
        //
        // The receiver expression and field name aren't walked
        // recursively because `automaton`/`field` are bare
        // identifiers in the AST, not nested expressions.
        ExprKind::Snapshot { .. } => {}
        // Slice 24: `@shadow Auto.field` — pending shadow
        // read of a `#staged` automaton. Like `@snapshot`,
        // the user has explicitly opted into a race-free read
        // boundary (the shadow is a private buffer until
        // `#flush`); we don't record it in the
        // race-detectable read set. The resolver enforces the
        // staged-only invariant (E0414).
        ExprKind::Shadow { .. } => {}
        // `Auto@state` reads the state-tag — for v0.2-β we treat
        // this as a read of an implicit "tag" pseudo-field of the
        // automaton. Today the tag isn't a named field in the
        // basis (slice-9 prepends it as LLVM struct slot 0
        // without a source-level name), so we skip recording it.
        // A future slice could add `__state_tag` to the basis if
        // race-on-tag-read becomes a concern.
        ExprKind::StateRead(_) => {}
        // Atomic literals + path expressions can't contain nested
        // reads.
        ExprKind::IntLit(_)
        | ExprKind::HexLit(_)
        | ExprKind::BinLit(_)
        | ExprKind::FloatLit(_)
        | ExprKind::CharLit(_)
        | ExprKind::ByteLit(_)
        | ExprKind::StringLit(_)
        | ExprKind::BoolLit(_)
        | ExprKind::Null
        | ExprKind::Path(_) => {}
        // Forward-compat for new expression kinds.
        _ => {}
    }
}

/// Compute the transitive closure of writes AND reads through the
/// proc-call graph.
///
/// Worklist algorithm: starts each callable with its direct writes
/// and reads, repeatedly unions in callees' writes and reads until
/// no profile changes. Terminates because the union over a finite
/// field-set is monotonic and bounded.
///
/// Cycles in the proc-call graph (e.g. effect A calls B calls A)
/// are handled defensively: each iteration only unions in
/// already-known writes/reads, so the fixed point is reached
/// without infinite recursion.
///
/// Reads were added in v0.2-β for spec §7.2's graded read-write
/// algebra. Their propagation is symmetric to writes — if A calls
/// B, then everything B reads is reachable from A.
fn transitively_close(
    direct: &HashMap<CallableId, DirectProfile>,
) -> HashMap<CallableId, MutationProfile> {
    // Initialise: each callable starts with its direct writes + reads.
    let mut profiles: HashMap<CallableId, MutationProfile> = direct
        .iter()
        .map(|(id, dp)| {
            let actual_automata: HashSet<String> =
                dp.writes.iter().map(|fr| fr.automaton.clone()).collect();
            (
                id.clone(),
                MutationProfile {
                    actual_writes: dp.writes.clone(),
                    actual_reads: dp.reads.clone(),
                    actual_automata,
                },
            )
        })
        .collect();

    // Build a name-to-callable index for Transition resolution. Per the
    // walker, Transition callees are recorded with `automaton: ""`. Map
    // each transition name to the set of fully-qualified CallableIds it
    // could refer to (typically one; ambiguity is rare and would be
    // E0422 territory).
    let trans_index: HashMap<String, Vec<CallableId>> = {
        let mut m: HashMap<String, Vec<CallableId>> = HashMap::new();
        for id in direct.keys() {
            if let CallableId::Transition { name, .. } = id {
                m.entry(name.clone()).or_default().push(id.clone());
            }
        }
        m
    };

    // Fixed-point iteration. Bounded by total number of
    // (callable, field) pairs in the program; reads propagate
    // symmetrically with writes.
    loop {
        let mut changed = false;
        for (caller_id, dp) in direct {
            let mut write_additions: HashSet<FieldRef> = HashSet::new();
            let mut read_additions: HashSet<FieldRef> = HashSet::new();
            for callee_id in &dp.proc_calls {
                let resolved_callees: Vec<CallableId> = match callee_id {
                    CallableId::Effect(_) | CallableId::Interrupt(_) => {
                        vec![callee_id.clone()]
                    }
                    CallableId::Transition { automaton, name } if automaton.is_empty() => {
                        // Resolve via the trans_index.
                        trans_index.get(name).cloned().unwrap_or_default()
                    }
                    CallableId::Transition { .. } => vec![callee_id.clone()],
                };
                for resolved in resolved_callees {
                    if let Some(callee_profile) = profiles.get(&resolved) {
                        let caller_writes = profiles
                            .get(caller_id)
                            .map(|p| &p.actual_writes);
                        let caller_reads = profiles
                            .get(caller_id)
                            .map(|p| &p.actual_reads);
                        for fr in &callee_profile.actual_writes {
                            if !caller_writes.is_some_and(|w| w.contains(fr)) {
                                write_additions.insert(fr.clone());
                            }
                        }
                        for fr in &callee_profile.actual_reads {
                            if !caller_reads.is_some_and(|r| r.contains(fr)) {
                                read_additions.insert(fr.clone());
                            }
                        }
                    }
                }
            }
            if !write_additions.is_empty() || !read_additions.is_empty() {
                if let Some(p) = profiles.get_mut(caller_id) {
                    for fr in write_additions {
                        p.actual_automata.insert(fr.automaton.clone());
                        p.actual_writes.insert(fr);
                    }
                    for fr in read_additions {
                        // Note: reads do NOT contribute to actual_automata
                        // (§6.2 #mutates check gates writes only).
                        p.actual_reads.insert(fr);
                    }
                    changed = true;
                }
            }
        }
        if !changed {
            break;
        }
    }

    profiles
}

fn validate_declared_mutates(
    callable_name: &str,
    callable_span: Span,
    declared_mutates: &[String],
    declared_cannot_mutate: &[String],
    profile: &MutationProfile,
    errors: &mut Vec<EffectError>,
) {
    let declared_set: HashSet<&str> = declared_mutates.iter().map(String::as_str).collect();
    let excluded_set: HashSet<&str> =
        declared_cannot_mutate.iter().map(String::as_str).collect();

    // E0410: every actually-mutated automaton must be in declared_mutates.
    let mut sorted_actual: Vec<&str> =
        profile.actual_automata.iter().map(String::as_str).collect();
    sorted_actual.sort_unstable();
    for auto in sorted_actual {
        if !declared_set.contains(auto) {
            errors.push(EffectError::EffectMutatesUndeclaredAutomaton {
                callable: callable_name.to_owned(),
                automaton: auto.to_owned(),
                at: callable_span.start,
            });
        }
        if excluded_set.contains(auto) {
            errors.push(EffectError::EffectMutatesExcludedAutomaton {
                callable: callable_name.to_owned(),
                automaton: auto.to_owned(),
                at: callable_span.start,
            });
        }
    }
}

// ─── Slice 3: proc-call graph + cycle detection (§6.3) ──────────────────────

/// The proc-call graph as a first-class artifact.
///
/// Per spec §6.3: edges are `caller → callee` for every `#> name(args)` call
/// site, and the graph is rejected if it contains any cycles (E0422 unless
/// explicitly marked recursive — v0.1 has no recursion-marker syntax, so all
/// cycles are rejected).
///
/// This artifact is consumed by `crates/codegen` (for inlining decisions and
/// state-tag emission ordering per §6.4) and by `cliffordc audit` (for
/// dependency-graph rendering).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ProcCallGraph {
    /// Map: caller → set of callees. Only includes callables that actually
    /// make `#>` calls; callables with no outbound edges are absent (the
    /// caller can iterate `mutation_profiles` to enumerate every callable).
    edges: HashMap<CallableId, HashSet<CallableId>>,
}

impl ProcCallGraph {
    /// Look up the set of callees a given caller invokes via `#>`.
    /// Returns `None` if the caller has no outbound proc-calls.
    #[must_use]
    pub fn callees(&self, caller: &CallableId) -> Option<&HashSet<CallableId>> {
        self.edges.get(caller)
    }

    /// Iterate over every (caller, callees) pair. Order is unspecified.
    pub fn all(&self) -> impl Iterator<Item = (&CallableId, &HashSet<CallableId>)> {
        self.edges.iter()
    }

    /// Number of callers with at least one outbound edge.
    #[must_use]
    pub fn len(&self) -> usize {
        self.edges.len()
    }

    /// True if the graph has no edges.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.edges.is_empty()
    }
}

/// Extract the proc-call graph and reject any cycles.
///
/// Walks every callable, collecting `#> proc()` edges using the resolver's
/// `BindingRef::Proc` bindings (the same mechanism `extract_mutation_profiles`
/// uses for the transitive write closure). Then runs DFS-based cycle
/// detection; for each strongly-connected component of size > 1 (or a
/// self-loop), emits one `E0422 ProcCallCycle` listing every callable on
/// the cycle.
///
/// The graph itself is returned even on success so consumers can use it for
/// downstream analyses (codegen ordering, audit reports). On error, the
/// graph is *also* returned (in `Err`'s `Vec` length tells you how many
/// distinct cycles were found, but the graph state is still valid).
///
/// # Examples
///
/// ```
/// use clifford_lexer::tokenize;
/// use clifford_parser::parse;
/// use clifford_resolve::resolve;
/// use clifford_effect::extract_call_graph;
///
/// // No cycles → clean.
/// let src = "#effect a() #mutates: [] { } \
///            #effect b() #mutates: [] { #> a(); }";
/// let tokens = tokenize(src).unwrap();
/// let program = parse(&tokens).unwrap();
/// let resolution = resolve(&program).unwrap();
/// let graph = extract_call_graph(&program, &resolution).unwrap();
/// // `b` calls `a`; `a` has no callees.
/// assert!(graph.callees(&clifford_effect::CallableId::Effect("b".into())).is_some());
/// assert!(graph.callees(&clifford_effect::CallableId::Effect("a".into())).is_none());
/// ```
///
/// # Errors
///
/// Returns `Err(Vec<EffectError>)` containing one `ProcCallCycle` per
/// detected cycle. Multiple disjoint cycles produce multiple errors.
pub fn extract_call_graph(
    program: &Program,
    resolution: &Resolution,
) -> Result<ProcCallGraph, Vec<EffectError>> {
    let direct = collect_direct_profiles(program, resolution);
    let edges = build_edges(&direct);
    let graph = ProcCallGraph { edges };

    let cycles = find_cycles(&graph);
    if cycles.is_empty() {
        Ok(graph)
    } else {
        let errors: Vec<EffectError> = cycles
            .into_iter()
            .map(|cycle| {
                let names: Vec<String> = cycle.iter().map(callable_display).collect();
                let display = if names.len() == 1 {
                    // Self-loop: render as `a → a`.
                    format!("`{}` → `{}`", names[0], names[0])
                } else {
                    let mut path = names
                        .iter()
                        .map(|n| format!("`{n}`"))
                        .collect::<Vec<_>>()
                        .join(" → ");
                    path.push_str(&format!(" → `{}`", names[0]));
                    path
                };
                EffectError::ProcCallCycle {
                    cycle: names,
                    cycle_display: display,
                }
            })
            .collect();
        Err(errors)
    }
}

/// Build the call-graph edge map from the direct-profile collection.
///
/// Resolves Transition-context callees (which start with empty automaton
/// per the resolver placeholder) by matching the transition name against
/// the index of all transitions in the program.
fn build_edges(
    direct: &HashMap<CallableId, DirectProfile>,
) -> HashMap<CallableId, HashSet<CallableId>> {
    // Build the trans_index for resolving Transition callees with empty
    // automaton placeholder (same approach as transitively_close).
    let trans_index: HashMap<String, Vec<CallableId>> = {
        let mut m: HashMap<String, Vec<CallableId>> = HashMap::new();
        for id in direct.keys() {
            if let CallableId::Transition { name, .. } = id {
                m.entry(name.clone()).or_default().push(id.clone());
            }
        }
        m
    };

    let mut edges: HashMap<CallableId, HashSet<CallableId>> = HashMap::new();
    for (caller, dp) in direct {
        if dp.proc_calls.is_empty() {
            continue;
        }
        let mut resolved: HashSet<CallableId> = HashSet::new();
        for callee in &dp.proc_calls {
            match callee {
                CallableId::Effect(_) | CallableId::Interrupt(_) => {
                    // Only include the edge if the callee actually exists
                    // in the direct map (i.e. the program declares it).
                    if direct.contains_key(callee) {
                        resolved.insert(callee.clone());
                    }
                }
                CallableId::Transition { automaton, name } if automaton.is_empty() => {
                    if let Some(targets) = trans_index.get(name) {
                        for t in targets {
                            resolved.insert(t.clone());
                        }
                    }
                }
                CallableId::Transition { .. } => {
                    if direct.contains_key(callee) {
                        resolved.insert(callee.clone());
                    }
                }
            }
        }
        if !resolved.is_empty() {
            edges.insert(caller.clone(), resolved);
        }
    }
    edges
}

/// Find every cycle in the proc-call graph using DFS with three-color
/// marking (white = unvisited, gray = on current path, black = fully
/// processed).
///
/// Returns a list of cycles; each cycle is a `Vec<CallableId>` listing
/// the cycle members in traversal order. Self-loops are returned as a
/// single-element cycle. The traversal is deterministic via sorted
/// iteration over node names.
///
/// Algorithm: for each unvisited node, run DFS; when the search re-enters
/// a gray node, extract the cycle from the path stack. Each cycle is
/// emitted once (canonicalised by rotating to start at the
/// lexicographically-smallest member).
fn find_cycles(graph: &ProcCallGraph) -> Vec<Vec<CallableId>> {
    use std::collections::BTreeMap;

    // Sort callers for deterministic traversal order.
    let sorted: BTreeMap<String, &CallableId> = graph
        .edges
        .keys()
        .map(|id| (callable_sort_key(id), id))
        .collect();

    let mut color: HashMap<CallableId, u8> = HashMap::new(); // 0=white, 1=gray, 2=black
    let mut path: Vec<CallableId> = Vec::new();
    let mut cycles: Vec<Vec<CallableId>> = Vec::new();
    let mut seen_canonical: HashSet<Vec<CallableId>> = HashSet::new();

    fn dfs(
        node: &CallableId,
        graph: &ProcCallGraph,
        color: &mut HashMap<CallableId, u8>,
        path: &mut Vec<CallableId>,
        cycles: &mut Vec<Vec<CallableId>>,
        seen_canonical: &mut HashSet<Vec<CallableId>>,
    ) {
        color.insert(node.clone(), 1);
        path.push(node.clone());

        if let Some(callees) = graph.callees(node) {
            // Sort callees for determinism.
            let mut sorted_callees: Vec<&CallableId> = callees.iter().collect();
            sorted_callees.sort_by_key(|id| callable_sort_key(id));

            for callee in sorted_callees {
                match color.get(callee).copied().unwrap_or(0) {
                    1 => {
                        // Gray: cycle detected. Extract from path.
                        if let Some(start_idx) =
                            path.iter().position(|n| n == callee)
                        {
                            let cycle: Vec<CallableId> = path[start_idx..].to_vec();
                            let canonical = canonicalise_cycle(&cycle);
                            if seen_canonical.insert(canonical.clone()) {
                                cycles.push(canonical);
                            }
                        }
                    }
                    0 => {
                        dfs(callee, graph, color, path, cycles, seen_canonical);
                    }
                    _ => {} // black: already fully processed
                }
            }
        }

        path.pop();
        color.insert(node.clone(), 2);
    }

    for (_, node) in sorted {
        if color.get(node).copied().unwrap_or(0) == 0 {
            dfs(node, graph, &mut color, &mut path, &mut cycles, &mut seen_canonical);
        }
    }

    cycles
}

/// Human-readable rendering of a [`CallableId`] for diagnostics.
fn callable_display(id: &CallableId) -> String {
    match id {
        CallableId::Effect(n) => format!("#effect {n}"),
        CallableId::Interrupt(n) => format!("#interrupt {n}"),
        CallableId::Transition { automaton, name } => {
            format!("#transition {automaton}::{name}")
        }
    }
}

/// Stable string key for sorting callables.
fn callable_sort_key(id: &CallableId) -> String {
    match id {
        CallableId::Effect(n) => format!("E:{n}"),
        CallableId::Interrupt(n) => format!("I:{n}"),
        CallableId::Transition { automaton, name } => {
            format!("T:{automaton}::{name}")
        }
    }
}

/// Canonicalise a cycle by rotating it so the lexicographically-smallest
/// member is first. Ensures the same cycle isn't reported twice from
/// different DFS entry points.
fn canonicalise_cycle(cycle: &[CallableId]) -> Vec<CallableId> {
    if cycle.is_empty() {
        return Vec::new();
    }
    let min_idx = cycle
        .iter()
        .enumerate()
        .min_by_key(|(_, id)| callable_sort_key(id))
        .map(|(i, _)| i)
        .unwrap_or(0);
    let mut out: Vec<CallableId> = Vec::with_capacity(cycle.len());
    out.extend_from_slice(&cycle[min_idx..]);
    out.extend_from_slice(&cycle[..min_idx]);
    out
}

// ─── Slice 4: Refinement #5e interrupt-overlap set ──────────────────────────

/// For each `#automaton A`, the set of `#interrupt` declarations that mutate
/// `A` transitively.
///
/// Per Refinement #5e, this set drives `clifford-codegen`'s §8.4 decision
/// about whether each `#transition` of `A` needs critical-section wrapping:
///
/// - If `R(A)` is non-empty for an automaton `A`, every transition of `A`
///   that runs in user code must be wrapped in a critical section
///   (CLI/STI on Cortex-M; `csrrci sie` on RISC-V) so that an interrupt
///   doesn't preempt mid-transition and observe a torn state.
/// - If `R(A)` is empty (no interrupt touches `A`), no wrapping is needed
///   and the transition compiles to plain straight-line code.
///
/// The set is also useful for `cliffordc audit`'s "interrupts that affect
/// this automaton" report and for the `@sequential(A, B)` overrides per
/// Decision #11.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct InterruptOverlap {
    /// Map: automaton-name → set of interrupt names that mutate it
    /// (transitively per `#mutates` + proc-call closure).
    map: HashMap<String, HashSet<String>>,
}

impl InterruptOverlap {
    /// Look up R(A) for an automaton — the set of interrupt names that
    /// mutate `A` transitively. Returns an empty set (not `None`) when
    /// no interrupt touches `A`; consumers can use `is_empty()` to test.
    #[must_use]
    pub fn interrupts_for(&self, automaton: &str) -> &HashSet<String> {
        // Use a cached static empty set — saves an allocation in the
        // common "no interrupt touches this automaton" lookup path.
        static EMPTY: std::sync::OnceLock<HashSet<String>> = std::sync::OnceLock::new();
        self.map
            .get(automaton)
            .unwrap_or_else(|| EMPTY.get_or_init(HashSet::new))
    }

    /// True if any interrupt mutates this automaton transitively.
    /// Convenience for the common "do I need critical-section wrapping?"
    /// test the codegen does for each automaton's transitions.
    #[must_use]
    pub fn is_overlapped(&self, automaton: &str) -> bool {
        self.map
            .get(automaton)
            .map(|s| !s.is_empty())
            .unwrap_or(false)
    }

    /// Iterate over `(automaton, interrupts)` pairs where the automaton
    /// has at least one overlapping interrupt.
    pub fn all(&self) -> impl Iterator<Item = (&String, &HashSet<String>)> {
        self.map.iter()
    }

    /// Number of automatons with at least one overlapping interrupt.
    #[must_use]
    pub fn len(&self) -> usize {
        self.map.len()
    }

    /// True if no automaton has any overlapping interrupt.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.map.is_empty()
    }
}

/// Compute the interrupt-overlap set R(A) for every automaton in the program.
///
/// Algorithm:
///
/// 1. From the [`MutationProfiles`], extract the transitive
///    `actual_automata` set for every `#interrupt` callable.
/// 2. Invert the relation: for each automaton `A` named in any interrupt's
///    `actual_automata`, record the interrupt's name in `R(A)`.
/// 3. Return the resulting per-automaton set.
///
/// Because [`MutationProfiles`] already captures transitive closure
/// through `#> proc()` calls (per slice E2), this slice does not have to
/// re-walk the call graph — it just inverts the existing relation.
///
/// # Examples
///
/// ```
/// use clifford_lexer::tokenize;
/// use clifford_parser::parse;
/// use clifford_resolve::resolve;
/// use clifford_effect::{extract_mutation_profiles, compute_interrupt_overlap};
///
/// let src = "\
///   #automaton Counter { value: u32; }\n\
///   #interrupt UART_RX() #mutates: [Counter] #priority: HIGH {\n  \
///     Counter.value = 1u32;\n  \
///   }\n\
/// ";
/// let tokens = tokenize(src).unwrap();
/// let program = parse(&tokens).unwrap();
/// let resolution = resolve(&program).unwrap();
/// let profiles = extract_mutation_profiles(&program, &resolution).unwrap();
/// let overlap = compute_interrupt_overlap(&profiles);
///
/// assert!(overlap.is_overlapped("Counter"));
/// assert!(overlap.interrupts_for("Counter").contains("UART_RX"));
/// ```
#[must_use]
pub fn compute_interrupt_overlap(profiles: &MutationProfiles) -> InterruptOverlap {
    let mut map: HashMap<String, HashSet<String>> = HashMap::new();
    for (id, profile) in profiles.all() {
        let interrupt_name = match id {
            CallableId::Interrupt(n) => n.clone(),
            _ => continue,
        };
        for auto_name in &profile.actual_automata {
            map.entry(auto_name.clone())
                .or_default()
                .insert(interrupt_name.clone());
        }
    }
    InterruptOverlap { map }
}

#[cfg(test)]
mod tests {
    use super::*;
    use clifford_lexer::tokenize;
    use clifford_parser::parse;
    use clifford_resolve::resolve;

    fn extract_str(src: &str) -> Result<Categories, Vec<EffectError>> {
        let tokens = tokenize(src).expect("tokenize");
        let program = parse(&tokens).expect("parse");
        extract_categories(&program)
    }

    fn profiles_str(src: &str) -> Result<MutationProfiles, Vec<EffectError>> {
        let tokens = tokenize(src).expect("tokenize");
        let program = parse(&tokens).expect("parse");
        let resolution = resolve(&program).expect("resolve");
        extract_mutation_profiles(&program, &resolution)
    }

    // ── Empty / no-automatons ────────────────────────────────────────────

    #[test]
    fn empty_program_has_empty_categories() {
        let cats = extract_str("").unwrap();
        assert!(cats.is_empty());
        assert_eq!(cats.len(), 0);
    }

    #[test]
    fn program_with_no_automatons_has_empty_categories() {
        let cats = extract_str("@fn helper() { }").unwrap();
        assert!(cats.is_empty());
    }

    // ── Monoid automata (no #states clause) ──────────────────────────────

    #[test]
    fn monoid_automaton_gets_synthetic_ready_state() {
        let cats = extract_str("#automaton Counter { value: u32; }").unwrap();
        let cat = cats.lookup("Counter").unwrap();
        assert!(cat.is_monoid);
        assert_eq!(cat.states.len(), 1);
        assert_eq!(cat.states[0].name, "Ready");
        assert_eq!(cat.transitions.len(), 0);
        assert_eq!(cat.initial, 0);
    }

    #[test]
    fn monoid_automaton_with_destinationless_transition() {
        let cats = extract_str(
            "#automaton Counter { \
             value: u32; \
             #transition tick { Counter.value = Counter.value + 1u32; } \
             }",
        )
        .unwrap();
        let cat = cats.lookup("Counter").unwrap();
        assert!(cat.is_monoid);
        assert_eq!(cat.transitions.len(), 1);
        assert_eq!(cat.transitions[0].name, "tick");
        assert!(cat.transitions[0].destination.is_none());
    }

    #[test]
    fn monoid_automaton_with_destination_is_e0431() {
        let errors = extract_str(
            "#automaton Counter { \
             #transition oops -> Halted { } \
             }",
        )
        .unwrap_err();
        assert!(errors.iter().any(|e| matches!(
            e,
            EffectError::MonoidTransitionWithDestination {
                ref automaton,
                ref transition,
                ref target,
                ..
            } if automaton == "Counter" && transition == "oops" && target == "Halted"
        )));
    }

    // ── Multi-state automata ─────────────────────────────────────────────

    #[test]
    fn multistate_automaton_states_recorded() {
        let cats = extract_str(
            "#automaton Sm { \
             #states: [Idle, Running, Halted]; \
             }",
        )
        .unwrap();
        let cat = cats.lookup("Sm").unwrap();
        assert!(!cat.is_monoid);
        let names: Vec<_> = cat.states.iter().map(|s| s.name.as_str()).collect();
        assert_eq!(names, vec!["Idle", "Running", "Halted"]);
        assert_eq!(cat.initial, 0); // first state by source order
    }

    #[test]
    fn multistate_automaton_transitions_to_real_state_ok() {
        let cats = extract_str(
            "#automaton Counter { \
             #states: [Idle, Counting]; \
             value: u32; \
             #transition start -> Counting { } \
             #transition tick { } \
             #transition halt -> Idle { } \
             }",
        )
        .unwrap();
        let cat = cats.lookup("Counter").unwrap();
        assert_eq!(cat.transitions.len(), 3);
        let dests: Vec<_> = cat
            .transitions
            .iter()
            .map(|t| t.destination.as_deref())
            .collect();
        assert_eq!(dests, vec![Some("Counting"), None, Some("Idle")]);
    }

    #[test]
    fn multistate_unknown_destination_is_e0430() {
        let errors = extract_str(
            "#automaton Sm { \
             #states: [Idle, Running]; \
             #transition start -> Bogus { } \
             }",
        )
        .unwrap_err();
        assert!(errors.iter().any(|e| matches!(
            e,
            EffectError::UnknownState {
                ref automaton,
                ref transition,
                ref target,
                ..
            } if automaton == "Sm" && transition == "start" && target == "Bogus"
        )));
    }

    // ── Duplicate detection ──────────────────────────────────────────────

    #[test]
    fn duplicate_transition_in_same_automaton_is_e0432() {
        let errors = extract_str(
            "#automaton Sm { \
             #states: [Idle, Running]; \
             #transition tick -> Running { } \
             #transition tick -> Idle { } \
             }",
        )
        .unwrap_err();
        assert!(errors.iter().any(|e| matches!(
            e,
            EffectError::DuplicateTransition {
                ref automaton,
                ref transition,
                ..
            } if automaton == "Sm" && transition == "tick"
        )));
    }

    #[test]
    fn errors_collected_not_fail_fast() {
        let errors = extract_str(
            "#automaton Sm { \
             #states: [Idle, Running]; \
             #transition first -> Bogus { } \
             #transition second -> AlsoBogus { } \
             #transition third -> Running { } \
             #transition third -> Idle { } \
             }",
        )
        .unwrap_err();
        // Two unknown-state errors + one duplicate-transition error.
        let unknown_count = errors
            .iter()
            .filter(|e| matches!(e, EffectError::UnknownState { .. }))
            .count();
        let dup_count = errors
            .iter()
            .filter(|e| matches!(e, EffectError::DuplicateTransition { .. }))
            .count();
        assert_eq!(unknown_count, 2);
        assert_eq!(dup_count, 1);
    }

    // ── Multiple automata in one program ─────────────────────────────────

    #[test]
    fn multiple_automatons_each_get_own_category() {
        let cats = extract_str(
            "#automaton A { value: u32; } \
             #automaton B { #states: [On, Off]; #transition flip -> Off { } }",
        )
        .unwrap();
        assert_eq!(cats.len(), 2);
        assert!(cats.lookup("A").unwrap().is_monoid);
        assert!(!cats.lookup("B").unwrap().is_monoid);
        assert_eq!(cats.lookup("B").unwrap().transitions.len(), 1);
    }

    // ── item_index correctly populated ───────────────────────────────────

    #[test]
    fn item_index_reflects_program_position() {
        let cats = extract_str(
            "@fn helper() { } #automaton Sm { #states: [A]; } #automaton Tm { }",
        )
        .unwrap();
        // helper at 0; Sm at 1; Tm at 2.
        assert_eq!(cats.lookup("Sm").unwrap().item_index, 1);
        assert_eq!(cats.lookup("Tm").unwrap().item_index, 2);
    }

    // ── Realistic combined example ───────────────────────────────────────

    #[test]
    fn realistic_program_extracts_cleanly() {
        let src = "\
            #automaton Counter {\n  \
              #basis: counter_basis;\n  \
              #states: [Idle, Counting, Halted];\n  \
              value: u32;\n  \
              #transition start -> Counting { Counter.value = 0u32; }\n  \
              #transition tick { Counter.value = Counter.value + 1u32; }\n  \
              #transition halt -> Halted { }\n\
            }";
        let cats = extract_str(src).unwrap();
        let cat = cats.lookup("Counter").unwrap();
        assert!(!cat.is_monoid);
        assert_eq!(cat.states.len(), 3);
        let trans_names: Vec<_> = cat.transitions.iter().map(|t| t.name.as_str()).collect();
        assert_eq!(trans_names, vec!["start", "tick", "halt"]);
        // start -> Counting; tick: same-state; halt -> Halted.
        assert_eq!(cat.transitions[0].destination.as_deref(), Some("Counting"));
        assert!(cat.transitions[1].destination.is_none());
        assert_eq!(cat.transitions[2].destination.as_deref(), Some("Halted"));
    }

    // ─── Slice 2: mutation profile extraction ────────────────────────────

    fn make_field(automaton: &str, field: &str) -> FieldRef {
        FieldRef {
            automaton: automaton.to_owned(),
            field: field.to_owned(),
        }
    }

    // ── Empty / no-effects baseline ──────────────────────────────────────

    #[test]
    fn empty_program_has_empty_profiles() {
        let p = profiles_str("").unwrap();
        assert!(p.is_empty());
    }

    #[test]
    fn program_without_effects_has_empty_profiles() {
        let p = profiles_str("@fn helper() { }").unwrap();
        assert!(p.is_empty());
    }

    // ── Direct writes via MutateShort ────────────────────────────────────

    #[test]
    fn mutate_short_collected_as_direct_write() {
        let p = profiles_str(
            "#automaton Counter { value: u32; } \
             #effect bump() #mutates: [Counter] { Counter.value = 1u32; }",
        )
        .unwrap();
        let id = CallableId::Effect("bump".to_owned());
        let profile = p.lookup(&id).expect("bump profile");
        assert!(profile.actual_writes.contains(&make_field("Counter", "value")));
        assert!(profile.actual_automata.contains("Counter"));
    }

    // ── Direct writes via canonical Mutate { … } ─────────────────────────

    #[test]
    fn mutate_canonical_collected() {
        let p = profiles_str(
            "#automaton Counter { value: u32; flags: u8; } \
             #effect set_both() #mutates: [Counter] { \
               #mutate Counter { value = 1u32, flags = 0u8 }; \
             }",
        )
        .unwrap();
        let id = CallableId::Effect("set_both".to_owned());
        let profile = p.lookup(&id).expect("set_both profile");
        assert!(profile.actual_writes.contains(&make_field("Counter", "value")));
        assert!(profile.actual_writes.contains(&make_field("Counter", "flags")));
    }

    // ── Interrupts use the same machinery ────────────────────────────────

    #[test]
    fn interrupt_writes_collected() {
        let p = profiles_str(
            "#automaton Counter { value: u32; } \
             #interrupt UART_RX() #mutates: [Counter] #priority: HIGH { \
               Counter.value = Counter.value + 1u32; \
             }",
        )
        .unwrap();
        let id = CallableId::Interrupt("UART_RX".to_owned());
        let profile = p.lookup(&id).expect("UART_RX profile");
        assert!(profile.actual_writes.contains(&make_field("Counter", "value")));
    }

    // ── Transitions ──────────────────────────────────────────────────────

    #[test]
    fn transition_writes_collected() {
        let p = profiles_str(
            "#automaton Counter { value: u32; \
             #transition tick { Counter.value = Counter.value + 1u32; } \
             }",
        )
        .unwrap();
        let id = CallableId::Transition {
            automaton: "Counter".to_owned(),
            name: "tick".to_owned(),
        };
        let profile = p.lookup(&id).expect("tick profile");
        assert!(profile.actual_writes.contains(&make_field("Counter", "value")));
    }

    // ── Transitive proc-call closure ─────────────────────────────────────

    #[test]
    fn proc_call_to_effect_propagates_writes() {
        // bump() calls inner() via #>; inner() writes Counter.value;
        // bump() should inherit that write transitively.
        let p = profiles_str(
            "#automaton Counter { value: u32; } \
             #effect inner() #mutates: [Counter] { Counter.value = 1u32; } \
             #effect bump() #mutates: [Counter] { #> inner(); }",
        )
        .unwrap();
        let bump_id = CallableId::Effect("bump".to_owned());
        let profile = p.lookup(&bump_id).expect("bump profile");
        assert!(
            profile
                .actual_writes
                .contains(&make_field("Counter", "value")),
            "bump should transitively pick up Counter.value write from inner"
        );
    }

    #[test]
    fn proc_call_to_transition_propagates_writes() {
        let p = profiles_str(
            "#automaton Counter { value: u32; \
             #transition tick { Counter.value = Counter.value + 1u32; } \
             } \
             #effect bumper() #mutates: [Counter] { #> tick(); }",
        )
        .unwrap();
        let bumper_id = CallableId::Effect("bumper".to_owned());
        let profile = p.lookup(&bumper_id).expect("bumper profile");
        assert!(
            profile
                .actual_writes
                .contains(&make_field("Counter", "value")),
            "bumper should transitively pick up Counter.value write from tick"
        );
    }

    #[test]
    fn deep_proc_call_chain_propagates_all_writes() {
        // a() calls b(); b() calls c(); c() writes Counter.value.
        // a() should end up with that write transitively.
        let p = profiles_str(
            "#automaton Counter { value: u32; } \
             #effect c() #mutates: [Counter] { Counter.value = 0u32; } \
             #effect b() #mutates: [Counter] { #> c(); } \
             #effect a() #mutates: [Counter] { #> b(); }",
        )
        .unwrap();
        let a_id = CallableId::Effect("a".to_owned());
        let profile = p.lookup(&a_id).expect("a profile");
        assert!(
            profile
                .actual_writes
                .contains(&make_field("Counter", "value")),
            "a should transitively reach Counter.value through b -> c"
        );
    }

    // ── Validation: E0410 ────────────────────────────────────────────────

    #[test]
    fn effect_writing_undeclared_automaton_is_e0410() {
        // bump() #mutates: [] but writes Counter.value -- E0410.
        let errors = profiles_str(
            "#automaton Counter { value: u32; } \
             #effect bump() #mutates: [] { Counter.value = 1u32; }",
        )
        .unwrap_err();
        assert!(errors.iter().any(|e| matches!(
            e,
            EffectError::EffectMutatesUndeclaredAutomaton {
                ref callable,
                ref automaton,
                ..
            } if callable == "bump" && automaton == "Counter"
        )));
    }

    #[test]
    fn transitive_undeclared_mutate_is_e0410() {
        // bump() #mutates: [] but calls inner() which writes Counter.value.
        // E0410 should fire even though bump() has no direct write.
        let errors = profiles_str(
            "#automaton Counter { value: u32; } \
             #effect inner() #mutates: [Counter] { Counter.value = 1u32; } \
             #effect bump() #mutates: [] { #> inner(); }",
        )
        .unwrap_err();
        assert!(errors.iter().any(|e| matches!(
            e,
            EffectError::EffectMutatesUndeclaredAutomaton {
                ref callable,
                ..
            } if callable == "bump"
        )));
    }

    // ── Validation: E0411 ────────────────────────────────────────────────

    #[test]
    fn effect_writing_excluded_automaton_is_e0411() {
        let errors = profiles_str(
            "#automaton Counter { value: u32; } \
             #effect bump() #mutates: [Counter] #cannot_mutate: [Counter] { \
               Counter.value = 1u32; \
             }",
        )
        .unwrap_err();
        assert!(errors.iter().any(|e| matches!(
            e,
            EffectError::EffectMutatesExcludedAutomaton {
                ref callable,
                ref automaton,
                ..
            } if callable == "bump" && automaton == "Counter"
        )));
    }

    // ── Negative: declared mutation OK ───────────────────────────────────

    #[test]
    fn correctly_declared_mutate_is_clean() {
        let p = profiles_str(
            "#automaton Counter { value: u32; } \
             #effect bump() #mutates: [Counter] { Counter.value = 1u32; }",
        );
        assert!(p.is_ok(), "got errors: {:?}", p);
    }

    // ── Multi-automaton effect ───────────────────────────────────────────

    #[test]
    fn effect_writing_two_automatons_validated_correctly() {
        let p = profiles_str(
            "#automaton A { x: u32; } \
             #automaton B { y: u32; } \
             #effect both() #mutates: [A, B] { A.x = 1u32; B.y = 2u32; }",
        );
        assert!(p.is_ok(), "got errors: {:?}", p);
        let id = CallableId::Effect("both".to_owned());
        let profile = p.unwrap().lookup(&id).cloned().expect("both profile");
        assert_eq!(profile.actual_automata.len(), 2);
        assert!(profile.actual_automata.contains("A"));
        assert!(profile.actual_automata.contains("B"));
    }

    // ── Realistic combined exercise ──────────────────────────────────────

    #[test]
    fn realistic_program_profiles_correctly() {
        let src = "\
            #automaton Counter {\n  \
              value: u32;\n  \
              #transition tick { Counter.value = Counter.value + 1u32; }\n\
            }\n\
            #effect bump() #mutates: [Counter] {\n  \
              #> tick();\n\
            }\n\
            #effect double() #mutates: [Counter] {\n  \
              #> bump();\n  \
              #> bump();\n\
            }\n\
        ";
        let p = profiles_str(src).expect("profile extraction");

        // tick directly writes Counter.value.
        let tick_id = CallableId::Transition {
            automaton: "Counter".to_owned(),
            name: "tick".to_owned(),
        };
        assert!(p
            .lookup(&tick_id)
            .unwrap()
            .actual_writes
            .contains(&make_field("Counter", "value")));

        // bump transitively reaches Counter.value via tick.
        let bump_id = CallableId::Effect("bump".to_owned());
        assert!(p
            .lookup(&bump_id)
            .unwrap()
            .actual_writes
            .contains(&make_field("Counter", "value")));

        // double transitively reaches Counter.value via bump → tick.
        let double_id = CallableId::Effect("double".to_owned());
        assert!(p
            .lookup(&double_id)
            .unwrap()
            .actual_writes
            .contains(&make_field("Counter", "value")));
    }

    // ── Mutual recursion (cycle) terminates ──────────────────────────────

    #[test]
    fn mutual_recursion_terminates() {
        // a() calls b() calls a(). The transitive closure must terminate;
        // each gets the other's writes once.
        let src = "#automaton Counter { value: u32; } \
                   #effect a() #mutates: [Counter] { Counter.value = 1u32; #> b(); } \
                   #effect b() #mutates: [Counter] { #> a(); }";
        let p = profiles_str(src).expect("should not infinite-loop");
        let a_id = CallableId::Effect("a".to_owned());
        let b_id = CallableId::Effect("b".to_owned());
        // a directly writes Counter.value; b inherits it from a.
        assert!(p
            .lookup(&a_id)
            .unwrap()
            .actual_writes
            .contains(&make_field("Counter", "value")));
        assert!(p
            .lookup(&b_id)
            .unwrap()
            .actual_writes
            .contains(&make_field("Counter", "value")));
    }

    // ─── Slice 3: proc-call graph + cycle detection ──────────────────────

    fn graph_str(src: &str) -> Result<ProcCallGraph, Vec<EffectError>> {
        let tokens = tokenize(src).expect("tokenize");
        let program = parse(&tokens).expect("parse");
        let resolution = resolve(&program).expect("resolve");
        extract_call_graph(&program, &resolution)
    }

    // ── Empty / no-edges baseline ────────────────────────────────────────

    #[test]
    fn empty_program_has_empty_graph() {
        let g = graph_str("").unwrap();
        assert!(g.is_empty());
    }

    #[test]
    fn callable_with_no_proc_calls_has_no_outgoing_edges() {
        let g = graph_str(
            "#automaton Counter { value: u32; } \
             #effect bump() #mutates: [Counter] { Counter.value = 1u32; }",
        )
        .unwrap();
        assert!(g.is_empty(), "no #> calls → no edges");
    }

    // ── Linear chain (no cycle) ──────────────────────────────────────────

    #[test]
    fn linear_chain_two_callables_clean() {
        let g = graph_str(
            "#effect a() #mutates: [] { } \
             #effect b() #mutates: [] { #> a(); }",
        )
        .unwrap();
        let b_id = CallableId::Effect("b".to_owned());
        let a_id = CallableId::Effect("a".to_owned());
        // b → a; a has no outgoing.
        let b_callees = g.callees(&b_id).expect("b has callees");
        assert!(b_callees.contains(&a_id));
        assert!(g.callees(&a_id).is_none());
    }

    #[test]
    fn linear_chain_three_callables_clean() {
        let g = graph_str(
            "#effect a() #mutates: [] { } \
             #effect b() #mutates: [] { #> a(); } \
             #effect c() #mutates: [] { #> b(); }",
        )
        .unwrap();
        // c → b → a; no cycles.
        assert_eq!(g.len(), 2); // c and b have outgoing edges
    }

    // ── Self-loop ────────────────────────────────────────────────────────

    #[test]
    fn self_loop_is_e0422() {
        let errors = graph_str(
            "#effect recursive() #mutates: [] { #> recursive(); }",
        )
        .unwrap_err();
        assert_eq!(errors.len(), 1);
        match &errors[0] {
            EffectError::ProcCallCycle { cycle, cycle_display } => {
                assert_eq!(cycle.len(), 1);
                assert!(cycle[0].contains("recursive"));
                assert!(cycle_display.contains("recursive"));
            }
            other => panic!("expected ProcCallCycle, got {:?}", other),
        }
    }

    // ── Mutual recursion (2-cycle) ───────────────────────────────────────

    #[test]
    fn mutual_recursion_is_e0422() {
        let errors = graph_str(
            "#effect a() #mutates: [] { #> b(); } \
             #effect b() #mutates: [] { #> a(); }",
        )
        .unwrap_err();
        assert_eq!(errors.len(), 1);
        match &errors[0] {
            EffectError::ProcCallCycle { cycle, .. } => {
                assert_eq!(cycle.len(), 2);
                let names: HashSet<&str> =
                    cycle.iter().map(|s| s.as_str()).collect();
                assert!(names.iter().any(|n| n.contains('a')));
                assert!(names.iter().any(|n| n.contains('b')));
            }
            _ => panic!(),
        }
    }

    // ── 3-cycle ──────────────────────────────────────────────────────────

    #[test]
    fn three_cycle_is_e0422() {
        let errors = graph_str(
            "#effect a() #mutates: [] { #> b(); } \
             #effect b() #mutates: [] { #> c(); } \
             #effect c() #mutates: [] { #> a(); }",
        )
        .unwrap_err();
        assert_eq!(errors.len(), 1);
        match &errors[0] {
            EffectError::ProcCallCycle { cycle, cycle_display } => {
                assert_eq!(cycle.len(), 3);
                // Display contains all three with arrows.
                assert!(cycle_display.contains('→'));
            }
            _ => panic!(),
        }
    }

    // ── Multiple disjoint cycles ─────────────────────────────────────────

    #[test]
    fn two_disjoint_cycles_both_reported() {
        let errors = graph_str(
            "#effect a() #mutates: [] { #> b(); } \
             #effect b() #mutates: [] { #> a(); } \
             #effect c() #mutates: [] { #> d(); } \
             #effect d() #mutates: [] { #> c(); }",
        )
        .unwrap_err();
        assert_eq!(
            errors.len(),
            2,
            "expected two disjoint cycles, got {} errors: {:?}",
            errors.len(),
            errors
        );
    }

    // ── Cycle through transitions ────────────────────────────────────────

    #[test]
    fn transition_self_call_cycle_caught() {
        // Sm has #transition tick which calls itself.
        let errors = graph_str(
            "#automaton Sm { value: u32; \
             #transition tick { #> tick(); } \
             }",
        )
        .unwrap_err();
        assert_eq!(errors.len(), 1);
        match &errors[0] {
            EffectError::ProcCallCycle { cycle, .. } => {
                assert_eq!(cycle.len(), 1);
                assert!(cycle[0].contains("tick"));
            }
            _ => panic!(),
        }
    }

    // ── DAG with diamond (no cycle) ──────────────────────────────────────

    #[test]
    fn diamond_dag_is_clean() {
        // a → b, a → c, both b and c → d. Diamond shape, no cycle.
        let g = graph_str(
            "#effect d() #mutates: [] { } \
             #effect c() #mutates: [] { #> d(); } \
             #effect b() #mutates: [] { #> d(); } \
             #effect a() #mutates: [] { #> b(); #> c(); }",
        );
        assert!(g.is_ok(), "diamond is a DAG, should be clean: {:?}", g);
    }

    // ── Canonicalisation: same cycle from different entry points ─────────

    #[test]
    fn cycle_reported_once_regardless_of_dfs_entry() {
        // a → b → a is a 2-cycle. DFS could enter from a or b; either way
        // the cycle should be reported exactly once after canonicalisation.
        let errors = graph_str(
            "#effect a() #mutates: [] { #> b(); } \
             #effect b() #mutates: [] { #> a(); }",
        )
        .unwrap_err();
        assert_eq!(errors.len(), 1, "cycle should be reported once");
    }

    // ── Realistic program (from Wari-style design) ───────────────────────

    #[test]
    fn realistic_clean_program() {
        // bump → tick (transition), zap → reset (transition). No cycle.
        let g = graph_str(
            "#automaton Counter { value: u32; \
             #transition tick { Counter.value = Counter.value + 1u32; } \
             #transition reset { Counter.value = 0u32; } \
             } \
             #effect bump() #mutates: [Counter] { #> tick(); } \
             #effect zap()  #mutates: [Counter] { #> reset(); }",
        );
        assert!(g.is_ok(), "should be clean: {:?}", g);
    }

    // ─── Slice 4: interrupt-overlap set R(A) ─────────────────────────────

    fn overlap_str(src: &str) -> InterruptOverlap {
        let tokens = tokenize(src).expect("tokenize");
        let program = parse(&tokens).expect("parse");
        let resolution = resolve(&program).expect("resolve");
        let profiles = extract_mutation_profiles(&program, &resolution).expect("profiles");
        compute_interrupt_overlap(&profiles)
    }

    // ── No interrupts → empty overlap ────────────────────────────────────

    #[test]
    fn empty_program_has_empty_overlap() {
        let o = overlap_str("");
        assert!(o.is_empty());
    }

    #[test]
    fn no_interrupts_means_no_overlap() {
        let o = overlap_str(
            "#automaton C { v: u32; } \
             #effect e() #mutates: [C] { C.v = 1u32; }",
        );
        assert!(o.is_empty());
        assert!(!o.is_overlapped("C"));
        assert!(o.interrupts_for("C").is_empty());
    }

    // ── Direct interrupt → automaton overlap ────────────────────────────

    #[test]
    fn single_interrupt_one_automaton_overlap() {
        let o = overlap_str(
            "#automaton Counter { value: u32; } \
             #interrupt UART_RX() #mutates: [Counter] #priority: HIGH { \
               Counter.value = 1u32; \
             }",
        );
        assert!(o.is_overlapped("Counter"));
        assert!(o.interrupts_for("Counter").contains("UART_RX"));
        assert_eq!(o.interrupts_for("Counter").len(), 1);
    }

    // ── Multiple interrupts, same automaton ──────────────────────────────

    #[test]
    fn two_interrupts_same_automaton_both_overlap() {
        let o = overlap_str(
            "#automaton Counter { value: u32; } \
             #interrupt UART_RX() #mutates: [Counter] #priority: HIGH { \
               Counter.value = 1u32; \
             } \
             #interrupt TIMER() #mutates: [Counter] #priority: LOW { \
               Counter.value = Counter.value + 1u32; \
             }",
        );
        let interrupts = o.interrupts_for("Counter");
        assert_eq!(interrupts.len(), 2);
        assert!(interrupts.contains("UART_RX"));
        assert!(interrupts.contains("TIMER"));
    }

    // ── One interrupt, multiple automatons ──────────────────────────────

    #[test]
    fn one_interrupt_two_automatons_both_overlap() {
        let o = overlap_str(
            "#automaton A { x: u32; } \
             #automaton B { y: u32; } \
             #interrupt SHARED() #mutates: [A, B] #priority: HIGH { \
               A.x = 1u32; B.y = 2u32; \
             }",
        );
        assert!(o.is_overlapped("A"));
        assert!(o.is_overlapped("B"));
        assert!(o.interrupts_for("A").contains("SHARED"));
        assert!(o.interrupts_for("B").contains("SHARED"));
    }

    // ── Transitive: interrupt → effect → automaton ──────────────────────

    #[test]
    fn transitive_interrupt_overlap_through_proc_call() {
        // The interrupt directly mutates [], but calls inner() which
        // mutates Counter. Per E2's transitive closure, the interrupt's
        // actual_automata includes Counter. Therefore Counter is in R(A).
        let o = overlap_str(
            "#automaton Counter { value: u32; } \
             #effect inner() #mutates: [Counter] { Counter.value = 1u32; } \
             #interrupt OUTER() #mutates: [Counter] #priority: HIGH { \
               #> inner(); \
             }",
        );
        assert!(o.is_overlapped("Counter"));
        assert!(o.interrupts_for("Counter").contains("OUTER"));
    }

    // ── Effects don't show up in R(A) ────────────────────────────────────

    #[test]
    fn effects_alone_dont_create_overlap() {
        // Three effects, no interrupts — R(A) is empty regardless of
        // how many effects mutate the automaton.
        let o = overlap_str(
            "#automaton Counter { value: u32; } \
             #effect a() #mutates: [Counter] { Counter.value = 1u32; } \
             #effect b() #mutates: [Counter] { Counter.value = 2u32; } \
             #effect c() #mutates: [Counter] { Counter.value = 3u32; }",
        );
        assert!(o.is_empty(), "effects shouldn't create overlap");
    }

    // ── Realistic firmware: IRQ + main-loop effect ───────────────────────

    #[test]
    fn realistic_irq_plus_consumer() {
        // The Wari-shape pattern: UART_RX_IRQ produces; consume_byte
        // consumes. Both touch UartRx → R(UartRx) = {UART_RX_IRQ}.
        let o = overlap_str(
            "#automaton UartRx { data: u8; head: usize; tail: usize; } \
             #interrupt UART_RX_IRQ() #mutates: [UartRx] #priority: HIGH { \
               UartRx.head = UartRx.head + 1usize; \
             } \
             #effect consume_byte() #mutates: [UartRx] { \
               UartRx.tail = UartRx.tail + 1usize; \
             }",
        );
        assert!(o.is_overlapped("UartRx"));
        assert_eq!(o.interrupts_for("UartRx").len(), 1);
        assert!(o.interrupts_for("UartRx").contains("UART_RX_IRQ"));
    }

    // ── Lookup for non-existent automaton ───────────────────────────────

    #[test]
    fn lookup_for_nonexistent_automaton_returns_empty() {
        let o = overlap_str(
            "#automaton C { v: u32; } \
             #interrupt I() #mutates: [C] #priority: HIGH { C.v = 1u32; }",
        );
        // Querying for an automaton that doesn't exist returns the empty set.
        assert!(o.interrupts_for("Nonexistent").is_empty());
        assert!(!o.is_overlapped("Nonexistent"));
    }

    // ── all() iterator ───────────────────────────────────────────────────

    #[test]
    fn all_iterator_returns_every_overlapped_automaton() {
        let o = overlap_str(
            "#automaton A { x: u32; } \
             #automaton B { y: u32; } \
             #automaton C { z: u32; } \
             #interrupt I1() #mutates: [A] #priority: HIGH { A.x = 1u32; } \
             #interrupt I2() #mutates: [B] #priority: LOW { B.y = 1u32; }",
        );
        // A and B are overlapped; C is not.
        let names: HashSet<&String> = o.all().map(|(name, _)| name).collect();
        assert_eq!(names.len(), 2);
        assert!(names.iter().any(|n| n.as_str() == "A"));
        assert!(names.iter().any(|n| n.as_str() == "B"));
        assert!(!names.iter().any(|n| n.as_str() == "C"));
    }

    // ── Slice 19: `#flush` flows into the mutation profile ──────────────

    #[test]
    fn flush_records_one_write_per_field() {
        // `#flush S;` where `S` has fields a, b, c records three
        // direct writes — `S.a`, `S.b`, `S.c` — so the §6.2 check
        // and the §7 ortho engine both see a flush as a write to
        // every field of the staged automaton.
        let profiles = profiles_str(
            "#staged #automaton S { a: u32; b: u32; c: u32; } \
             #effect commit() #mutates: [S] { #flush S; return; }",
        )
        .expect("profile extraction");
        let id = CallableId::Effect("commit".to_owned());
        let p = profiles.lookup(&id).expect("profile for commit");
        assert!(
            p.actual_writes.contains(&make_field("S", "a")),
            "expected #flush to record write to S.a; got {:?}",
            p.actual_writes
        );
        assert!(
            p.actual_writes.contains(&make_field("S", "b")),
            "expected #flush to record write to S.b; got {:?}",
            p.actual_writes
        );
        assert!(
            p.actual_writes.contains(&make_field("S", "c")),
            "expected #flush to record write to S.c; got {:?}",
            p.actual_writes
        );
        assert!(p.actual_automata.contains("S"));
    }

    #[test]
    fn flush_outside_mutates_clause_is_e0410() {
        // `#flush S;` inside an effect that doesn't list `S` in its
        // `#mutates: [...]` clause must surface E0410 — the existing
        // mutation-profile check fires uniformly because slice 19
        // routes flush writes through `actual_writes`.
        let errors = profiles_str(
            "#staged #automaton S { a: u32; } \
             #effect bad_commit() #mutates: [] { #flush S; return; }",
        )
        .expect_err("expected E0410 for flush outside #mutates");
        assert!(
            errors.iter().any(|e| matches!(
                e,
                EffectError::EffectMutatesUndeclaredAutomaton {
                    ref callable,
                    ref automaton,
                    ..
                } if callable == "bad_commit" && automaton == "S"
            )),
            "expected E0410 for `bad_commit` writing `S` via flush; got {errors:?}"
        );
    }

    #[test]
    fn flush_inside_mutates_clause_is_well_formed() {
        // The happy path — `#flush S;` inside an effect that lists
        // `S` in `#mutates: [...]` extracts cleanly with no errors.
        let profiles = profiles_str(
            "#staged #automaton S { a: u32; } \
             #effect commit() #mutates: [S] { #flush S; return; }",
        )
        .expect("happy-path extraction");
        let id = CallableId::Effect("commit".to_owned());
        assert!(profiles.lookup(&id).is_some(), "profile present for commit");
    }

    #[test]
    fn flush_transitively_propagates_through_proc_call() {
        // A caller that #> calls a flush-containing callee picks
        // up the flush's write set transitively. Mirrors the
        // existing transitive-mutate test for #mutate.
        let errors = profiles_str(
            "#staged #automaton S { a: u32; } \
             #effect inner() #mutates: [S] { #flush S; return; } \
             #effect outer() #mutates: [] { #> inner(); }",
        )
        .expect_err("expected E0410 for transitive flush");
        assert!(
            errors.iter().any(|e| matches!(
                e,
                EffectError::EffectMutatesUndeclaredAutomaton {
                    ref callable,
                    ..
                } if callable == "outer"
            )),
            "expected E0410 on `outer` via transitive flush; got {errors:?}"
        );
    }
}
