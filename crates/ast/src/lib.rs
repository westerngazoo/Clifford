//! # clifford-ast
//!
//! Shared AST types for the Clifford compiler. Implements §3 (Parser
//! Behavior) of `docs/CLIFFORD_SPEC.md` — specifically the AST node kinds
//! catalogued there.
//!
//! ## Why a separate crate
//!
//! The AST is consumed by every phase from `parser` onward. Putting it in its
//! own crate (rather than re-exporting from `parser`) keeps the dependency
//! pipeline clean (per CLAUDE.md §2: no backward edges) and lets `resolve`,
//! `types`, `check`, etc. depend on AST without depending on the parser.
//!
//! ## Sigil layer is preserved on every node
//!
//! Per §3 of the spec and Decision #1 (sigil layering), the parser stamps
//! every item and statement with its sigil layer (`@` functional, `#`
//! imperative). That stamp lives on the AST node and is consumed by
//! `clifford-check` (§5.5) to enforce the cross-boundary rules without
//! re-scanning source.
//!
//! ## Implementation status
//!
//! First slice (this PR): the [`Program`] / [`Item`] / [`FnDecl`] /
//! [`AutomatonDecl`] skeleton. Items carry name + span only — bodies,
//! parameters, return types, trait lists, automaton fields, transitions,
//! effects all come in subsequent slices alongside their parser
//! implementations.

#![forbid(unsafe_code)]
#![warn(missing_docs)]

use clifford_lexer::Span;

/// Which sigil layer an item or statement belongs to.
///
/// Per Decision #1 in `docs/DECISIONS.md`, every AST node carries this stamp
/// from parsing forward. The type checker (§5.5) reads it to enforce that
/// `@`-layer code cannot contain `#`-layer constructs (Emergent Rule 4).
///
/// # Examples
///
/// ```
/// use clifford_ast::Layer;
/// let l = Layer::Functional;
/// assert!(l.is_functional());
/// assert!(!l.is_imperative());
/// ```
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Layer {
    /// Functional layer (`@`-prefixed). `@fn`, `@type`, `@trait`, `@module`.
    /// Default-immutable, default-`$ [Pure]`. Cannot contain `#`-constructs.
    Functional,
    /// Imperative layer (`#`-prefixed). `#automaton`, `#effect`, `#interrupt`,
    /// `#interface`, `#impl`, `#test`, `#mutate`, `#transition`, `#> proc()`,
    /// narrow unsafe primitives. Can call `@fn` freely; can perform mutation.
    Imperative,
}

impl Layer {
    /// True if this is the functional layer.
    #[must_use]
    pub fn is_functional(self) -> bool {
        matches!(self, Self::Functional)
    }

    /// True if this is the imperative layer.
    #[must_use]
    pub fn is_imperative(self) -> bool {
        matches!(self, Self::Imperative)
    }
}

/// The root of the parsed AST: a sequence of top-level items.
///
/// Per §2.1 of the spec, a program is an unordered sequence of items. Order
/// preservation in this `Vec` is a deliberate choice for reproducible
/// diagnostics and golden-file tests; semantics do not depend on order.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Program {
    /// Source span covering the entire file.
    pub span: Span,
    /// Top-level items in source order.
    pub items: Vec<Item>,
}

/// A top-level item.
///
/// Slice 2 variants cover most of §2.1's shape-only items. Bodies (function
/// bodies, effect bodies, transition bodies, interface method signatures,
/// impl method bodies) are deferred to subsequent parser slices that build
/// out statement/expression parsing.
///
/// Still deferred per §2.1: `@type`, `@trait`, `@module`, `static`, `const`,
/// `extern_block`, `use_decl`. These need type expressions and value
/// expressions which are slice-3+ work.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum Item {
    /// An `@fn` declaration.
    Fn(FnDecl),
    /// An `#automaton` declaration.
    Automaton(AutomatonDecl),
    /// An `@type` declaration: type alias or ADT (sum type).
    Type(TypeDecl),
    /// An `@trait Name { method_sigs }` declaration (§4.5).
    Trait(TraitDecl),
    /// An `#effect` declaration (top-level per Refinement #5a).
    Effect(EffectDecl),
    /// An `#interrupt` declaration. Differs from `#effect` in that the name
    /// becomes the linker symbol (Decision #10) and `#priority` is required.
    Interrupt(InterruptDecl),
    /// An `#interface` declaration (Decision #16).
    Interface(InterfaceDecl),
    /// An `#impl Interface for Automaton { … }` block (Decision #16).
    Impl(ImplDecl),
    /// A `#test "name" { … }` block (Decision #7).
    Test(TestDecl),
    /// An `@sequential(A, B);` non-concurrency attribute (Decision #11).
    Sequential(SequentialAttr),
}

impl Item {
    /// Which sigil layer does this item live in?
    ///
    /// Derived from the variant rather than stored, since the layer is fully
    /// determined by the item kind. Storing the layer would invite drift
    /// between variant and field.
    ///
    /// `@sequential(A, B);` is treated as functional-layer for
    /// classification purposes — the attribute lives in the functional
    /// layer per its `@` sigil — though it carries no body and serves only
    /// as input to the GA orthogonality engine (§7.3).
    #[must_use]
    pub fn layer(&self) -> Layer {
        match self {
            Self::Fn(_) | Self::Type(_) | Self::Trait(_) | Self::Sequential(_) => {
                Layer::Functional
            }
            Self::Automaton(_)
            | Self::Effect(_)
            | Self::Interrupt(_)
            | Self::Interface(_)
            | Self::Impl(_)
            | Self::Test(_) => Layer::Imperative,
        }
    }

    /// The source span covering the whole item, from leading sigil to its
    /// terminating token (closing brace or terminating semicolon).
    #[must_use]
    pub fn span(&self) -> Span {
        match self {
            Self::Fn(d) => d.span,
            Self::Type(d) => d.span,
            Self::Trait(d) => d.span,
            Self::Automaton(d) => d.span,
            Self::Effect(d) => d.span,
            Self::Interrupt(d) => d.span,
            Self::Interface(d) => d.span,
            Self::Impl(d) => d.span,
            Self::Test(d) => d.span,
            Self::Sequential(d) => d.span,
        }
    }
}

/// An `@fn name(params) -> T $ [TraitList] { body }` declaration.
///
/// Slice-7 wires in real body parsing. Generic parameters, where-clause,
/// and extern modifier still arrive in subsequent slices.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FnDecl {
    /// The function's name.
    pub name: String,
    /// Value parameters in source order. Empty for `@fn name() { }`.
    pub params: Vec<Param>,
    /// Optional return type. `None` means `()` (unit) by spec convention,
    /// preserved as `None` so round-tripping reproduces source exactly.
    pub return_type: Option<TypeExpr>,
    /// `$ [Trait, Trait, …]` markers per Decision #2 / §4.5. Empty if no
    /// `$ [...]` clause appears in source. Per Emergent Rule 2, an empty
    /// trait list at the AST level is interpreted as `[Pure]` by `clifford-types`.
    pub trait_list: Vec<TraitRef>,
    /// `true` if the source had a leading `@partial` modifier per
    /// Decision #23 / ADR 0003. A partial `@fn` opts out of the
    /// totality requirement: it may not terminate (the structural-
    /// recursion check is suppressed), and it can only be called from
    /// other `@partial @fn`s, `#`-layer callables, or future
    /// `@with_budget` blocks (v0.4+). v0.2-α: parser stamps the flag;
    /// `clifford-check` honours it once the totality check lands.
    pub partial: bool,
    /// Function body — sequence of statements per §2.6.
    pub body: Block,
    /// Source span covering `@partial? @fn name(params) -> T $ [...] { body }` end-to-end.
    pub span: Span,
}

/// A single function parameter `mut? name: TypeExpr`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Param {
    /// `true` if the binding is declared `mut name: …`. Per §4.6, a `mut`
    /// parameter binding is only meaningful inside a mutation context;
    /// `clifford-check` (§5.4) rejects `mut` parameters in `@fn` bodies.
    pub mutable: bool,
    /// Parameter name.
    pub name: String,
    /// Parameter type.
    pub ty: TypeExpr,
    /// Source span covering `mut? name: type` end-to-end.
    pub span: Span,
}

/// An `#automaton Name { … }` declaration.
///
/// Slice-8 scope: full automaton body. Members appear in any order inside
/// the braces:
///
/// - `#address: HEX;` — register-block annotation (Decision #6). When present,
///   the automaton is a *register block*; every field must carry an
///   `#offset:` clause and may carry an `#access:` clause. Enforced by
///   `clifford-check` (§5.5), not by the parser.
/// - `#basis: name;` — explicit GA basis-vector assignment (Decision #4). When
///   absent, the GA orthogonality engine (§7) auto-assigns one.
/// - `#states: [Name1, Name2, …];` — explicit state list (Decision #5).
///   When absent, the AST records `states = None` (caller treats this as a
///   monoid automaton with synthetic state `[Ready]` per Decision #5).
/// - field declarations: `name: TypeExpr (#offset: HEX)? (#access: MODE)?;`
/// - `#transition name (-> Dest)? { stmts }` named transition blocks
///   (Refinement #5b). State changes happen *exclusively* inside transitions.
///
/// Generic parameters on `#automaton` are deferred to a later slice — none
/// of the worked examples use them yet.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AutomatonDecl {
    /// The automaton's name.
    pub name: String,
    /// `#address: 0xHEX;` clause if present — marks this as a register block
    /// per Decision #6.
    pub address: Option<AddressClause>,
    /// `#basis: ident;` clause if present per Decision #4. When `None`, the
    /// GA engine assigns a basis vector at §7 lowering time.
    pub basis: Option<BasisClause>,
    /// `#states: [Name1, Name2, …];` clause. `None` means *no `#states`
    /// clause appeared* — the automaton is a monoid (single synthetic state
    /// `[Ready]` per Decision #5). An empty `Some(vec![])` is rejected by
    /// the parser as semantically nonsensical (a multi-state automaton with
    /// zero states cannot exist).
    pub states: Option<Vec<StateName>>,
    /// Field declarations in source order.
    pub fields: Vec<AutomatonField>,
    /// Named `#transition` blocks in source order.
    pub transitions: Vec<TransitionDecl>,
    /// Source span covering `#automaton Name { … }` end-to-end.
    pub span: Span,
}

/// `#address: 0xHEX;` clause on an automaton (Decision #6).
///
/// The raw hex-literal text is preserved (`"0x4000_0000"`) so that error
/// messages can reproduce the source spelling. The numeric value is parsed
/// at type-check time (§5) where target-pointer-width validation happens.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AddressClause {
    /// Raw hex-literal text from the lexer, e.g. `"0x4000_0000"`.
    pub value: String,
    /// Source span covering `#address: 0xHEX`.
    pub span: Span,
}

/// `#basis: name;` clause on an automaton (Decision #4).
///
/// The named identifier becomes the GA basis-vector for this automaton in
/// the orthogonality engine (§7). Absent ⇒ engine auto-assigns.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BasisClause {
    /// The basis-vector identifier the user supplied.
    pub name: String,
    /// Source span covering `#basis: name`.
    pub span: Span,
}

/// One state name inside `#states: [Name1, Name2, …]`.
///
/// Held as a name+span pair so error messages can point back to a specific
/// entry — the GA orthogonality engine in particular needs precise spans
/// when reporting state-conflict diagnostics.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StateName {
    /// The state's identifier.
    pub name: String,
    /// Source span covering the identifier.
    pub span: Span,
}

/// A single field declaration inside an `#automaton` body.
///
/// For ordinary automata: `name: TypeExpr;` — `offset` and `access` are both
/// `None`. For register-block automata (those with an `#address` clause per
/// Decision #6): every field requires `#offset: 0xHEX` and may declare an
/// `#access:` mode. The parser preserves whatever the user wrote;
/// `clifford-check` (§5.5) enforces "register-block automata require
/// `#offset` on every field".
///
/// **Decision #21 reservation:** `kind` discriminates private vs shared
/// fields per `docs/DECISIONS.md` Decision #21 and `docs/CLIFFORD_SPEC.md`
/// §7.0 / §7.9. v0.1–v0.6 implementations always set `kind = FieldKind::Private`;
/// v0.7+ will introduce `FieldKind::Shared { lock: … }` and the mixed-metric
/// algebra extension. The enum is `#[non_exhaustive]` so adding the v0.7
/// variant is a non-breaking AST change.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AutomatonField {
    /// Field name.
    pub name: String,
    /// Field type.
    pub ty: TypeExpr,
    /// `#offset: 0xHEX` if present. Raw hex-literal text preserved.
    pub offset: Option<String>,
    /// `#access: read|write|read_write` if present.
    pub access: Option<AccessMode>,
    /// Whether this field is private (default, v0.1) or shared (v0.7+).
    /// See [`FieldKind`].
    pub kind: FieldKind,
    /// `#hidden` modifier per Decision #25 (algebraic-trivial encapsulation).
    /// `true` if the field is marked `#hidden`, `false` otherwise. A hidden
    /// field's basis vector cannot appear in the `actual_writes` set of any
    /// callable outside the owning automaton's surface; `clifford-resolve`
    /// (slice R3 `require_field` check) emits `E0407 HiddenFieldNotAccessible`
    /// when an outside callable references one. The orthogonality engine
    /// itself has no special machinery — the field simply doesn't enter
    /// non-owning callables' basis assignment, so the wedge product never
    /// collapses against it from outside ("the bit isn't there for outsiders
    /// to refer to"). Order-independent with `offset` / `access`.
    pub hidden: bool,
    /// Source span covering the whole field declaration end-to-end.
    pub span: Span,
}

/// Whether an [`AutomatonField`] participates in the GA orthogonality
/// engine's null subspace (private; current behavior) or non-null subspace
/// (shared; reserved for v0.7+).
///
/// Per `docs/DECISIONS.md` Decision #21 and ADR 0002: v0.1–v0.6 implementations
/// always emit [`FieldKind::Private`]; the parser does not yet recognise the
/// `#shared` field qualifier (the lexer reserves the token but the parser
/// rejects it with a "reserved for v0.7" diagnostic). The v0.7 work that
/// enables Shared fields lands as a non-breaking AST change because this
/// enum is `#[non_exhaustive]`.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum FieldKind {
    /// Private field — contributes a null basis vector to the GA orthogonality
    /// engine's behavior multivector. The §7.4 `wedge == 0` collapse on
    /// shared writes is the current race-detection behavior.
    Private,
    // FUTURE (Decision #21, v0.7+):
    //
    // /// `#shared` field — contributes a non-null basis vector. Overlap on
    // /// this basis vector is permitted but generates a separate proof
    // /// obligation that the named lock is held by both concurrent contexts.
    // Shared {
    //     /// Identifier of the `#lock` declaration guarding this field.
    //     lock: String,
    // },
}

/// Access mode on a register-block field (Decision #6 `#access:` clause).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum AccessMode {
    /// `#access: read` — readable, not writable.
    Read,
    /// `#access: write` — writable, not readable (write-only registers).
    Write,
    /// `#access: read_write` — both.
    ReadWrite,
}

/// A `#transition name (-> Dest)? { stmts }` block (Refinement #5b).
///
/// Per Decision #5, state changes happen *exclusively* inside named
/// transition blocks — no inline state assignments anywhere else.
/// `destination` is optional because monoid automata (no `#states` clause)
/// have nowhere to transition *to*; their transitions just mutate fields.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TransitionDecl {
    /// The transition's name.
    pub name: String,
    /// Optional `-> NextState` target. `None` for monoid-automaton
    /// transitions and for transitions that stay in the current state.
    pub destination: Option<String>,
    /// `$ [Trait, Trait, …]` markers per Decision #22 (extension of
    /// Decision #2's `@fn` trait-list mechanism to imperative-layer
    /// callables). Empty if no `$ [...]` clause appears in source.
    /// Predeclared traits per Decision #22 — `Hardware`, `Realtime`,
    /// `Acquire` / `Release` / `SeqCst`, `LockingDiscipline`,
    /// `PureState`, `Encapsulated` — are validated downstream by
    /// `clifford-types` and consumed by codegen / `cliffordc audit` /
    /// certification. The orthogonality engine ignores them.
    pub trait_list: Vec<TraitRef>,
    /// Transition body — sequence of statements per §2.6.
    pub body: Block,
    /// Source span covering `#transition name (-> Dest)? $ [...]? { … }`
    /// end-to-end.
    pub span: Span,
}

/// An `#effect name(params) -> T #mutates: [A, B] { … }` declaration
/// (top-level per Refinement #5a).
///
/// Slice-4 scope: parameters and optional return type wired in. Body
/// content (statements) lands in slice 6. The `#mutates` clause is
/// required (per §2.5 notes for `#effect`); it may be empty (`#mutates: []`)
/// for pure effects. `#cannot_mutate` is optional. Other effect_meta
/// clauses (`#invariant`, `#atomic`) arrive in subsequent slices.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EffectDecl {
    /// The effect's name.
    pub name: String,
    /// Value parameters in source order. Empty for `#effect tick() …`.
    pub params: Vec<Param>,
    /// Optional return type. `None` ⇒ unit.
    pub return_type: Option<TypeExpr>,
    /// Automaton names listed in `#mutates: [...]`. May be empty for pure
    /// effects (the spec permits an empty list).
    pub mutates: Vec<String>,
    /// Automaton names listed in `#cannot_mutate: [...]`. Optional.
    pub cannot_mutate: Vec<String>,
    /// `$ [Trait, Trait, …]` markers per Decision #22. Empty if no
    /// `$ [...]` clause appears in source. See [`TransitionDecl::trait_list`]
    /// for predeclared trait set and consumer responsibilities.
    pub trait_list: Vec<TraitRef>,
    /// Effect body — sequence of statements.
    pub body: Block,
    /// Source span covering the full declaration end-to-end.
    pub span: Span,
}

/// An `#interrupt NAME(params) -> T #mutates: [A] #priority: HIGH { … }` declaration.
///
/// The `name` is the linker symbol per Decision #10 — users write the
/// target-standard interrupt vector name (e.g., `USART1_IRQHandler`).
/// `#priority` is required for `#interrupt` (per §2.5 notes). Interrupts
/// rarely take parameters in real firmware (the calling convention is
/// fixed by the target), but the grammar permits them and `clifford-check`
/// will validate against the target's interrupt ABI in §8.5 lowering.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InterruptDecl {
    /// Interrupt vector name; becomes the linker symbol (Decision #10).
    pub name: String,
    /// Value parameters in source order. Usually empty for interrupt
    /// handlers; permitted by grammar.
    pub params: Vec<Param>,
    /// Optional return type. Interrupts almost always return unit; the
    /// grammar permits otherwise but the type checker may reject.
    pub return_type: Option<TypeExpr>,
    /// Automaton names listed in `#mutates: [...]`.
    pub mutates: Vec<String>,
    /// Required `#priority: …` per §2.5 effect_meta requirements for `#interrupt`.
    pub priority: PriorityLevel,
    /// `$ [Trait, Trait, …]` markers per Decision #22. Empty if no
    /// `$ [...]` clause appears in source. See [`TransitionDecl::trait_list`]
    /// for predeclared trait set and consumer responsibilities. Particularly
    /// relevant on `#interrupt`s for `Hardware` / `Realtime` classification
    /// and for memory-ordering markers (`Acquire` / `Release` / `SeqCst`).
    pub trait_list: Vec<TraitRef>,
    /// Interrupt handler body — sequence of statements.
    pub body: Block,
    /// Source span covering the full declaration.
    pub span: Span,
}

/// `#priority: LOW | MEDIUM | HIGH | <integer>` per §2.5.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PriorityLevel {
    /// `LOW`
    Low,
    /// `MEDIUM`
    Medium,
    /// `HIGH`
    High,
    /// An explicit integer priority. The raw token text is preserved so
    /// the type checker can validate the numeric range against the target.
    Numeric(String),
}

/// An `#interface Name { effect sig; effect sig; }` declaration (Decision #16).
///
/// Slice-6 scope: name + body of effect signatures (signatures only, no
/// bodies — those land in `#impl` per Decision #16). Implementation method
/// bodies are still slice 7 (statement parsing).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InterfaceDecl {
    /// The interface's name.
    pub name: String,
    /// Generic parameters in declaration order. Empty for non-generic
    /// interfaces (the common case).
    pub generic_params: Vec<GenericParam>,
    /// Effect signatures the interface requires. Each `effect name(params)
    /// -> ret;` is one entry. Empty body interfaces are valid (rare in
    /// practice; useful as marker traits).
    pub methods: Vec<InterfaceMethod>,
    /// Source span covering the full declaration.
    pub span: Span,
}

/// One `effect name(params) -> ret;` entry inside an `#interface` body.
///
/// The implicit `#mutates: [self]` per Decision #16 Rule 1 is not stored
/// on the AST node — it's restored at `clifford-resolve` / `clifford-effect`
/// time when the interface is monomorphized against a concrete automaton.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InterfaceMethod {
    /// Method name.
    pub name: String,
    /// Value parameters in source order.
    pub params: Vec<Param>,
    /// Optional return type. `None` ⇒ unit.
    pub return_type: Option<TypeExpr>,
    /// Source span covering the full signature including the trailing `;`.
    pub span: Span,
}

/// An `@trait Name { method_sigs }` declaration (§4.5).
///
/// Per Decision #2 (hybrid trait scheme), traits declare method signatures;
/// satisfaction is structural — any `@type` with matching method signatures
/// satisfies the trait without needing an explicit `impl` block. Optional
/// explicit `impl Trait for Type` form is reserved for v0.2.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TraitDecl {
    /// The trait's name.
    pub name: String,
    /// Generic parameters in declaration order. Empty for non-generic
    /// traits (the common case).
    pub generic_params: Vec<GenericParam>,
    /// Method signatures the trait requires.
    pub methods: Vec<TraitMethod>,
    /// Source span covering the full declaration.
    pub span: Span,
}

/// One `@fn name(params) -> ret $ [TraitList];` entry inside an `@trait` body.
///
/// Per §4.5, trait declarations contain only method signatures — no default
/// bodies in v0.1.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TraitMethod {
    /// Method name.
    pub name: String,
    /// Value parameters in source order.
    pub params: Vec<Param>,
    /// Optional return type.
    pub return_type: Option<TypeExpr>,
    /// `$ [TraitList]` markers attached to the method signature.
    pub trait_list: Vec<TraitRef>,
    /// Source span covering the full signature including the trailing `;`.
    pub span: Span,
}

/// An `#impl Interface for Automaton { … }` block (Decision #16).
///
/// Slice-2 scope: interface name + automaton name + span. Method bodies
/// (the `effect name(params) -> ret { … }` items inside the braces) arrive
/// in slice 3 alongside body parsing.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ImplDecl {
    /// The interface being implemented.
    pub interface_name: String,
    /// The automaton implementing the interface.
    pub automaton_name: String,
    /// Source span covering the full declaration.
    pub span: Span,
}

/// A `#test "description" { … }` block (Decision #7).
///
/// Slice-2 scope: description string + span. Test bodies arrive when
/// statement parsing lands in slice 4. Each test runs in isolation;
/// automata are reset to their declared initial state before each
/// invocation (semantic detail enforced at runtime, not at parse).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TestDecl {
    /// Test description from the string literal.
    pub description: String,
    /// Source span covering the full declaration.
    pub span: Span,
}

/// An `@type Name<T> = …;` declaration (§2.3).
///
/// The body is either a type alias (single `TypeExpr`) or an algebraic data
/// type (sum-of-variants). Generic parameters are optional (`Vec` is empty
/// for monomorphic declarations).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TypeDecl {
    /// The type's name.
    pub name: String,
    /// Generic parameters in declaration order. Empty for non-generic types.
    pub generic_params: Vec<GenericParam>,
    /// Either an alias body (`= TypeExpr`) or an ADT body (`= | A | B(T) | C { f: T }`).
    pub body: TypeBody,
    /// Source span covering `@type Name<…> = …;` end-to-end.
    pub span: Span,
}

/// The right-hand side of an `@type` declaration.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TypeBody {
    /// `@type Foo = u32;` — a type alias.
    Alias(TypeExpr),
    /// `@type Foo = | A | B(T) | C { f: T };` — an algebraic data type.
    /// Always at least one variant; the leading `|` in source is optional
    /// per §2.3 grammar but the AST does not preserve whether it was present.
    Adt(Vec<Variant>),
}

/// A variant in an ADT body (§2.3).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Variant {
    /// Variant name.
    pub name: String,
    /// Variant data: none (unit-like), tuple-style, or struct-style.
    pub data: VariantData,
    /// Source span covering the whole variant (`Name`, `Name(T1, T2)`, or
    /// `Name { f1: T1, f2: T2 }`).
    pub span: Span,
}

/// The data carried by an ADT variant.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum VariantData {
    /// `Name` — unit-like variant with no payload.
    None,
    /// `Name(T1, T2, …)` — tuple-style variant. Always at least one type;
    /// for zero types use `VariantData::None` (don't write `Name()`).
    Tuple(Vec<TypeExpr>),
    /// `Name { f1: T1, f2: T2, … }` — struct-style variant.
    Struct(Vec<Field>),
}

/// A named, typed field — used in struct-style ADT variants and (later, when
/// `#automaton` member parsing lands) in automaton field declarations.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Field {
    /// Field name.
    pub name: String,
    /// Field type.
    pub ty: TypeExpr,
    /// Source span covering `name: type`.
    pub span: Span,
}

/// A generic parameter declaration: `T` or `T: Pure + Readable`.
///
/// Per §2.2 the spec form is `ident (':' trait_bound)?` where `trait_bound`
/// is a sequence of trait references separated by `+`. We use [`TraitRef`]
/// directly for the bounds rather than the spec's `type_expr` form because
/// non-trait types in bound position are nonsense; the type checker would
/// reject them anyway, so the parser rejects them here instead.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GenericParam {
    /// Parameter name.
    pub name: String,
    /// Trait bounds. Empty for unbounded parameters.
    pub bounds: Vec<TraitRef>,
    /// Source span covering `name (: bound + bound)?` end-to-end.
    pub span: Span,
}

/// An `@sequential(AutomatonA, AutomatonB);` top-level attribute (Decision #11).
///
/// Asserts to the GA orthogonality engine (§7.3) that the two named
/// automata never run concurrently. Symmetric: `@sequential(A, B)` and
/// `@sequential(B, A)` carry the same meaning. The attribute is *trusted*
/// — the compiler does not verify it, just consumes it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SequentialAttr {
    /// First automaton in the pair.
    pub a: String,
    /// Second automaton in the pair.
    pub b: String,
    /// Source span covering `@sequential(A, B);` end-to-end.
    pub span: Span,
}

// ─── Type expressions (§2.7) ─────────────────────────────────────────────────

/// A type expression — anywhere a type can appear (function parameters,
/// return types, struct/automaton field types, ADT variant payloads, type
/// aliases, etc.).
///
/// `TypeExpr` is recursive (types contain types — `&T`, `[T; N]`,
/// `(T1, T2)`, `@fn(T) -> T`, `access<T>`, `Path<T>`), so the recursive
/// arms wrap their inner [`TypeExpr`] in `Box` to keep the enum size
/// finite. Wrapper structs hold the children to keep `TypeKind` flat.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TypeExpr {
    /// What kind of type this is.
    pub kind: TypeKind,
    /// Source span covering the whole type expression.
    pub span: Span,
}

/// A type-expression variant. Mirrors §2.7 of `CLIFFORD_SPEC.md`.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum TypeKind {
    /// `()` — the unit type.
    Unit,
    /// One of the predeclared primitive types from §4.1.
    Primitive(PrimitiveType),
    /// A path-based type: `Counter`, `Result<T, E>`, `clifford::core::Option<T>`.
    /// Single-segment paths whose name doesn't match a primitive are also
    /// represented here (the resolver decides at type-check time whether the
    /// path resolves to a `@type`, an `#automaton`, etc.).
    Path(PathType),
    /// `&T` or `&mut T`. References are body-scoped per Decision #13;
    /// occurrences in return positions or field positions are caught at
    /// §5.7 (`E0702` / `E0703`).
    Ref(RefType),
    /// `access<T>` (read-write) or `access const<T>` (read-only) — Decision #19
    /// nominal access types. Each `@type` declaration of an access type
    /// produces a distinct nominal identity even when their underlying
    /// representation is congruent.
    Access(AccessType),
    /// `[T; N]` — fixed-size array, stack-allocated.
    Array(ArrayType),
    /// `[T]` — slice (fat pointer: `(ptr, len)`).
    Slice(SliceType),
    /// `(T1, T2, …)` — tuple. Per §2.7 grammar requires ≥ 2 elements;
    /// `(T)` is just a parenthesised type (not a 1-tuple) and `()` is `Unit`.
    Tuple(TupleType),
    /// `@fn(T1, T2) -> T3 $ [Trait, …]` — function-pointer type. The trait
    /// list is part of the type's identity per §2.7 / Decision #2.
    Fn(FnType),
}

/// Predeclared primitive types from §4.1.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum PrimitiveType {
    /// `u8`
    U8,
    /// `u16`
    U16,
    /// `u32`
    U32,
    /// `u64`
    U64,
    /// `usize` (target pointer width)
    Usize,
    /// `i8`
    I8,
    /// `i16`
    I16,
    /// `i32`
    I32,
    /// `i64`
    I64,
    /// `isize` (target pointer width)
    Isize,
    /// `f32`
    F32,
    /// `f64`
    F64,
    /// `bool` (1 bit logically; stored as 8)
    Bool,
    /// `char` (Unicode scalar value; 32-bit)
    Char,
}

/// A path-based type with optional generic arguments.
///
/// `segments` holds the `::`-separated parts, e.g. `clifford::core::Option`
/// becomes `["clifford", "core", "Option"]`. The vector is always non-empty.
/// `generic_args` is empty for non-generic paths.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PathType {
    /// `::`-separated segments, leftmost first. Always at least one segment.
    pub segments: Vec<String>,
    /// `<T1, T2, …>` arguments after the path. Empty for non-generic paths.
    pub generic_args: Vec<TypeExpr>,
}

/// A reference type `&T` (immutable) or `&mut T`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RefType {
    /// `true` for `&mut T`, `false` for `&T`.
    pub mutable: bool,
    /// The referenced type.
    pub inner: Box<TypeExpr>,
}

/// A nominal access type `access<T>` (read-write) or `access const<T>` (Decision #19).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AccessType {
    /// `true` for `access const<T>`, `false` for `access<T>`.
    pub is_const: bool,
    /// The pointee type.
    pub inner: Box<TypeExpr>,
}

/// A fixed-size array type `[T; N]`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ArrayType {
    /// Element type.
    pub element: Box<TypeExpr>,
    /// Array length.
    pub size: ArraySize,
}

/// The size of an array type. v0.1 only supports integer literals here
/// (`[u8; 64]`); generic-parameter sizes (`[T; N]` where `N` is a const
/// generic) and full const expressions are deferred to subsequent slices.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ArraySize {
    /// Raw integer-literal text from the lexer (e.g. `"64"`, `"1_000"`).
    /// The type checker validates the numeric value against `usize::MAX`.
    IntLiteral(String),
}

/// A slice type `[T]`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SliceType {
    /// Element type.
    pub element: Box<TypeExpr>,
}

/// A tuple type `(T1, T2, …)` with ≥ 2 elements.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TupleType {
    /// Element types in source order. Always at least 2 elements per §2.7.
    pub elements: Vec<TypeExpr>,
}

/// A function-pointer type `@fn(T1, T2) -> T3 $ [Trait, …]`.
///
/// Per §2.7 / Decision #2, two function-pointer types differing only in
/// trait list are distinct types. Assigning a `$ [Readable]` `@fn` to a
/// slot expecting `$ [Pure]` is a type error.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FnType {
    /// Parameter types in declaration order.
    pub params: Vec<TypeExpr>,
    /// Return type, or `None` for `@fn(...)` with no `-> T` (which means
    /// returns `Unit`; preserved as `None` to round-trip exactly).
    pub return_type: Option<Box<TypeExpr>>,
    /// `$ [TraitList]` markers attached to the function-pointer type.
    /// Empty if no `$ [...]` appears.
    pub trait_list: Vec<TraitRef>,
}

/// A reference to a trait (in a trait-list, in a generic bound, or in an
/// `#impl Interface for Automaton` clause).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TraitRef {
    /// Trait name (single segment for now; multi-segment paths arrive when
    /// the module system lands).
    pub name: String,
    /// Generic arguments to the trait, if any (e.g., `Iterator<Item = T>`
    /// would be `Iterator<T>` in v0.1's simpler form).
    pub generic_args: Vec<TypeExpr>,
    /// Source span covering `Name` or `Name<T1, T2>` end-to-end.
    pub span: Span,
}

// ─── Expressions and statements (§2.6) ───────────────────────────────────────

/// A value expression.
///
/// Implements §2.6 of `CLIFFORD_SPEC.md` — the full expression grammar.
/// Recursive (expressions contain expressions), so the recursive arms wrap
/// their children in `Box`. The variant is tagged on `ExprKind`; `Expr`
/// itself adds the source span shared by every node.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Expr {
    /// What kind of expression this is.
    pub kind: ExprKind,
    /// Source span covering the whole expression.
    pub span: Span,
}

/// An expression variant.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum ExprKind {
    /// Decimal integer literal (raw text, with optional type suffix).
    IntLit(String),
    /// Hex integer literal `0x…`.
    HexLit(String),
    /// Binary integer literal `0b…`.
    BinLit(String),
    /// Float literal.
    FloatLit(String),
    /// `'X'` character literal.
    CharLit(char),
    /// `b'X'` byte literal (Decision #19).
    ByteLit(u8),
    /// `"…"` string literal.
    StringLit(String),
    /// `true` / `false`.
    BoolLit(bool),
    /// `null` — context-typed null access literal (Decision #19).
    Null,

    /// A name or `::`-separated path: `foo`, `Result::Ok`,
    /// `clifford::core::option`. The parser produces this for both single
    /// idents and multi-segment paths; the resolver decides whether the
    /// path resolves to a binding, an automaton state (Refinement #5d
    /// `Auto::Name`), a constructor, or something else.
    Path(Vec<String>),

    /// `Auto@state` — read the current state-tag of an automaton
    /// (Refinement #5d). The parser captures the automaton name; the
    /// resolver verifies it refers to an in-scope multi-state automaton.
    StateRead(String),

    /// `@snapshot Auto.field` — boundary-crossing read operator per
    /// Decision #24 / ADR 0004. Yields an *owned copy* of the named
    /// field's current value at the snapshot site, on the pure side.
    /// The parser captures the automaton name and field name; the
    /// resolver verifies both exist; `clifford-check` enforces the
    /// `Readable` capability (#`-layer only in v0.2; ADR 0003's
    /// `Readable` row gates `@fn` access in v0.4+).
    ///
    /// v0.2-α scope (this slice): parser produces the AST node
    /// only; downstream checks (atomicity, lock-holding, row
    /// gating) come online in subsequent v0.2 slices.
    Snapshot {
        /// The automaton being read from (single segment; `Self.field`
        /// inside transitions is rejected as `E0553` per ADR 0004 Q2).
        automaton: String,
        /// The field being read.
        field: String,
    },

    /// Parenthesised single expression. Distinguished from a 1-tuple
    /// (which doesn't exist; use `Tuple(vec![one])` only for ≥ 2 elements).
    /// Preserved so round-tripping reproduces source.
    Paren(Box<Expr>),

    /// Tuple expression `(a, b, c)`. Always at least 2 elements; the
    /// 1-element case is a `Paren`.
    Tuple(Vec<Expr>),

    /// Array literal `[a, b, c]`.
    Array(Vec<Expr>),

    /// Array repeat literal `[expr; count]`.
    ArrayRepeat {
        /// The repeated value.
        value: Box<Expr>,
        /// The number of repetitions; must be a const-evaluable expression
        /// (the type checker enforces this).
        count: Box<Expr>,
    },

    /// `obj.field` — field access (no method call; that's `MethodCall`).
    FieldAccess {
        /// The receiver expression.
        obj: Box<Expr>,
        /// The field name.
        field: String,
    },

    /// `obj[index]` — indexing.
    Index {
        /// The collection expression.
        obj: Box<Expr>,
        /// The index expression.
        index: Box<Expr>,
    },

    /// `f(a, b, …)` — function or callable invocation.
    Call {
        /// The callee expression (typically a path).
        callee: Box<Expr>,
        /// Argument expressions in source order.
        args: Vec<Expr>,
    },

    /// `obj.method(a, b, …)` — method call.
    MethodCall {
        /// The receiver expression.
        obj: Box<Expr>,
        /// The method name.
        method: String,
        /// Argument expressions in source order.
        args: Vec<Expr>,
    },

    /// Prefix unary: `-x`, `!x`, `~x`, `*x`.
    Unary {
        /// Which prefix operator.
        op: UnaryOp,
        /// The operand.
        operand: Box<Expr>,
    },

    /// Borrow expression: `&x` (immutable) or `&mut x`. Per Decision #13
    /// Rule 0, `&mut` of an automaton field is rejected by `clifford-check`,
    /// not by the parser — we accept it and let the later phase reject.
    Ref {
        /// `true` for `&mut x`.
        mutable: bool,
        /// The operand being borrowed.
        operand: Box<Expr>,
    },

    /// Binary expression: `lhs op rhs`.
    Binary {
        /// Which binary operator.
        op: BinaryOp,
        /// Left operand.
        lhs: Box<Expr>,
        /// Right operand.
        rhs: Box<Expr>,
    },

    /// `expr as Type` — cast expression.
    Cast {
        /// The value being cast.
        value: Box<Expr>,
        /// The target type.
        ty: TypeExpr,
    },

    /// `lo..hi` (half-open) or `lo..=hi` (inclusive). For sigma loops
    /// (Decision #14), both endpoints are typically present; the
    /// open-ended forms `..hi` / `lo..` / `..` are not yet supported.
    Range {
        /// Lower bound.
        lo: Box<Expr>,
        /// Upper bound.
        hi: Box<Expr>,
        /// `true` for `..=`, `false` for `..`.
        inclusive: bool,
    },

    /// `#unchecked_load<T>(p)` — Decision #17 narrow unsafe primitive.
    UncheckedLoad {
        /// The element type being loaded.
        ty: TypeExpr,
        /// The access pointer expression.
        ptr: Box<Expr>,
    },

    /// `#volatile_load<T>(p)` — Decision #17.
    VolatileLoad {
        /// The element type being loaded.
        ty: TypeExpr,
        /// The access pointer expression.
        ptr: Box<Expr>,
    },

    /// `#unchecked_cast<S, T>("reason", value)` — Decision #17 + Refinement #19a.
    /// The mandatory reason string is preserved on the AST and emitted to the
    /// audit log by `cliffordc audit --list-unsafe`.
    UncheckedCast {
        /// Source type.
        from_ty: TypeExpr,
        /// Target type.
        to_ty: TypeExpr,
        /// Mandatory non-empty reason string per Refinement #19a.
        reason: String,
        /// The value being cast.
        value: Box<Expr>,
    },

    /// `#unchecked_offset<T>(p, n)` — Decision #19 pointer arithmetic.
    UncheckedOffset {
        /// The element type.
        ty: TypeExpr,
        /// The base access pointer.
        ptr: Box<Expr>,
        /// The element-count offset (signed).
        n: Box<Expr>,
    },
}

/// Prefix unary operators.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum UnaryOp {
    /// `-x` arithmetic negation
    Neg,
    /// `!x` logical not
    Not,
    /// `~x` bitwise not
    BitNot,
    /// `*x` dereference (only meaningful on raw pointer-shaped values;
    /// for narrow primitives use `#unchecked_load` / `#volatile_load`)
    Deref,
}

/// Binary operators, grouped by category.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum BinaryOp {
    /// `||`
    Or,
    /// `&&`
    And,
    /// `==`
    Eq,
    /// `!=`
    Ne,
    /// `<`
    Lt,
    /// `<=`
    Le,
    /// `>`
    Gt,
    /// `>=`
    Ge,
    /// `|`
    BitOr,
    /// `^`
    BitXor,
    /// `&`
    BitAnd,
    /// `<<`
    Shl,
    /// `>>`
    Shr,
    /// `+`
    Add,
    /// `-`
    Sub,
    /// `*`
    Mul,
    /// `/`
    Div,
    /// `%`
    Rem,
}

// ─── Statements (§2.6) ───────────────────────────────────────────────────────

/// A statement in a block body.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Stmt {
    /// What kind of statement this is.
    pub kind: StmtKind,
    /// Source span covering the whole statement.
    pub span: Span,
}

/// A statement variant.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum StmtKind {
    /// `let mut? name (: T)? = expr;` — explicit-form binding.
    Let {
        /// `true` for `let mut`.
        mutable: bool,
        /// Binding name.
        name: String,
        /// Optional type annotation.
        ty: Option<TypeExpr>,
        /// Initialiser expression.
        value: Expr,
    },

    /// `let name := expr;` — short-binding form (Decision #8). Type is
    /// always inferred; no explicit annotation, no `mut`.
    LetShort {
        /// Binding name.
        name: String,
        /// Initialiser expression.
        value: Expr,
    },

    /// `expr;` — expression statement.
    Expr(Expr),

    /// `return expr?;`
    Return(Option<Expr>),

    /// `#mutate Auto { f1 = e1, f2 = e2 };` — bulk-write to automaton fields.
    Mutate {
        /// The automaton being mutated.
        automaton: String,
        /// Field assignments in source order.
        assigns: Vec<FieldAssign>,
    },

    /// `Auto.field <op>= expr;` — single-field mutation sugar (Decision #15).
    /// Desugars to `Mutate` semantically but preserved as a distinct variant
    /// so round-tripping reproduces source.
    MutateShort {
        /// The automaton being mutated.
        automaton: String,
        /// The single field being assigned.
        field: String,
        /// Compound assignment operator (`Eq` for plain `=`, others for
        /// `+=` / `-=` / etc.).
        op: AssignOp,
        /// Right-hand side.
        value: Expr,
    },

    /// `#> name(args);` — effect-procedure call (Decision #3).
    /// CallContext (transition vs identity vs generic per Refinement #5b)
    /// is determined by `clifford-resolve`, not the parser.
    ProcCall {
        /// Callee name (single ident; `Interface::method` form for
        /// generic-context calls per Decision #16 lands in slice 8).
        name: String,
        /// Argument expressions.
        args: Vec<Expr>,
    },

    /// `#unchecked_store<T>(ptr, value);` — Decision #17 unsafe-store
    /// primitive (statement form; the load form is an expression).
    UncheckedStore {
        /// Element type.
        ty: TypeExpr,
        /// Access pointer.
        ptr: Expr,
        /// Value being stored.
        value: Expr,
    },

    /// `#volatile_store<T>(ptr, value);`
    VolatileStore {
        /// Element type.
        ty: TypeExpr,
        /// Access pointer.
        ptr: Expr,
        /// Value being stored.
        value: Expr,
    },

    /// `sigma <var> in <range_expr> { body }` — Decision #14 / §5.8
    /// bounded-iteration loop. The range source is a half-open
    /// (`lo..hi`) or inclusive (`lo..=hi`) range. The loop variable
    /// is bound only inside `body` with the implicit refinement type
    /// `bounded<lo, hi>` per §5.8.
    ///
    /// v0.1 scope (this slice): single-ident pattern + range source
    /// only. The `(index, value)` pattern and array-source
    /// (`sigma x in &arr`) forms in §5.8 land in subsequent slices
    /// once slice-indexing infrastructure is in place.
    Sigma {
        /// The loop variable's source name.
        var: String,
        /// The range source — typed as `ExprKind::Range` after
        /// parsing. Stored as `Expr` rather than the narrowed
        /// `RangeExpr` so future array-source forms can drop in
        /// without an enum change.
        source: Expr,
        /// Body block; runs once per iteration.
        body: Block,
    },

    /// `name = expr;` — local mutable re-assignment.
    ///
    /// Distinct from `Mutate` / `MutateShort` which target
    /// automaton fields (`Auto.field = …`). This variant is for
    /// plain locals that were declared with `let mut`. The
    /// resolver enforces that `name` resolves to a `let mut`
    /// binding — re-assigning a `let` (immutable) or a parameter
    /// without `mut` is rejected with an `E0410`-shaped error.
    ///
    /// At codegen, every `let mut` binding lives in a stack slot
    /// (`alloca`) so re-assignment is a `store`; reads through
    /// `ExprKind::Path` become `load`s. Immutable `let` bindings
    /// keep their slice-1 SSA-direct lowering — the alloca is
    /// only emitted when the binding is `mut`.
    ///
    /// v0.1 scope: single-ident LHS only. Tuple destructuring and
    /// field-of-local assignment (`local.field = …`) are deferred
    /// to later slices.
    Assign {
        /// The local being re-assigned.
        name: String,
        /// Right-hand side.
        value: Expr,
    },
}

/// One `field = expr` (or `field[index] = expr`) inside a `#mutate Auto { … }`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FieldAssign {
    /// Field name.
    pub field: String,
    /// Optional index (for `field[i] = expr` forms).
    pub index: Option<Expr>,
    /// Right-hand side.
    pub value: Expr,
    /// Source span.
    pub span: Span,
}

/// Assignment operators for the single-field mutation sugar (Decision #15).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum AssignOp {
    /// `=`
    Eq,
    /// `+=`
    PlusEq,
    /// `-=`
    MinusEq,
    /// `*=`
    StarEq,
    /// `/=`
    SlashEq,
    /// `%=`
    PercentEq,
    /// `&=`
    AmpEq,
    /// `|=`
    PipeEq,
    /// `^=`
    CaretEq,
    /// `<<=`
    ShlEq,
    /// `>>=`
    ShrEq,
}

/// A block of statements.
///
/// Per §2.6: `block := '{' stmt* expr? '}'`. The optional trailing
/// expression is the block's value when it appears in expression position
/// (e.g., as the body of a function returning a value).
///
/// Slice-7 scope: only the leading `stmt*` portion is parsed; the trailing
/// expression form is treated as `Stmt::Expr(...)` requiring a terminating
/// `;` for now. Tail-expression-as-block-value lands when control-flow
/// expressions (`if`, `match`) need it.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Block {
    /// Statements in source order.
    pub stmts: Vec<Stmt>,
    /// Source span covering `{ … }`.
    pub span: Span,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn layer_predicates() {
        assert!(Layer::Functional.is_functional());
        assert!(!Layer::Functional.is_imperative());
        assert!(Layer::Imperative.is_imperative());
        assert!(!Layer::Imperative.is_functional());
    }

    #[test]
    fn empty_program() {
        let p = Program::default();
        assert!(p.items.is_empty());
        assert_eq!(p.span.start, 0);
        assert_eq!(p.span.end, 0);
    }

    #[test]
    fn item_layer_is_derived_from_variant() {
        let f = Item::Fn(FnDecl {
            name: "foo".into(),
            params: Vec::new(),
            return_type: None,
            trait_list: Vec::new(),
            partial: false,
            body: Block::default(),
            span: Span::new(0, 10),
        });
        assert_eq!(f.layer(), Layer::Functional);

        let a = Item::Automaton(AutomatonDecl {
            name: "Bar".into(),
            address: None,
            basis: None,
            states: None,
            fields: Vec::new(),
            transitions: Vec::new(),
            span: Span::new(0, 14),
        });
        assert_eq!(a.layer(), Layer::Imperative);
    }
}
