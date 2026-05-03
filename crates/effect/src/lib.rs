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
//! **Slice 2 (this PR):** §6.2 mutation profile extraction —
//! [`extract_mutation_profiles`]. For every `#effect` / `#interrupt` /
//! `#transition`, computes the set of `(automaton, field)` pairs the
//! callable writes — *transitively* through `#> proc()` calls per
//! Decision #3. Validates that each callable's actual mutation-set is a
//! subset of its declared `#mutates` clause (`E0410`) and disjoint from
//! its `#cannot_mutate` clause (`E0411`). The output is what
//! `crates/ortho` consumes (per §7.2) — it's the per-effect "what fields
//! does this thing write" set that the GA orthogonality check operates
//! on. §6.3 proc-call resolution + CallContext propagation,
//! §6.4 state-tag update points, §6.5 invariant verification,
//! §6.6 atomic-annotation lowering hints, and Refinement #5e
//! interrupt-overlap sets all arrive in subsequent slices.

#![forbid(unsafe_code)]
#![warn(missing_docs)]

use std::collections::{HashMap, HashSet};

use clifford_ast::{
    AutomatonDecl, Block, FieldAssign, Item, Program, Stmt, StmtKind, TransitionDecl,
};
use clifford_lexer::Span;
use clifford_resolve::{BindingRef, CallContext, Resolution};
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
/// calls per Decision #3. This is what `crates/ortho` consumes.
///
/// `actual_automata` is the set of automaton names appearing in
/// `actual_writes`; pre-computed for the §6.2 subset / disjointness
/// checks against the declared `#mutates` and `#cannot_mutate` clauses.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct MutationProfile {
    /// Every `(automaton, field)` pair the callable writes (direct +
    /// transitive through proc-calls).
    pub actual_writes: HashSet<FieldRef>,
    /// Every automaton name appearing in `actual_writes`. Derived
    /// upstream and cached here for fast subset / disjointness checks.
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

/// Internal: per-callable record of direct writes and direct proc-call
/// targets. The transitive-closure pass runs over this to produce the
/// final [`MutationProfile`]s.
#[derive(Debug, Clone, Default)]
struct DirectProfile {
    writes: HashSet<FieldRef>,
    proc_calls: HashSet<CallableId>,
}

fn collect_direct_profiles(
    program: &Program,
    resolution: &Resolution,
) -> HashMap<CallableId, DirectProfile> {
    let mut map: HashMap<CallableId, DirectProfile> = HashMap::new();

    for item in &program.items {
        match item {
            Item::Effect(decl) => {
                let id = CallableId::Effect(decl.name.clone());
                let profile = walk_body_for_direct(&decl.body, resolution);
                map.insert(id, profile);
            }
            Item::Interrupt(decl) => {
                let id = CallableId::Interrupt(decl.name.clone());
                let profile = walk_body_for_direct(&decl.body, resolution);
                map.insert(id, profile);
            }
            Item::Automaton(decl) => {
                for trans in &decl.transitions {
                    let id = CallableId::Transition {
                        automaton: decl.name.clone(),
                        name: trans.name.clone(),
                    };
                    let profile = walk_transition_for_direct(decl, trans, resolution);
                    map.insert(id, profile);
                }
            }
            _ => {}
        }
    }

    map
}

fn walk_body_for_direct(body: &Block, resolution: &Resolution) -> DirectProfile {
    let mut profile = DirectProfile::default();
    walk_stmts(&body.stmts, resolution, &mut profile);
    profile
}

fn walk_transition_for_direct(
    _automaton: &AutomatonDecl,
    trans: &TransitionDecl,
    resolution: &Resolution,
) -> DirectProfile {
    let mut profile = DirectProfile::default();
    walk_stmts(&trans.body.stmts, resolution, &mut profile);
    profile
}

fn walk_stmts(stmts: &[Stmt], resolution: &Resolution, out: &mut DirectProfile) {
    for stmt in stmts {
        match &stmt.kind {
            StmtKind::Mutate { automaton, assigns } => {
                for FieldAssign { field, .. } in assigns {
                    out.writes.insert(FieldRef {
                        automaton: automaton.clone(),
                        field: field.clone(),
                    });
                }
            }
            StmtKind::MutateShort {
                automaton, field, ..
            } => {
                out.writes.insert(FieldRef {
                    automaton: automaton.clone(),
                    field: field.clone(),
                });
            }
            StmtKind::ProcCall { name, .. } => {
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
            }
            // Recurse into expression-bearing statements where bodies
            // can't appear today, but where a future construct (e.g.
            // `if`/`match` blocks) would warrant recursion. For now,
            // these statements have no nested write sites.
            StmtKind::Let { .. }
            | StmtKind::LetShort { .. }
            | StmtKind::Expr(_)
            | StmtKind::Return(_)
            | StmtKind::UncheckedStore { .. }
            | StmtKind::VolatileStore { .. } => {}
            // Forward-compat for new statement kinds.
            _ => {}
        }
    }
}

/// Compute the transitive closure of writes through the proc-call graph.
///
/// Worklist algorithm: starts each callable with its direct writes,
/// repeatedly unions in callees' writes until no profile changes.
/// Terminates because the union over a finite field-set is monotonic
/// and bounded.
///
/// Cycles in the proc-call graph (e.g. effect A calls B calls A) are
/// handled defensively: each iteration only unions in writes already
/// known, so the fixed point is reached without infinite recursion.
fn transitively_close(
    direct: &HashMap<CallableId, DirectProfile>,
) -> HashMap<CallableId, MutationProfile> {
    // Initialise: each callable starts with its direct writes.
    let mut profiles: HashMap<CallableId, MutationProfile> = direct
        .iter()
        .map(|(id, dp)| {
            let actual_automata: HashSet<String> =
                dp.writes.iter().map(|fr| fr.automaton.clone()).collect();
            (
                id.clone(),
                MutationProfile {
                    actual_writes: dp.writes.clone(),
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
    // (callable, field) pairs in the program.
    loop {
        let mut changed = false;
        for (caller_id, dp) in direct {
            let mut additions: HashSet<FieldRef> = HashSet::new();
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
                        for fr in &callee_profile.actual_writes {
                            if !profiles
                                .get(caller_id)
                                .map(|p| p.actual_writes.contains(fr))
                                .unwrap_or(false)
                            {
                                additions.insert(fr.clone());
                            }
                        }
                    }
                }
            }
            if !additions.is_empty() {
                if let Some(p) = profiles.get_mut(caller_id) {
                    for fr in additions {
                        p.actual_automata.insert(fr.automaton.clone());
                        p.actual_writes.insert(fr);
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
}
