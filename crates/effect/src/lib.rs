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
//! **Slice 1 (this PR):** §6.1 category construction. Public entry point
//! [`extract_categories`] walks every `#automaton` and produces an
//! [`AutomatonCategory`] per automaton — its state set, its transitions
//! as morphisms, and the implicit identity at every state. Validates that
//! every `#transition Source -> Target` references a state that exists in
//! the automaton's `#states` declaration (`E0430`). Monoid automata
//! (no `#states` clause) get a synthetic `[Ready]` state per Decision #5
//! Rule 4. Reachability / stuck-state / deadlock-SCC warnings, mutation
//! profile extraction, proc-call resolution + classification, state-tag
//! update points, and interrupt-overlap sets all arrive in subsequent
//! slices.

#![forbid(unsafe_code)]
#![warn(missing_docs)]

use std::collections::{HashMap, HashSet};

use clifford_ast::{AutomatonDecl, Item, Program, TransitionDecl};
use clifford_lexer::Span;
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

#[cfg(test)]
mod tests {
    use super::*;
    use clifford_lexer::tokenize;
    use clifford_parser::parse;

    fn extract_str(src: &str) -> Result<Categories, Vec<EffectError>> {
        let tokens = tokenize(src).expect("tokenize");
        let program = parse(&tokens).expect("parse");
        extract_categories(&program)
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
}
