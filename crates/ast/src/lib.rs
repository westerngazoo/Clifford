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
            Self::Fn(_) | Self::Type(_) | Self::Sequential(_) => Layer::Functional,
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

/// An `@fn name(params) -> T $ [TraitList] { … }` declaration.
///
/// Slice-4 scope: name, value parameters, optional return type, optional
/// trait list, span. Generic parameters, where-clause, extern modifier,
/// and body content arrive in subsequent slices.
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
    /// Source span covering `@fn name(params) -> T $ [...] { }` end-to-end.
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
/// First-slice scope: name + span only. `#address` register-block annotation
/// (Decision #6), `#basis` clause (Decision #4), `#states` list, automaton
/// fields, named transitions (Refinement #5b) all arrive in subsequent
/// slices.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AutomatonDecl {
    /// The automaton's name.
    pub name: String,
    /// Source span covering `#automaton Name { }` end-to-end.
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

/// An `#interface Name { … }` declaration (Decision #16).
///
/// Slice-2 scope: name + span only. The interface body — a list of effect
/// signatures `effect name(params) -> ret;` — arrives in slice 3 alongside
/// parameter and return-type parsing.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InterfaceDecl {
    /// The interface's name.
    pub name: String,
    /// Source span covering the full declaration.
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
            span: Span::new(0, 10),
        });
        assert_eq!(f.layer(), Layer::Functional);

        let a = Item::Automaton(AutomatonDecl {
            name: "Bar".into(),
            span: Span::new(0, 14),
        });
        assert_eq!(a.layer(), Layer::Imperative);
    }
}
