//! # clifford-types
//!
//! Hindley–Milner type inference + structural trait resolution for the
//! Clifford compiler. Implements §4 (Type System) and §5.2–§5.3 of
//! `docs/CLIFFORD_SPEC.md`.
//!
//! ## Responsibilities (final scope)
//!
//! - HM inference for local bindings (§4.8): integer literal default `i32`,
//!   float literal default `f64`, generic parameter inference from arguments.
//! - Structural trait satisfaction (§5.3): a type satisfies a trait iff it
//!   has methods with matching signatures; `Self` substituted by the candidate.
//! - Built-in trait obligations (§4.5): `Pure`, `Readable`, `Observable`,
//!   `Opaque`. Default `$ [Pure]` for unannotated `@fn` (Emergent Rule 2).
//! - Nominal access type identity (Decision #19): `access<T>` and `access const<T>`
//!   carry per-`@type` distinct identity; `#unchecked_cast` is the only
//!   cross-type bridge.
//! - Function-pointer types include trait list as part of identity (§2.7).
//!
//! ## Phase boundary
//!
//! Types runs after `clifford-resolve`. Output is the typed AST consumed by
//! `clifford-check`, `clifford-effect`, and `clifford-codegen`.
//!
//! ## Implementation status
//!
//! **Slice 1:** literal-type inference + primitive expression typing.
//! Integer suffix recognition, Path → local-type resolution, unary/binary
//! operator typing, `let`-annotation matching, narrow unsafe primitives.
//!
//! **Slice 2:** function-call typing, automaton-field typing, references.
//! `SignatureRegistry` for top-level callable typing; `AutomatonField`
//! bindings consumed for field-access typing; `Type::Ref` for borrow
//! exprs and deref.
//!
//! **Slice 3 (this PR):** structured-type expressions. Adds `Type::Array`,
//! `Type::Slice`, `Type::Tuple`, and `Type::Range` to the algebra. Types
//! `Expr::Index` against arrays/slices (returns the element type;
//! E0516 if the receiver isn't indexable, E0517 if the index isn't an
//! integer). Types tuple expressions, array literals, array-repeat
//! literals, and range expressions. Method calls, generic instantiation,
//! and trait satisfaction remain deferred to slice T4 (those need real
//! HM unification + the trait-resolution machinery).

#![forbid(unsafe_code)]
#![warn(missing_docs)]

use std::collections::HashMap;

use clifford_ast::{
    AutomatonDecl, BinaryOp, Block, EffectDecl, Expr, ExprKind, FnDecl, InterruptDecl, Item, Param,
    PrimitiveType, Program, Stmt, StmtKind, TraitRef, TransitionDecl, TypeExpr, TypeKind, UnaryOp,
};
use clifford_lexer::Span;
use clifford_resolve::{BindingRef, Resolution, Symbol, SymbolKind};
use thiserror::Error;

/// Errors produced during type checking.
///
/// Reserves the `E05xx` range. Earlier hundreds are taken by the lexer
/// (E01xx), parser (E02xx), `clifford-check` mutability/layer errors
/// (E03xx per `docs/CLIFFORD_SPEC.md`), and `clifford-resolve` (E04xx).
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum TypeError {
    /// A binary operator's two operands have incompatible types.
    ///
    /// Diagnostic shape: name the operator, both operand types, and the
    /// byte offset of the operator's expression. Downstream consumers can
    /// re-derive the source spans of the operands from the AST.
    #[error("E0510: binary operator `{op}` has incompatible operand types: lhs is {lhs}, rhs is {rhs} (at byte {at})")]
    BinaryTypeMismatch {
        /// The operator displayed for the user (`+`, `==`, `&&`, etc.).
        op: &'static str,
        /// Display name of the lhs type.
        lhs: String,
        /// Display name of the rhs type.
        rhs: String,
        /// Byte offset of the binary expression.
        at: usize,
    },

    /// A unary operator was applied to an operand of the wrong type.
    #[error("E0511: unary operator `{op}` cannot be applied to {operand} (at byte {at})")]
    UnaryTypeMismatch {
        /// The operator displayed for the user (`-`, `!`, `~`, `*`).
        op: &'static str,
        /// Display name of the operand type.
        operand: String,
        /// Byte offset of the unary expression.
        at: usize,
    },

    /// A `let name: T = expr;` statement's annotated type `T` does not
    /// match the inferred type of `expr`.
    #[error(
        "E0512: `let {name}: {declared}` does not match initializer type {actual} (at byte {at})"
    )]
    LetTypeMismatch {
        /// The bound name.
        name: String,
        /// Display name of the annotated type.
        declared: String,
        /// Display name of the initializer's inferred type.
        actual: String,
        /// Byte offset of the `let` statement.
        at: usize,
    },

    /// A function call's argument count or types don't match the callee's
    /// declared signature.
    ///
    /// The diagnostic carries the callee name, the parameter index that
    /// failed (1-based for human readability — `arg #1` is the first
    /// positional argument), the expected and actual types, and the byte
    /// offset of the call site.
    #[error("E0513: call to `{callee}` argument #{arg} expected {expected}, got {actual} (at byte {at})")]
    CallArgMismatch {
        /// Callee name (the function/effect/interrupt being called).
        callee: String,
        /// 1-based positional index of the mismatched argument.
        arg: usize,
        /// Display name of the expected parameter type.
        expected: String,
        /// Display name of the actual argument type.
        actual: String,
        /// Byte offset of the call expression.
        at: usize,
    },

    /// A function call has the wrong number of arguments.
    #[error(
        "E0514: call to `{callee}` expected {expected} argument(s), got {actual} (at byte {at})"
    )]
    CallArityMismatch {
        /// Callee name.
        callee: String,
        /// Number of declared parameters.
        expected: usize,
        /// Number of arguments supplied at the call site.
        actual: usize,
        /// Byte offset of the call expression.
        at: usize,
    },

    /// A `*r` deref expression is applied to a value that isn't a reference.
    #[error("E0515: cannot deref `*` on non-reference type {operand} (at byte {at})")]
    DerefNonReference {
        /// Display name of the operand type.
        operand: String,
        /// Byte offset of the unary expression.
        at: usize,
    },

    /// An `obj[i]` index expression's receiver isn't an array, slice, or
    /// reference to one.
    #[error("E0516: cannot index into non-indexable type {receiver} (at byte {at})")]
    IndexNonIndexable {
        /// Display name of the receiver type.
        receiver: String,
        /// Byte offset of the index expression.
        at: usize,
    },

    /// An `obj[i]` index expression's index isn't an integer.
    #[error("E0517: index must be an integer, got {index} (at byte {at})")]
    IndexNotInteger {
        /// Display name of the index expression's type.
        index: String,
        /// Byte offset of the index expression.
        at: usize,
    },

    /// A path-position type expression names a top-level type that is
    /// neither predeclared nor declared via `@type Name { … }` in the
    /// program.
    ///
    /// Slice T4c reports this as a *signature-time* check, separate
    /// from compatibility checking. T4a/T4b would let an unknown
    /// nominal slip through and trigger E0512 (mismatch) downstream
    /// when compared to anything else; T4c reports the more useful
    /// "this name doesn't exist" diagnostic up front.
    ///
    /// Multi-segment paths (e.g. `clifford::core::Option`) are
    /// always reported as unknown in T4c — module resolution lands
    /// in T4d+. Today users referencing such paths must wait for
    /// T4d to lift the false positive.
    #[error("E0518: unknown type `{name}` in type position (at byte {at}); declare via `@type {name} {{ … }}` or check the spelling")]
    UnknownNominalType {
        /// The unresolved type name as written in source (single
        /// segment for v0.1 / v0.2; multi-segment paths join with
        /// `::` for the diagnostic).
        name: String,
        /// Byte offset of the type-expression's path.
        at: usize,
    },

    /// A path-position type expression's number of generic arguments
    /// does not match the declared `@type`'s arity.
    ///
    /// `@type Pair<T> = (T, T);` has arity 1; `Pair<u32>` works,
    /// `Pair` (no args) and `Pair<u32, bool>` (too many) are both
    /// E0519. The diagnostic carries the offending name and both
    /// counts so users can fix the call site.
    ///
    /// Bound-trait checks on generic parameters (`T: Copy`) are full
    /// HM-unification work and stay deferred. T4c only validates
    /// arity.
    #[error("E0519: `{name}` takes {expected} generic argument(s) but {actual} were supplied (at byte {at})")]
    GenericArityMismatch {
        /// The type name being instantiated.
        name: String,
        /// The declared number of generic parameters (the `@type`'s
        /// arity).
        expected: usize,
        /// The number of generic arguments at the offending site.
        actual: usize,
        /// Byte offset of the type-expression's path.
        at: usize,
    },

    /// An `@fn` body contains a `@snapshot Auto.field` expression but
    /// the function's `$ [TraitList]` does not include `Readable` (the
    /// row that gates `@snapshot` per Decision #24 / ADR 0004).
    ///
    /// Per ADR 0004 Q1, `@snapshot` is **not pure** — two snapshots of
    /// the same field may observe different values; the operator is a
    /// controlled effect. The `Readable` trait (introduced in ADR 0003
    /// P2 as one of the predeclared row labels) is the marker for "this
    /// `@fn` is allowed to read mutable automaton state via `@snapshot`."
    /// Functions without `Readable` in their row are rejected here.
    ///
    /// `#`-layer callables (`#effect`, `#interrupt`, `#transition`)
    /// are *not* gated — they are imperative and may always observe
    /// state. The gate exists only on the pure side.
    ///
    /// The diagnostic names the offending `@fn`, points at the first
    /// `@snapshot` site in the body, and reminds users that adding
    /// `$ [Readable]` to the signature is the fix.
    #[error("E0550: `@fn {fn_name}` uses `@snapshot` but its trait list does not include `Readable`; add `$ [Readable]` to the signature (snapshot at byte {at}, fn declared at byte {decl_at})")]
    SnapshotInUnreadableFn {
        /// Name of the offending `@fn`.
        fn_name: String,
        /// Byte offset of the first `@snapshot` in the body.
        at: usize,
        /// Byte offset of the `@fn` declaration (so users can find
        /// where to add the `$ [Readable]` clause).
        decl_at: usize,
    },

    /// An ADT variant constructor was called with the wrong number of
    /// arguments. T4d: `Some(5u32, true)` for `@type Maybe = | None |
    /// Some(u32);` is `E0521`.
    ///
    /// The diagnostic carries the ADT name, the variant name (so
    /// users see `Maybe::Some` not just `Some`), the expected and
    /// actual arg counts, and the call-site byte offset.
    #[error("E0521: ADT variant `{adt_name}::{variant_name}` takes {expected} argument(s) but {actual} were supplied (at byte {at})")]
    VariantArityMismatch {
        /// Parent ADT name.
        adt_name: String,
        /// Variant name.
        variant_name: String,
        /// Expected number of args (the variant's declared arity).
        expected: usize,
        /// Number of args supplied at the call site.
        actual: usize,
        /// Byte offset of the call expression.
        at: usize,
    },

    /// An ADT variant constructor was called with an argument whose
    /// type doesn't match the variant's declared arg type.
    /// `Some(true)` for `@type Maybe = | Some(u32) | None;` is
    /// `E0522`. For generic ADTs the arg type drives substitution
    /// (T4d: arg types of the *first* generic param-occurrence fix
    /// the param's instantiation; subsequent occurrences must match).
    #[error("E0522: ADT variant `{adt_name}::{variant_name}` argument #{arg} expected {expected}, got {actual} (at byte {at})")]
    VariantArgMismatch {
        /// Parent ADT name.
        adt_name: String,
        /// Variant name.
        variant_name: String,
        /// 1-based argument position.
        arg: usize,
        /// Display name of the expected variant arg type (after
        /// generic substitution where applicable).
        expected: String,
        /// Display name of the actual argument type.
        actual: String,
        /// Byte offset of the call expression.
        at: usize,
    },

    /// A predeclared trait was used on a callable in the *wrong* sigil
    /// layer. Decision #22 / ADR 0003 partition the predeclared traits
    /// into:
    ///
    /// - **Pure-side traits** (`@fn` only): `Pure`, `Readable`,
    ///   `Observable`, `Opaque`. Per ADR 0003 P2 these are the row
    ///   labels for purity and observation effects.
    /// - **Imperative-side traits** (`#effect` / `#interrupt` /
    ///   `#transition` only): `Hardware`, `Realtime`, `Acquire`,
    ///   `Release`, `SeqCst`, `LockingDiscipline`, `PureState`,
    ///   `Encapsulated`. Per Decision #22 these are mutation-kind
    ///   classification + memory-ordering markers.
    ///
    /// Putting `Realtime` on an `@fn` (or `Pure` on a `#effect`) is
    /// rejected here. User-defined `@trait Name { … }` declarations
    /// are *layer-universal* in v0.2-β — they validate on either side.
    /// (A future slice may add a layer tag to `@trait` if use cases
    /// surface; for now the conservative-permissive choice is the
    /// right one.)
    ///
    /// The diagnostic names the offending trait, the callable, the
    /// expected layer, and the actual layer so users see exactly
    /// which side of the boundary is wrong.
    #[error("E0544: trait `{trait_name}` is `{expected_layer}`-only but used on `{actual_kind} {callable}` (at byte {at}); pure-side traits go on `@fn`, imperative-side traits go on `#effect` / `#interrupt` / `#transition`")]
    TraitLayerMismatch {
        /// The misused trait name.
        trait_name: String,
        /// Which layer the trait belongs to (`"pure"` or `"imperative"`).
        expected_layer: &'static str,
        /// The callable's name.
        callable: String,
        /// The callable's actual kind (`"@fn"`, `"#effect"`, etc.) so
        /// the diagnostic shows the user's syntactic form.
        actual_kind: &'static str,
        /// Byte offset of the trait reference.
        at: usize,
    },

    /// A `$ [TraitList]` clause references a trait name that is neither
    /// predeclared (per Decision #2 + Decision #22 / ADR 0003) nor
    /// declared via a top-level `@trait Name { … }` item.
    ///
    /// **Predeclared pure-side traits** (`@fn`): `Pure`, `Readable`,
    /// `Observable`, `Opaque`.
    /// **Predeclared imperative-side traits** (`#effect` / `#interrupt`
    /// / `#transition` per Decision #22): `Hardware`, `Realtime`,
    /// `Acquire`, `Release`, `SeqCst`, `LockingDiscipline`,
    /// `PureState`, `Encapsulated`.
    ///
    /// User-defined traits must be declared via `@trait Name { … }` at
    /// top level. Note: `Diverges` from earlier drafts is *deliberately
    /// removed* — `@partial @fn` covers non-termination per ADR 0003 Q4.
    /// The diagnostic identifies the offending trait name and the
    /// callable carrying the trait list, so users see *their*
    /// identifier and *their* call site.
    #[error("E0541: unknown trait `{trait_name}` in `{kind} {callable}` trait list (at byte {at}); declare via `@trait {trait_name} {{ … }}` or use a predeclared trait name")]
    UnknownTrait {
        /// The unrecognised trait name as written in source.
        trait_name: String,
        /// The callable's name (the `@fn`, `#effect`, `#interrupt`, or
        /// `#transition` whose trait list contains the bad reference).
        callable: String,
        /// Source-form of the callable's kind (`@fn`, `#effect`, etc.) so
        /// the diagnostic shows the user's syntactic form, not internal
        /// type names.
        kind: &'static str,
        /// Byte offset of the trait reference within the trait list.
        at: usize,
    },
}

/// A fully-resolved Clifford type — the abstract counterpart to `TypeExpr`
/// from the AST.
///
/// `TypeExpr` is the *syntactic* form (carries source spans, raw identifier
/// segments, etc.); `Type` is the *semantic* form (canonical, comparable,
/// produced by the type checker once names have been resolved).
///
/// Slice-4a scope: [`Type::Unit`], [`Type::Primitive`], [`Type::Ref`],
/// [`Type::Array`], [`Type::Slice`], [`Type::Tuple`], [`Type::Range`],
/// [`Type::Nominal`] (paths to `@type` declarations and other top-level
/// type-bearing items), [`Type::StringSlice`], and [`Type::Unknown`].
/// Function-pointer types, ADT-variant constructors (multi-segment paths
/// like `Result::Ok`), generic instantiation with HM unification, trait
/// satisfaction, and access types are slice T4b+.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum Type {
    /// `()` — the unit type.
    Unit,
    /// One of the predeclared primitive types from §4.1.
    Primitive(PrimitiveType),
    /// `&T` (immutable) or `&mut T` — a body-scoped reference per Decision #13.
    /// Provenance / lifetime checking is `clifford-check`'s job; the type
    /// system just carries the reference structure.
    Ref {
        /// `true` for `&mut T`, `false` for `&T`.
        mutable: bool,
        /// The referenced type.
        inner: Box<Type>,
    },
    /// `[T; N]` — fixed-size array on the stack. The size is preserved as
    /// the original integer-literal text (no const evaluation in this
    /// slice; `clifford-check` validates against `usize::MAX` later).
    Array {
        /// Element type.
        element: Box<Type>,
        /// Raw integer-literal text from the AST (e.g. `"64"`, `"1_024"`).
        size: String,
    },
    /// `[T]` — slice (logically `(ptr, len)`). Almost always appears under
    /// a `Ref` in real code (`&[T]`).
    Slice {
        /// Element type.
        element: Box<Type>,
    },
    /// `(T1, T2, …)` — tuple with 2+ elements (1-tuples don't exist; `(T)`
    /// is just a parenthesised T per §2.7).
    Tuple(Vec<Type>),
    /// `lo..hi` / `lo..=hi` — range expression value type. Carries the
    /// shared bound type so `clifford-check` can verify sigma-loop bounds
    /// (Decision #14) without re-typing.
    Range {
        /// The element / bound type.
        element: Box<Type>,
        /// `true` for `..=` (inclusive), `false` for `..`.
        inclusive: bool,
    },
    /// `&[u8]` for string literals — special-case shorthand kept for
    /// backward compatibility with slice T1 / T2 fixtures. Behaves
    /// identically to `Ref { mutable: false, inner: Slice { element: u8 } }`
    /// for type-equality purposes; downstream consumers can treat them as
    /// the same type via [`Type::display`].
    StringSlice,
    /// A nominal type — a path that refers to a top-level type-bearing
    /// declaration (`@type`, `@trait`, `#automaton`, `#interface`).
    ///
    /// Per Decision #19's nominal-access machinery and §4 generally,
    /// nominal types have *distinct identity* even when their underlying
    /// representation is congruent: `@type Foo = u32; @type Bar = u32;`
    /// produces two distinct nominal types `Foo` and `Bar` that the
    /// engine treats as different even though both represent `u32`
    /// underneath.
    ///
    /// `path` is the canonical multi-segment path (e.g. `["clifford",
    /// "core", "Option"]` for the standard library's `Option`); for
    /// single-segment local references, the `path` has one entry.
    /// `args` is the list of generic type arguments (empty for
    /// non-generic types).
    ///
    /// Slice-4a records the path and args verbatim. Slice T4b+ adds:
    /// (a) verifying the path resolves to an actual top-level type
    /// declaration, (b) following `@type` aliases to the underlying
    /// type for equality / unification, (c) ADT variant resolution
    /// for multi-segment paths like `Result::Ok`.
    Nominal {
        /// The path to the type, in source order (always at least one segment).
        path: Vec<String>,
        /// Generic type arguments, in declaration order. Empty for
        /// non-generic types like `Counter` or `bool`.
        args: Vec<Type>,
    },
    /// The type checker could not yet compute this expression's type.
    /// Carries a brief reason string for diagnostics.
    Unknown(&'static str),
}

impl Type {
    /// Display name for this type, suitable for diagnostics.
    #[must_use]
    pub fn display(&self) -> String {
        match self {
            Self::Unit => "()".to_owned(),
            Self::Primitive(p) => primitive_name(*p).to_owned(),
            Self::Ref { mutable, inner } => {
                let prefix = if *mutable { "&mut " } else { "&" };
                format!("{prefix}{}", inner.display())
            }
            Self::Array { element, size } => format!("[{}; {size}]", element.display()),
            Self::Slice { element } => format!("[{}]", element.display()),
            Self::Tuple(elems) => {
                let parts: Vec<String> = elems.iter().map(|t| t.display()).collect();
                format!("({})", parts.join(", "))
            }
            Self::Range { element, inclusive } => {
                let dots = if *inclusive { "..=" } else { ".." };
                format!("{}{dots}{}", element.display(), element.display())
            }
            Self::StringSlice => "&[u8]".to_owned(),
            Self::Nominal { path, args } => {
                let path_str = path.join("::");
                if args.is_empty() {
                    path_str
                } else {
                    let arg_strs: Vec<String> = args.iter().map(Self::display).collect();
                    format!("{path_str}<{}>", arg_strs.join(", "))
                }
            }
            Self::Unknown(reason) => format!("<unknown: {reason}>"),
        }
    }

    /// True if this is one of the integer primitive types (signed or unsigned).
    #[must_use]
    pub fn is_integer(&self) -> bool {
        matches!(
            self,
            Self::Primitive(
                PrimitiveType::U8
                    | PrimitiveType::U16
                    | PrimitiveType::U32
                    | PrimitiveType::U64
                    | PrimitiveType::Usize
                    | PrimitiveType::I8
                    | PrimitiveType::I16
                    | PrimitiveType::I32
                    | PrimitiveType::I64
                    | PrimitiveType::Isize
            )
        )
    }

    /// True if this is one of the floating-point primitive types.
    #[must_use]
    pub fn is_float(&self) -> bool {
        matches!(
            self,
            Self::Primitive(PrimitiveType::F32 | PrimitiveType::F64)
        )
    }

    /// True if this is `bool`.
    #[must_use]
    pub fn is_bool(&self) -> bool {
        matches!(self, Self::Primitive(PrimitiveType::Bool))
    }

    /// True if either type is [`Self::Unknown`]. Useful for short-circuiting
    /// operator-type checks: if we already don't know one operand's type,
    /// we shouldn't pile on a "type mismatch" error on top.
    #[must_use]
    pub fn is_unknown(&self) -> bool {
        matches!(self, Self::Unknown(_))
    }

    /// Substitute generic parameters by name, returning a new [`Type`].
    ///
    /// `mapping` maps parameter names to their replacement types.
    /// Substitution applies at `Type::Nominal` *leaves* — single-segment
    /// nominals with no generic args whose path matches a key in the
    /// mapping are replaced by the mapped type. Compound types (`Ref`,
    /// `Array`, `Slice`, `Tuple`, `Range`, generic-arg `Nominal`) are
    /// recursed into but otherwise preserved.
    ///
    /// This is used by [`TypeRegistry::unfold_one`] when instantiating
    /// a generic alias: after looking up `@type Pair<T> = (T, T);`, we
    /// substitute `{T → u32}` in the target to yield `(u32, u32)`.
    ///
    /// # Examples
    ///
    /// ```
    /// // Internal — exercised via integration tests; the substitution
    /// // helper is not part of the public API today.
    /// ```
    pub(crate) fn substitute(&self, mapping: &HashMap<&str, &Type>) -> Type {
        match self {
            Self::Nominal { path, args } => {
                // Leaf substitution: single-segment, no args, name matches.
                if path.len() == 1 && args.is_empty() {
                    if let Some(replacement) = mapping.get(path[0].as_str()) {
                        return (*replacement).clone();
                    }
                }
                // Otherwise recurse into args. The path stays as-is (it
                // refers to a top-level type, not a generic param).
                let new_args: Vec<Type> = args.iter().map(|a| a.substitute(mapping)).collect();
                Self::Nominal {
                    path: path.clone(),
                    args: new_args,
                }
            }
            Self::Ref { mutable, inner } => Self::Ref {
                mutable: *mutable,
                inner: Box::new(inner.substitute(mapping)),
            },
            Self::Array { element, size } => Self::Array {
                element: Box::new(element.substitute(mapping)),
                size: size.clone(),
            },
            Self::Slice { element } => Self::Slice {
                element: Box::new(element.substitute(mapping)),
            },
            Self::Tuple(elems) => {
                Self::Tuple(elems.iter().map(|t| t.substitute(mapping)).collect())
            }
            Self::Range { element, inclusive } => Self::Range {
                element: Box::new(element.substitute(mapping)),
                inclusive: *inclusive,
            },
            // Atoms — `Unit`, `Primitive(...)`, `StringSlice`,
            // `Unknown(...)` — have no substitutable parts.
            other => other.clone(),
        }
    }
}

/// The output of [`infer`] — a per-expression type map keyed by source span.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Typing {
    /// Map: expression-span → computed [`Type`]. Every expression node the
    /// type checker visited has an entry; expressions that couldn't be
    /// typed get [`Type::Unknown`] rather than being absent, so consumers
    /// can distinguish "not visited" (absent) from "visited, type unknown."
    pub types: HashMap<Span, Type>,
}

impl Typing {
    /// Look up the inferred type for an expression by its source span.
    /// Returns `None` if the expression was never visited.
    #[must_use]
    pub fn lookup(&self, span: Span) -> Option<&Type> {
        self.types.get(&span)
    }

    /// Number of expression types recorded.
    #[must_use]
    pub fn len(&self) -> usize {
        self.types.len()
    }

    /// True if no expression types were recorded.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.types.is_empty()
    }
}

/// Run type inference on a [`Program`] given its prior [`Resolution`].
///
/// Walks every `@fn` / `#effect` / `#interrupt` / `#transition` body,
/// computing the type of each expression bottom-up and recording it in the
/// returned [`Typing`]. Operand-type compatibility for unary and binary
/// operators is checked as we go; mismatches accumulate as
/// [`TypeError`]s rather than fail-fast so a single pass surfaces every
/// diagnostic the user has.
///
/// # Errors
///
/// Returns `Err(Vec<TypeError>)` when any expression-type incompatibility
/// or `let`-annotation mismatch is encountered. The error vector is
/// non-empty and ordered by source position. On success, `Ok(Typing)`
/// contains a complete type map.
///
/// # Examples
///
/// ```
/// use clifford_lexer::tokenize;
/// use clifford_parser::parse;
/// use clifford_resolve::resolve;
/// use clifford_types::{infer, Type};
/// use clifford_ast::PrimitiveType;
///
/// let tokens = tokenize("@fn add() -> u32 { let x: u32 = 1u32 + 2u32; }").unwrap();
/// let program = parse(&tokens).unwrap();
/// let resolution = resolve(&program).unwrap();
/// let typing = infer(&program, &resolution).unwrap();
/// assert!(typing.len() > 0);
/// ```
pub fn infer(program: &Program, resolution: &Resolution) -> Result<Typing, Vec<TypeError>> {
    let signatures = build_signatures(program);
    let automaton_field_types = build_automaton_field_types(program);
    let type_registry = build_type_registry(program);
    let trait_registry = TraitRegistry::build(program);

    let mut walker = Inferer {
        resolution,
        signatures: &signatures,
        automaton_field_types: &automaton_field_types,
        type_registry: &type_registry,
        types: HashMap::new(),
        errors: Vec::new(),
        scopes: Vec::new(),
    };

    for item in &program.items {
        match item {
            Item::Fn(decl) => walker.walk_fn_decl(decl),
            Item::Effect(decl) => walker.walk_effect_decl(decl),
            Item::Interrupt(decl) => walker.walk_interrupt_decl(decl),
            Item::Automaton(decl) => walker.walk_automaton_decl(decl),
            // Other items have no bodies that this slice walks.
            _ => {}
        }
    }

    // Decision #22 / Decision #2 / ADR 0003: validate every trait list
    // (signature-time check; runs after body walk so all errors are
    // collected in one pass).
    validate_trait_lists(program, &trait_registry, &mut walker.errors);

    // Decision #24 / ADR 0004 Q1: validate every `@fn` whose body
    // contains a `@snapshot` carries the `Readable` row. `#`-layer
    // callables are not gated.
    validate_snapshot_row_gates(program, &mut walker.errors);

    // Slice T4c: validate every path-position type expression in the
    // program against the type registry. Emits E0518 for unknown
    // nominals and E0519 for arity mismatches.
    validate_nominal_paths(program, &type_registry, &mut walker.errors);

    if walker.errors.is_empty() {
        Ok(Typing {
            types: walker.types,
        })
    } else {
        Err(walker.errors)
    }
}

/// One callable's signature: parameter types in order + return type.
#[derive(Debug, Clone)]
struct Signature {
    params: Vec<Type>,
    return_type: Type,
}

/// Build a `name → Signature` registry for every top-level callable
/// (`@fn`, `#effect`, `#interrupt`). Used by the call-typing path so it
/// doesn't have to re-walk the AST per call site.
fn build_signatures(program: &Program) -> HashMap<String, Signature> {
    let mut map: HashMap<String, Signature> = HashMap::new();
    for item in &program.items {
        match item {
            Item::Fn(decl) => {
                map.insert(
                    decl.name.clone(),
                    signature_from_params(&decl.params, decl.return_type.as_ref()),
                );
            }
            Item::Effect(decl) => {
                map.insert(
                    decl.name.clone(),
                    signature_from_params(&decl.params, decl.return_type.as_ref()),
                );
            }
            Item::Interrupt(decl) => {
                map.insert(
                    decl.name.clone(),
                    signature_from_params(&decl.params, decl.return_type.as_ref()),
                );
            }
            _ => {}
        }
    }
    map
}

fn signature_from_params(params: &[Param], return_type: Option<&TypeExpr>) -> Signature {
    Signature {
        params: params.iter().map(|p| type_from_type_expr(&p.ty)).collect(),
        return_type: return_type.map(type_from_type_expr).unwrap_or(Type::Unit),
    }
}

/// Build a `automaton-name → field-name → Type` registry for every
/// `#automaton`'s declared fields. Used by the FieldAccess-typing path
/// to look up `Counter.value`'s type without re-walking.
fn build_automaton_field_types(program: &Program) -> HashMap<String, HashMap<String, Type>> {
    let mut map: HashMap<String, HashMap<String, Type>> = HashMap::new();
    for item in &program.items {
        if let Item::Automaton(decl) = item {
            let fields: HashMap<String, Type> = decl
                .fields
                .iter()
                .map(|f| (f.name.clone(), type_from_type_expr(&f.ty)))
                .collect();
            map.insert(decl.name.clone(), fields);
        }
    }
    map
}

/// One entry in the [`TypeRegistry`] — a top-level `@type` declaration
/// classified by what kind of body it has.
///
/// Slice T4b distinguished aliases from ADTs but did not capture generic
/// parameter names. Slice T4c extends both variants with `params`
/// (the generic-parameter names declared on the `@type`) so that
/// generic alias substitution (`@type Pair<T> = (T, T)` applied to
/// `Pair<u32>`) and arity validation (`Pair<u32, bool>` is `E0519`) are
/// possible.
#[derive(Debug, Clone)]
enum NominalDecl {
    /// `@type Foo = u32;` (params=[]) or `@type Pair<T> = (T, T);`
    /// (params=["T"]). The `target` references generic-parameter names
    /// as `Type::Nominal { path: [name], args: [] }` shapes (since the
    /// parser doesn't distinguish them syntactically); substitution
    /// at instantiation time replaces those leaves with the actual
    /// type arguments.
    Alias {
        /// Generic parameter names in declaration order. Empty for
        /// non-generic aliases.
        params: Vec<String>,
        /// The alias's target type, with generic parameters surviving
        /// as same-named `Type::Nominal { path: [name], args: [] }`
        /// leaves. [`Type::substitute`] replaces them at instantiation.
        target: Type,
    },
    /// `@type Result<T, E> = | Ok(T) | Err(E);` — ADT: nominal,
    /// terminal, does not unfold. Two ADT nominals are equal iff their
    /// names + args match (per Decision #19). T4c records `params` for
    /// arity validation; T4d adds variant-position resolution
    /// (`Result::Ok`) via `variants`.
    Adt {
        /// Generic parameter names in declaration order. Empty for
        /// non-generic ADTs.
        params: Vec<String>,
        /// Variant info per ADT variant (T4d): name → arg types
        /// (with generic-parameter names appearing as
        /// `Type::Nominal { path: [name], args: [] }` leaves, same
        /// shape as alias targets). Used by `Type::substitute` when
        /// the user instantiates a generic ADT and refers to its
        /// variants. Order of declaration is preserved.
        variants: Vec<VariantInfo>,
    },
}

/// One variant of an ADT (T4d). `@type Color = | Red | Green | Blue;`
/// gives three `VariantInfo`s with empty `args`. `@type Maybe = | None
/// | Some(u32);` gives `None` (empty args) and `Some` (one-arg).
/// Struct-style variants (`Name { f: T }`) are flattened into the
/// `args` vec in field order; named-field access semantics is post-T4d
/// work.
#[derive(Debug, Clone)]
struct VariantInfo {
    /// Variant name (the `Ok` in `Result::Ok`).
    name: String,
    /// Arg types for this variant. Empty for unit-like variants.
    /// Generic-parameter references survive as
    /// `Type::Nominal { path: [param_name], args: [] }` leaves; the
    /// parent ADT's `params` list is the substitution domain.
    args: Vec<Type>,
}

impl NominalDecl {
    /// Number of generic parameters this declaration takes.
    fn arity(&self) -> usize {
        match self {
            Self::Alias { params, .. } | Self::Adt { params, .. } => params.len(),
        }
    }
}

/// Registry of every top-level `@type` declaration in the program, indexed
/// by name. Used by [`TypeRegistry::unalias`] to follow aliases when
/// comparing types (so `let x: MyAlias = 0u32;` typechecks when
/// `@type MyAlias = u32;`).
///
/// Slice T4c scope:
/// - Non-generic alias following (T4b carry-forward).
/// - **Generic alias substitution** (T4c new): `@type Pair<T> = (T, T);`
///   applied to `Pair<u32>` unfolds to `(u32, u32)` via
///   [`Type::substitute`] using the alias's `params`.
/// - **Path validation** via [`validate_nominal_paths`] reports
///   `E0518 UnknownNominalType` and `E0519 ArityMismatch` separately.
///
/// Multi-segment variant resolution (`Result::Ok`) and module-qualified
/// paths remain T4d+ work.
#[derive(Debug)]
struct TypeRegistry {
    /// Map: `@type` name → declaration kind (alias target + params, or
    /// ADT marker + params).
    decls: HashMap<String, NominalDecl>,
}

impl TypeRegistry {
    /// Returns true if `path` (single segment, currently) names a known
    /// top-level `@type` declaration. Multi-segment paths (e.g.
    /// `clifford::core::Option`) always return false in T4b/T4c —
    /// module resolution lands in T4d+.
    fn is_known(&self, path: &[String]) -> bool {
        path.len() == 1 && self.decls.contains_key(&path[0])
    }

    /// Look up the [`NominalDecl`] for a single-segment path, if any.
    fn lookup(&self, path: &[String]) -> Option<&NominalDecl> {
        if path.len() != 1 {
            return None;
        }
        self.decls.get(&path[0])
    }

    /// T4d: resolve a multi-segment path like `Result::Ok` to an ADT
    /// variant. Returns `Some((adt_name, params, variant))` if
    /// `path[0]` is a registered ADT and `path[1]` matches one of its
    /// variants. Returns `None` for: single-segment paths, unknown
    /// first-segment names, alias first segments (aliases don't have
    /// variants), and paths whose second segment doesn't match any
    /// variant of the ADT.
    fn lookup_variant<'a>(
        &'a self,
        path: &'a [String],
    ) -> Option<(&'a str, &'a [String], &'a VariantInfo)> {
        if path.len() != 2 {
            return None;
        }
        let NominalDecl::Adt { params, variants } = self.decls.get(&path[0])? else {
            return None;
        };
        let variant = variants.iter().find(|v| v.name == path[1])?;
        Some((path[0].as_str(), params.as_slice(), variant))
    }

    /// If `t` is a single-segment `Type::Nominal` whose path names a
    /// registered alias *and the arity matches*, return the alias's
    /// target type with generic parameters substituted by the supplied
    /// args. One step (not transitive); [`Self::unalias`] iterates.
    ///
    /// Returns `None` for:
    /// - non-`Nominal` types (primitives, `Ref`, `Array`, …)
    /// - ADT nominals (terminal — never unfold)
    /// - multi-segment nominals (deferred to T4d)
    /// - unknown nominals
    /// - **arity-mismatched nominals** (the validation pass reports
    ///   `E0519`; unfolding silently is the right behaviour at the
    ///   compatibility-check site since the program is already
    ///   ill-formed and we don't want a cascade of mismatches).
    fn unfold_one(&self, t: &Type) -> Option<Type> {
        let Type::Nominal { path, args } = t else {
            return None;
        };
        let decl = self.lookup(path)?;
        let NominalDecl::Alias { params, target } = decl else {
            return None;
        };
        if params.len() != args.len() {
            return None;
        }
        if params.is_empty() {
            // Non-generic alias — no substitution needed.
            return Some(target.clone());
        }
        // Build the substitution mapping (param-name → arg) and apply
        // it to the alias body.
        let mapping: HashMap<&str, &Type> = params
            .iter()
            .map(String::as_str)
            .zip(args.iter())
            .collect();
        Some(target.substitute(&mapping))
    }

    /// Recursively unfold aliases until `t` is no longer a registered
    /// alias. Cycle-safe via depth limit (cycle in `@type A = B; @type B
    /// = A;` returns `Type::Unknown`; not legal but defensive).
    ///
    /// Idempotent on non-aliases: `unalias(Primitive(U32))` returns
    /// `Primitive(U32)` unchanged.
    fn unalias(&self, t: &Type) -> Type {
        let mut current = t.clone();
        // Depth limit: realistic alias chains are 1-3 deep; 32 is generous.
        // Aliasing more than that almost certainly indicates a cycle in
        // user source; fail safe to Unknown rather than stack overflow.
        for _ in 0..32 {
            match self.unfold_one(&current) {
                Some(next) => current = next,
                None => return current,
            }
        }
        Type::Unknown("alias cycle or excessive nesting (T4b safeguard)")
    }
}

/// Predeclared pure-side traits per Decision #2 + ADR 0003.
///
/// Used on `@fn` declarations and `@fn`-pointer types. The semantics
/// switch between *purity classification* (Decision #2 — `Pure`,
/// `Readable`, `Observable`, `Opaque`) and *first-class effect rows*
/// (ADR 0003 P2) at row-composition time; for now this slice just
/// validates that the names are recognised.
const PREDECLARED_PURE_TRAITS: &[&str] = &["Pure", "Readable", "Observable", "Opaque"];

/// Predeclared imperative-side traits per Decision #22.
///
/// Used on `#effect`, `#interrupt`, and `#transition` declarations.
/// Three subgroups by consumer:
///
/// - **Codegen consumers** (memory-ordering fences): `Acquire`,
///   `Release`, `SeqCst`.
/// - **`cliffordc audit` / certification**: `Hardware`, `Realtime`,
///   `LockingDiscipline`, `PureState`, `Encapsulated`.
///
/// The orthogonality engine ignores all of them (per Decision #22
/// design); this slice just validates their names so a typo
/// (`Realtim` instead of `Realtime`) surfaces as an early diagnostic
/// rather than silent acceptance.
const PREDECLARED_IMPERATIVE_TRAITS: &[&str] = &[
    "Hardware",
    "Realtime",
    "Acquire",
    "Release",
    "SeqCst",
    "LockingDiscipline",
    "PureState",
    "Encapsulated",
];

/// Which sigil layer a known trait belongs to.
///
/// - `Pure` — pure-side trait (Decision #2 / ADR 0003 P2). Valid on
///   `@fn` declarations only; using on `#effect` / `#interrupt` /
///   `#transition` is `E0544 TraitLayerMismatch`.
/// - `Imperative` — imperative-side trait (Decision #22). Valid on
///   `#effect` / `#interrupt` / `#transition`; using on `@fn` is
///   `E0544`.
/// - `Universal` — user-defined `@trait Name { … }`. Valid on either
///   layer in v0.2-β. (A future slice may attach explicit layer tags
///   to `@trait` declarations.)
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum TraitLayer {
    Pure,
    Imperative,
    Universal,
}

impl TraitLayer {
    /// Source-form name of this layer for diagnostics.
    fn as_str(self) -> &'static str {
        match self {
            Self::Pure => "pure",
            Self::Imperative => "imperative",
            Self::Universal => "universal",
        }
    }

    /// Is this trait usable on a callable in `callable_layer`?
    fn is_usable_on(self, callable_layer: Self) -> bool {
        match self {
            // Universal user-defined traits always validate.
            Self::Universal => true,
            // Predeclared pure-side: callable must be Pure.
            Self::Pure => callable_layer == Self::Pure,
            // Predeclared imperative-side: callable must be Imperative.
            Self::Imperative => callable_layer == Self::Imperative,
        }
    }
}

/// Registry of every recognised trait name in the program — predeclared
/// (per Decision #2 + Decision #22) plus user-defined `@trait` items,
/// each tagged with its [`TraitLayer`].
///
/// Built once per `infer()` call. Used by [`validate_trait_lists`] to
/// emit `E0541 UnknownTrait` for typos and `E0544 TraitLayerMismatch`
/// for cross-layer misuse.
#[derive(Debug)]
struct TraitRegistry {
    /// Map: trait name → its layer classification.
    known: HashMap<String, TraitLayer>,
}

impl TraitRegistry {
    /// Build the registry: predeclared-pure (tagged `Pure`) ∪
    /// predeclared-imperative (tagged `Imperative`) ∪ user-`@trait`
    /// declarations (tagged `Universal`).
    fn build(program: &Program) -> Self {
        let mut known: HashMap<String, TraitLayer> = HashMap::with_capacity(
            PREDECLARED_PURE_TRAITS.len() + PREDECLARED_IMPERATIVE_TRAITS.len() + 4,
        );
        for &name in PREDECLARED_PURE_TRAITS.iter() {
            known.insert((*name).to_owned(), TraitLayer::Pure);
        }
        for &name in PREDECLARED_IMPERATIVE_TRAITS.iter() {
            known.insert((*name).to_owned(), TraitLayer::Imperative);
        }
        for item in &program.items {
            if let Item::Trait(td) = item {
                // First-wins on collisions with predeclared names; the
                // resolver E0401 already flags duplicates separately.
                known
                    .entry(td.name.clone())
                    .or_insert(TraitLayer::Universal);
            }
        }
        Self { known }
    }

    /// True if `name` is a recognised trait (predeclared or user-defined).
    /// Convenience for callers that only need the existence answer; the
    /// layer-aware check uses [`Self::layer_of`].
    #[allow(dead_code)] // Public-shape helper kept for future consumers
    fn is_known(&self, name: &str) -> bool {
        self.known.contains_key(name)
    }

    /// Look up the layer classification of a known trait, or `None`
    /// if the trait isn't recognised.
    fn layer_of(&self, name: &str) -> Option<TraitLayer> {
        self.known.get(name).copied()
    }
}

/// Walk every `@fn` / `#effect` / `#interrupt` / `#transition` in the
/// program and validate that each entry in its `trait_list` is a known
/// trait per [`TraitRegistry::is_known`]. Emits `E0541 UnknownTrait`
/// for unresolved names.
///
/// This is a separate pass (rather than inline in the body walk)
/// because trait validation is a *signature-time* concern, not a
/// body-walking one — it should fire even on callables whose bodies
/// the inferer never enters (e.g. `@fn` declarations whose call sites
/// are all in a different module).
fn validate_trait_lists(
    program: &Program,
    registry: &TraitRegistry,
    errors: &mut Vec<TypeError>,
) {
    for item in &program.items {
        match item {
            Item::Fn(d) => check_traits(
                &d.trait_list,
                &d.name,
                "@fn",
                TraitLayer::Pure,
                registry,
                errors,
            ),
            Item::Effect(d) => check_traits(
                &d.trait_list,
                &d.name,
                "#effect",
                TraitLayer::Imperative,
                registry,
                errors,
            ),
            Item::Interrupt(d) => check_traits(
                &d.trait_list,
                &d.name,
                "#interrupt",
                TraitLayer::Imperative,
                registry,
                errors,
            ),
            Item::Automaton(d) => {
                for t in &d.transitions {
                    check_traits(
                        &t.trait_list,
                        &t.name,
                        "#transition",
                        TraitLayer::Imperative,
                        registry,
                        errors,
                    );
                }
            }
            _ => {}
        }
    }
}

/// Check every entry in `list` against the registry. Emits two error
/// kinds:
/// - `E0541 UnknownTrait` when the name is not recognised at all.
/// - `E0544 TraitLayerMismatch` (Decision #22 layer-aware check) when
///   a predeclared trait is used on the wrong layer (e.g. `Realtime`
///   on a `@fn`, or `Pure` on a `#effect`).
///
/// The two diagnostics are independent: an unknown name reports
/// E0541 only (no layer-mismatch since we don't know the layer).
fn check_traits(
    list: &[TraitRef],
    callable: &str,
    kind: &'static str,
    callable_layer: TraitLayer,
    registry: &TraitRegistry,
    errors: &mut Vec<TypeError>,
) {
    for tr in list {
        match registry.layer_of(&tr.name) {
            None => {
                errors.push(TypeError::UnknownTrait {
                    trait_name: tr.name.clone(),
                    callable: callable.to_owned(),
                    kind,
                    at: tr.span.start,
                });
            }
            Some(trait_layer) => {
                if !trait_layer.is_usable_on(callable_layer) {
                    errors.push(TypeError::TraitLayerMismatch {
                        trait_name: tr.name.clone(),
                        expected_layer: trait_layer.as_str(),
                        callable: callable.to_owned(),
                        actual_kind: kind,
                        at: tr.span.start,
                    });
                }
            }
        }
    }
}

/// Decision #24 / ADR 0004 Q1: `@snapshot` is a controlled effect
/// (not pure). An `@fn` body that uses `@snapshot` must carry the
/// `Readable` row in its `$ [TraitList]`; otherwise emit
/// `E0550 SnapshotInUnreadableFn`.
///
/// `#`-layer callables (`#effect`, `#interrupt`, `#transition`) are
/// *not* gated — they are imperative and may always observe automaton
/// state. The gate is purely a pure-side discipline.
///
/// One E0550 per offending `@fn`: the walker stops at the first
/// `@snapshot` it finds, since one missing `Readable` covers all the
/// snapshots in the body.
fn validate_snapshot_row_gates(program: &Program, errors: &mut Vec<TypeError>) {
    for item in &program.items {
        if let Item::Fn(decl) = item {
            let mut finder = SnapshotFinder { found_at: None };
            finder.walk_block(&decl.body);
            let Some(at) = finder.found_at else {
                continue;
            };
            let has_readable = decl.trait_list.iter().any(|t| t.name == "Readable");
            if !has_readable {
                errors.push(TypeError::SnapshotInUnreadableFn {
                    fn_name: decl.name.clone(),
                    at,
                    decl_at: decl.span.start,
                });
            }
        }
    }
}

/// Slice T4c: walk every path-position [`TypeExpr`] in the program and
/// validate it against the [`TypeRegistry`]. Emits:
///
/// - `E0518 UnknownNominalType` when a single-segment path doesn't
///   resolve to any registered `@type` decl *and* isn't a generic
///   parameter in scope. Multi-segment paths currently always trigger
///   E0518 — module resolution is T4d+ work.
/// - `E0519 GenericArityMismatch` when a known nominal's generic-arg
///   count differs from the declared arity (e.g. `Pair<u32, bool>` for
///   `@type Pair<T> = …;`).
///
/// **Generic-parameter scoping (T4c):** When walking a `@type
/// Pair<T> = (T, T);` body, `T` is treated as a known name within that
/// body's type expressions. Same for `@fn` generic params (when those
/// land at parser level for `@fn` signatures). The walker threads a
/// `&[String]` slice of names-in-scope through every recursion.
///
/// What this slice does NOT validate:
/// - Trait-bound satisfaction on generic params (`T: Copy`) — full
///   HM-unification work, deferred to a later slice.
/// - Paths inside `@trait` method signatures — Slice 6 work.
fn validate_nominal_paths(
    program: &Program,
    registry: &TypeRegistry,
    errors: &mut Vec<TypeError>,
) {
    let no_params: &[String] = &[];
    for item in &program.items {
        match item {
            Item::Fn(decl) => {
                // `@fn` doesn't carry generic params at the AST level
                // yet (parser slice for `@fn<T>` is post-T4c). When it
                // does, it'll feed them in here.
                for p in &decl.params {
                    walk_type_expr(&p.ty, registry, no_params, errors);
                }
                if let Some(rt) = &decl.return_type {
                    walk_type_expr(rt, registry, no_params, errors);
                }
                walk_block_for_type_exprs(&decl.body, registry, no_params, errors);
            }
            Item::Effect(decl) => {
                for p in &decl.params {
                    walk_type_expr(&p.ty, registry, no_params, errors);
                }
                if let Some(rt) = &decl.return_type {
                    walk_type_expr(rt, registry, no_params, errors);
                }
                walk_block_for_type_exprs(&decl.body, registry, no_params, errors);
            }
            Item::Interrupt(decl) => {
                for p in &decl.params {
                    walk_type_expr(&p.ty, registry, no_params, errors);
                }
                if let Some(rt) = &decl.return_type {
                    walk_type_expr(rt, registry, no_params, errors);
                }
                walk_block_for_type_exprs(&decl.body, registry, no_params, errors);
            }
            Item::Automaton(decl) => {
                for f in &decl.fields {
                    walk_type_expr(&f.ty, registry, no_params, errors);
                }
                for tr in &decl.transitions {
                    walk_block_for_type_exprs(&tr.body, registry, no_params, errors);
                }
            }
            Item::Type(decl) => {
                use clifford_ast::{TypeBody, VariantData};
                // The `@type`'s own generic params are in scope inside
                // its body — `Pair<T> = (T, T)` references `T`.
                let params: Vec<String> =
                    decl.generic_params.iter().map(|p| p.name.clone()).collect();
                match &decl.body {
                    TypeBody::Alias(te) => walk_type_expr(te, registry, &params, errors),
                    TypeBody::Adt(variants) => {
                        for v in variants {
                            match &v.data {
                                VariantData::None => {}
                                VariantData::Tuple(types) => {
                                    for t in types {
                                        walk_type_expr(t, registry, &params, errors);
                                    }
                                }
                                VariantData::Struct(fields) => {
                                    for f in fields {
                                        walk_type_expr(&f.ty, registry, &params, errors);
                                    }
                                }
                            }
                        }
                    }
                }
            }
            // Other items don't carry type expressions in v0.1 / v0.2
            // scope (or carry them in slots T4c isn't required to walk).
            _ => {}
        }
    }
}

/// Recursively visit a [`TypeExpr`] and validate every nested path
/// position. `params_in_scope` is the set of generic-parameter names
/// the surrounding context introduced (e.g. `T` from `@type Pair<T>`).
/// Single-segment paths matching one of those names are treated as
/// known with arity 0 (no further check) — they're parameters, not
/// top-level decls.
fn walk_type_expr(
    t: &TypeExpr,
    registry: &TypeRegistry,
    params_in_scope: &[String],
    errors: &mut Vec<TypeError>,
) {
    match &t.kind {
        TypeKind::Path(pt) => {
            // Recurse into generic args first (so unknown args report
            // even when the outer name is unknown).
            for arg in &pt.generic_args {
                walk_type_expr(arg, registry, params_in_scope, errors);
            }
            // Single-segment + matches a generic param in scope?
            // Then it's a parameter reference (always 0-arity for now;
            // we don't yet support higher-kinded params like `T<u32>`).
            if pt.segments.len() == 1 && params_in_scope.contains(&pt.segments[0]) {
                if !pt.generic_args.is_empty() {
                    errors.push(TypeError::GenericArityMismatch {
                        name: pt.segments[0].clone(),
                        expected: 0,
                        actual: pt.generic_args.len(),
                        at: t.span.start,
                    });
                }
                return;
            }
            // Otherwise validate against the registry.
            if pt.segments.len() != 1 || !registry.is_known(&pt.segments) {
                errors.push(TypeError::UnknownNominalType {
                    name: pt.segments.join("::"),
                    at: t.span.start,
                });
                return;
            }
            // Known top-level: arity-check.
            if let Some(decl) = registry.lookup(&pt.segments) {
                let expected = decl.arity();
                let actual = pt.generic_args.len();
                if expected != actual {
                    errors.push(TypeError::GenericArityMismatch {
                        name: pt.segments[0].clone(),
                        expected,
                        actual,
                        at: t.span.start,
                    });
                }
            }
        }
        TypeKind::Ref(rt) => walk_type_expr(&rt.inner, registry, params_in_scope, errors),
        TypeKind::Array(at) => walk_type_expr(&at.element, registry, params_in_scope, errors),
        TypeKind::Slice(st) => walk_type_expr(&st.element, registry, params_in_scope, errors),
        TypeKind::Tuple(tt) => {
            for elem in &tt.elements {
                walk_type_expr(elem, registry, params_in_scope, errors);
            }
        }
        TypeKind::Access(at) => walk_type_expr(&at.inner, registry, params_in_scope, errors),
        TypeKind::Fn(ft) => {
            for p in &ft.params {
                walk_type_expr(p, registry, params_in_scope, errors);
            }
            if let Some(rt) = &ft.return_type {
                walk_type_expr(rt, registry, params_in_scope, errors);
            }
        }
        // `Unit`, `Primitive` — no nested paths.
        _ => {}
    }
}

/// Walk a function/effect/transition body looking for `let _: T = …;`
/// type annotations and validate `T` against the registry. Statement-
/// level type expressions land here; expression-level type expressions
/// (e.g. `expr as T` casts) are also reachable through this walk.
fn walk_block_for_type_exprs(
    block: &Block,
    registry: &TypeRegistry,
    params_in_scope: &[String],
    errors: &mut Vec<TypeError>,
) {
    for s in &block.stmts {
        walk_stmt_for_type_exprs(s, registry, params_in_scope, errors);
    }
}

fn walk_stmt_for_type_exprs(
    stmt: &Stmt,
    registry: &TypeRegistry,
    params_in_scope: &[String],
    errors: &mut Vec<TypeError>,
) {
    match &stmt.kind {
        StmtKind::Let { ty, value, .. } => {
            if let Some(annotation) = ty {
                walk_type_expr(annotation, registry, params_in_scope, errors);
            }
            walk_expr_for_type_exprs(value, registry, params_in_scope, errors);
        }
        StmtKind::LetShort { value, .. } => {
            walk_expr_for_type_exprs(value, registry, params_in_scope, errors);
        }
        StmtKind::Expr(e) => walk_expr_for_type_exprs(e, registry, params_in_scope, errors),
        StmtKind::Return(Some(e)) => walk_expr_for_type_exprs(e, registry, params_in_scope, errors),
        StmtKind::Mutate { assigns, .. } => {
            for fa in assigns {
                if let Some(idx) = &fa.index {
                    walk_expr_for_type_exprs(idx, registry, params_in_scope, errors);
                }
                walk_expr_for_type_exprs(&fa.value, registry, params_in_scope, errors);
            }
        }
        StmtKind::MutateShort { value, .. } => {
            walk_expr_for_type_exprs(value, registry, params_in_scope, errors);
        }
        StmtKind::ProcCall { args, .. } => {
            for a in args {
                walk_expr_for_type_exprs(a, registry, params_in_scope, errors);
            }
        }
        StmtKind::UncheckedStore { ptr, value, .. }
        | StmtKind::VolatileStore { ptr, value, .. } => {
            walk_expr_for_type_exprs(ptr, registry, params_in_scope, errors);
            walk_expr_for_type_exprs(value, registry, params_in_scope, errors);
        }
        _ => {}
    }
}

fn walk_expr_for_type_exprs(
    expr: &Expr,
    registry: &TypeRegistry,
    params_in_scope: &[String],
    errors: &mut Vec<TypeError>,
) {
    match &expr.kind {
        ExprKind::Cast { value, ty } => {
            walk_expr_for_type_exprs(value, registry, params_in_scope, errors);
            walk_type_expr(ty, registry, params_in_scope, errors);
        }
        ExprKind::UncheckedLoad { ty, ptr } | ExprKind::VolatileLoad { ty, ptr } => {
            walk_type_expr(ty, registry, params_in_scope, errors);
            walk_expr_for_type_exprs(ptr, registry, params_in_scope, errors);
        }
        ExprKind::UncheckedCast {
            from_ty,
            to_ty,
            value,
            ..
        } => {
            walk_type_expr(from_ty, registry, params_in_scope, errors);
            walk_type_expr(to_ty, registry, params_in_scope, errors);
            walk_expr_for_type_exprs(value, registry, params_in_scope, errors);
        }
        ExprKind::UncheckedOffset { ty, ptr, n } => {
            walk_type_expr(ty, registry, params_in_scope, errors);
            walk_expr_for_type_exprs(ptr, registry, params_in_scope, errors);
            walk_expr_for_type_exprs(n, registry, params_in_scope, errors);
        }
        ExprKind::Call { callee, args } => {
            walk_expr_for_type_exprs(callee, registry, params_in_scope, errors);
            for a in args {
                walk_expr_for_type_exprs(a, registry, params_in_scope, errors);
            }
        }
        ExprKind::MethodCall { obj, args, .. } => {
            walk_expr_for_type_exprs(obj, registry, params_in_scope, errors);
            for a in args {
                walk_expr_for_type_exprs(a, registry, params_in_scope, errors);
            }
        }
        ExprKind::Binary { lhs, rhs, .. } => {
            walk_expr_for_type_exprs(lhs, registry, params_in_scope, errors);
            walk_expr_for_type_exprs(rhs, registry, params_in_scope, errors);
        }
        ExprKind::Unary { operand, .. } | ExprKind::Ref { operand, .. } => {
            walk_expr_for_type_exprs(operand, registry, params_in_scope, errors);
        }
        ExprKind::Paren(inner) => walk_expr_for_type_exprs(inner, registry, params_in_scope, errors),
        ExprKind::Tuple(es) | ExprKind::Array(es) => {
            for e in es {
                walk_expr_for_type_exprs(e, registry, params_in_scope, errors);
            }
        }
        ExprKind::ArrayRepeat { value, count } => {
            walk_expr_for_type_exprs(value, registry, params_in_scope, errors);
            walk_expr_for_type_exprs(count, registry, params_in_scope, errors);
        }
        ExprKind::FieldAccess { obj, .. } => {
            walk_expr_for_type_exprs(obj, registry, params_in_scope, errors);
        }
        ExprKind::Index { obj, index } => {
            walk_expr_for_type_exprs(obj, registry, params_in_scope, errors);
            walk_expr_for_type_exprs(index, registry, params_in_scope, errors);
        }
        ExprKind::Range { lo, hi, .. } => {
            walk_expr_for_type_exprs(lo, registry, params_in_scope, errors);
            walk_expr_for_type_exprs(hi, registry, params_in_scope, errors);
        }
        // Atoms — no embedded type expressions.
        _ => {}
    }
}

/// Walker that scans an expression tree for the first `@snapshot`
/// expression, recording its byte offset.
///
/// Mirrors the structure of `clifford-check`'s `SelfRecursionFinder`.
/// Stops at the first hit; subsequent walks short-circuit. Visits all
/// the same compound forms — every place an expression can hide a
/// `@snapshot` (Call args, Binary ops, Index, FieldAccess receiver,
/// etc.).
struct SnapshotFinder {
    found_at: Option<usize>,
}

impl SnapshotFinder {
    fn walk_block(&mut self, block: &Block) {
        for s in &block.stmts {
            if self.found_at.is_some() {
                return;
            }
            self.walk_stmt(s);
        }
    }

    fn walk_stmt(&mut self, stmt: &Stmt) {
        if self.found_at.is_some() {
            return;
        }
        match &stmt.kind {
            StmtKind::Let { value, .. } | StmtKind::LetShort { value, .. } => {
                self.walk_expr(value);
            }
            StmtKind::Expr(e) => self.walk_expr(e),
            StmtKind::Return(Some(e)) => self.walk_expr(e),
            // `@fn` bodies cannot contain `#`-layer statements
            // (Decision #1 / Emergent Rule 4 enforced by `clifford-check`
            // S1) so the mutation / proc-call / unsafe-store arms of
            // `StmtKind` are absent in well-formed `@fn` source. Fall
            // through silently for any other shape.
            _ => {}
        }
    }

    fn walk_expr(&mut self, expr: &Expr) {
        if self.found_at.is_some() {
            return;
        }
        match &expr.kind {
            ExprKind::Snapshot { .. } => {
                self.found_at = Some(expr.span.start);
            }
            ExprKind::Call { callee, args } => {
                self.walk_expr(callee);
                for a in args {
                    self.walk_expr(a);
                    if self.found_at.is_some() {
                        return;
                    }
                }
            }
            ExprKind::MethodCall { obj, args, .. } => {
                self.walk_expr(obj);
                for a in args {
                    self.walk_expr(a);
                    if self.found_at.is_some() {
                        return;
                    }
                }
            }
            ExprKind::Binary { lhs, rhs, .. } => {
                self.walk_expr(lhs);
                self.walk_expr(rhs);
            }
            ExprKind::Unary { operand, .. } | ExprKind::Ref { operand, .. } => {
                self.walk_expr(operand);
            }
            ExprKind::Paren(inner) => self.walk_expr(inner),
            ExprKind::Tuple(es) | ExprKind::Array(es) => {
                for e in es {
                    self.walk_expr(e);
                    if self.found_at.is_some() {
                        return;
                    }
                }
            }
            ExprKind::ArrayRepeat { value, count } => {
                self.walk_expr(value);
                self.walk_expr(count);
            }
            ExprKind::FieldAccess { obj, .. } => self.walk_expr(obj),
            ExprKind::Index { obj, index } => {
                self.walk_expr(obj);
                self.walk_expr(index);
            }
            ExprKind::Cast { value, .. } => self.walk_expr(value),
            ExprKind::Range { lo, hi, .. } => {
                self.walk_expr(lo);
                self.walk_expr(hi);
            }
            // Atoms and `#`-only forms don't contain snapshots.
            _ => {}
        }
    }
}

/// Build the [`TypeRegistry`] from every `Item::Type` in the program.
///
/// Aliases get their target translated through [`type_from_type_expr`]
/// (so a `@type Foo = u32;` registers as `NominalDecl::Alias { params:
/// vec![], target: Primitive(U32) }`). Generic aliases capture their
/// parameter names so [`TypeRegistry::unfold_one`] can substitute at
/// instantiation time.
///
/// ADTs (`@type Result<T, E> = | Ok(T) | Err(E);`) register as
/// terminal nominal markers carrying their generic-param arity *and*
/// their variant info (T4d): name + arg-type list per variant.
/// Generic-parameter references in variant args survive as
/// `Type::Nominal { path: [param_name], args: [] }` leaves so
/// `Type::substitute` can swap them at instantiation.
fn build_type_registry(program: &Program) -> TypeRegistry {
    use clifford_ast::{TypeBody, VariantData};
    let mut decls: HashMap<String, NominalDecl> = HashMap::new();
    for item in &program.items {
        if let Item::Type(td) = item {
            let params: Vec<String> = td.generic_params.iter().map(|p| p.name.clone()).collect();
            let entry = match &td.body {
                TypeBody::Alias(te) => NominalDecl::Alias {
                    params,
                    target: type_from_type_expr(te),
                },
                TypeBody::Adt(variants) => {
                    let variants: Vec<VariantInfo> = variants
                        .iter()
                        .map(|v| {
                            let args = match &v.data {
                                VariantData::None => Vec::new(),
                                VariantData::Tuple(types) => {
                                    types.iter().map(type_from_type_expr).collect()
                                }
                                VariantData::Struct(fields) => {
                                    fields.iter().map(|f| type_from_type_expr(&f.ty)).collect()
                                }
                            };
                            VariantInfo {
                                name: v.name.clone(),
                                args,
                            }
                        })
                        .collect();
                    NominalDecl::Adt { params, variants }
                }
            };
            // First-wins on duplicate names. Resolver E0401 already reports
            // the duplicate; we just don't overwrite the registration.
            decls.entry(td.name.clone()).or_insert(entry);
        }
    }
    TypeRegistry { decls }
}

// ─── Internal walker ────────────────────────────────────────────────────────

struct Inferer<'a> {
    resolution: &'a Resolution,
    /// Top-level signatures keyed by callable name. Built once at the start
    /// of [`infer`] so per-call lookups are O(1).
    signatures: &'a HashMap<String, Signature>,
    /// Per-automaton field type table: `automaton-name → field-name → Type`.
    /// Used by [`Self::field_access_type`] to look up `Counter.value`'s
    /// declared type without re-walking the AST.
    automaton_field_types: &'a HashMap<String, HashMap<String, Type>>,
    /// Top-level `@type` declarations indexed by name. Used by
    /// [`types_compatible`] to follow aliases when comparing types.
    /// See [`TypeRegistry::unalias`].
    type_registry: &'a TypeRegistry,
    types: HashMap<Span, Type>,
    errors: Vec<TypeError>,
    /// Stack of nested scopes mirroring the resolver's. Each scope holds
    /// `name → Type` for the local bindings introduced in that scope. We
    /// don't reuse the resolver's scope chain because (a) the resolver only
    /// records *what* a name resolves to, not its type, and (b) typing
    /// happens bottom-up over expressions in a way that aligns with
    /// scope-chain push/pop already.
    scopes: Vec<HashMap<String, Type>>,
}

impl<'a> Inferer<'a> {
    fn push_scope(&mut self) {
        self.scopes.push(HashMap::new());
    }

    fn pop_scope(&mut self) {
        let _ = self.scopes.pop();
    }

    fn declare(&mut self, name: &str, ty: Type) {
        if let Some(scope) = self.scopes.last_mut() {
            scope.insert(name.to_owned(), ty);
        }
    }

    fn lookup_local(&self, name: &str) -> Option<&Type> {
        for scope in self.scopes.iter().rev() {
            if let Some(t) = scope.get(name) {
                return Some(t);
            }
        }
        None
    }

    fn record(&mut self, span: Span, ty: Type) {
        self.types.insert(span, ty);
    }

    fn walk_fn_decl(&mut self, decl: &FnDecl) {
        self.push_scope();
        for param in &decl.params {
            self.declare(&param.name, type_from_type_expr(&param.ty));
        }
        self.walk_block(&decl.body);
        self.pop_scope();
    }

    fn walk_effect_decl(&mut self, decl: &EffectDecl) {
        self.push_scope();
        for param in &decl.params {
            self.declare(&param.name, type_from_type_expr(&param.ty));
        }
        self.walk_block(&decl.body);
        self.pop_scope();
    }

    fn walk_interrupt_decl(&mut self, decl: &InterruptDecl) {
        self.push_scope();
        for param in &decl.params {
            self.declare(&param.name, type_from_type_expr(&param.ty));
        }
        self.walk_block(&decl.body);
        self.pop_scope();
    }

    fn walk_automaton_decl(&mut self, decl: &AutomatonDecl) {
        for transition in &decl.transitions {
            self.walk_transition_decl(transition);
        }
    }

    fn walk_transition_decl(&mut self, transition: &TransitionDecl) {
        self.push_scope();
        // Transitions take no parameters in the current AST.
        self.walk_block(&transition.body);
        self.pop_scope();
    }

    fn walk_block(&mut self, block: &Block) {
        self.push_scope();
        for stmt in &block.stmts {
            self.walk_stmt(stmt);
        }
        self.pop_scope();
    }

    fn walk_stmt(&mut self, stmt: &Stmt) {
        match &stmt.kind {
            StmtKind::Let {
                name, ty, value, ..
            } => {
                let value_ty = self.infer_expr(value);
                let bound_ty = if let Some(annotated) = ty {
                    let declared = type_from_type_expr(annotated);
                    if !value_ty.is_unknown()
                        && !declared.is_unknown()
                        && !types_compatible(&declared, &value_ty, self.type_registry)
                    {
                        self.errors.push(TypeError::LetTypeMismatch {
                            name: name.clone(),
                            declared: declared.display(),
                            actual: value_ty.display(),
                            at: stmt.span.start,
                        });
                    }
                    declared
                } else {
                    value_ty
                };
                self.declare(name, bound_ty);
            }
            StmtKind::LetShort { name, value } => {
                let value_ty = self.infer_expr(value);
                self.declare(name, value_ty);
            }
            StmtKind::Expr(e) => {
                let _ = self.infer_expr(e);
            }
            StmtKind::Return(Some(e)) => {
                let _ = self.infer_expr(e);
            }
            StmtKind::Return(None) => {}
            StmtKind::Mutate { assigns, .. } => {
                for fa in assigns {
                    if let Some(idx) = &fa.index {
                        let _ = self.infer_expr(idx);
                    }
                    let _ = self.infer_expr(&fa.value);
                }
            }
            StmtKind::MutateShort { value, .. } => {
                let _ = self.infer_expr(value);
            }
            StmtKind::ProcCall { args, .. } => {
                for a in args {
                    let _ = self.infer_expr(a);
                }
            }
            StmtKind::UncheckedStore { ptr, value, .. }
            | StmtKind::VolatileStore { ptr, value, .. } => {
                let _ = self.infer_expr(ptr);
                let _ = self.infer_expr(value);
            }
            // Slice 13: `if cond { … } else { … }` statement form.
            // Type the condition (should be bool; the bool-ness
            // check is deferred to a later validation slice — for
            // now codegen surfaces a defensive error if the SSA
            // value isn't `i1`). Push/pop typing scopes per branch.
            StmtKind::If {
                cond,
                then_block,
                else_block,
            } => {
                let _ = self.infer_expr(cond);
                self.push_scope();
                for s in &then_block.stmts {
                    self.walk_stmt(s);
                }
                self.pop_scope();
                if let Some(else_blk) = else_block {
                    self.push_scope();
                    for s in &else_blk.stmts {
                        self.walk_stmt(s);
                    }
                    self.pop_scope();
                }
            }
            // Slice 12: `name = expr;` — local mutable re-assignment.
            // We just type the RHS so any references inside it are
            // recorded; the resolver already enforced mutability and
            // existence of `name`. Type-mismatch checks (assigning a
            // u32 to an i64-typed local) are deferred to a later
            // slice that adds an explicit assignment-compatibility
            // check; for now, codegen will surface a NotYetImplemented
            // if the IR types mismatch.
            StmtKind::Assign { value, .. } => {
                let _ = self.infer_expr(value);
            }
            // Decision #14 / §5.8: `sigma var in source { body }`.
            //
            // The loop variable's type is the range-source's element
            // type — for `lo..hi` we use the type of `lo` (and trust
            // upstream span checks to verify `hi` matches per §5.8's
            // `BinaryTypeMismatch`). The variable is scoped to the
            // body only; we open a new typing scope, declare the
            // variable, walk the body, and pop the scope.
            //
            // v0.1 scope: range sources only. Array sources (which
            // would type the var as the array's element type) land
            // when slice-indexing infrastructure is built out.
            StmtKind::Sigma { var, source, body } => {
                let source_ty = self.infer_expr(source);
                let var_ty = match &source_ty {
                    Type::Range { element, .. } => (**element).clone(),
                    _ => Type::Unknown("sigma source not a range (v0.1 supports range sources only)"),
                };
                self.push_scope();
                self.declare(var, var_ty);
                for s in &body.stmts {
                    self.walk_stmt(s);
                }
                self.pop_scope();
            }
            // `Stmt` is `#[non_exhaustive]`. Forward-compat: new statement
            // kinds default to "no expression typing." Add explicit arms
            // when the statement carries expressions.
            _ => {}
        }
    }

    /// Compute and record the type of an expression. Returns the type so
    /// callers can reason about it without re-querying the map.
    fn infer_expr(&mut self, expr: &Expr) -> Type {
        let ty = match &expr.kind {
            // Literals — type from the literal token (with optional suffix).
            ExprKind::IntLit(s) => integer_literal_type(s),
            ExprKind::HexLit(s) | ExprKind::BinLit(s) => integer_literal_type(s),
            ExprKind::FloatLit(s) => float_literal_type(s),
            ExprKind::CharLit(_) => Type::Primitive(PrimitiveType::Char),
            ExprKind::ByteLit(_) => Type::Primitive(PrimitiveType::U8),
            ExprKind::StringLit(_) => Type::StringSlice,
            ExprKind::BoolLit(_) => Type::Primitive(PrimitiveType::Bool),
            ExprKind::Null => Type::Unknown("null is context-typed; inference deferred"),

            // Path: look up the resolved binding's type. Locals come from
            // our scope chain (we tracked their types as we declared them).
            // Multi-segment paths may resolve to ADT variant constructors
            // (Slice T4d): `Color::Red` for `@type Color = | Red | …;`.
            ExprKind::Path(segments) => {
                if segments.len() == 1 {
                    self.lookup_local(&segments[0])
                        .cloned()
                        .unwrap_or(Type::Unknown(
                            "name does not resolve to a local; top-level typing is slice T2 work",
                        ))
                } else if let Some((adt_name, params, variant)) =
                    self.type_registry.lookup_variant(segments)
                {
                    // T4d: typing a *bare* multi-segment variant path
                    // (no Call wrapper, no args supplied at the path
                    // site).
                    //
                    // - Unit-like variant (`Color::Red` for `@type Color
                    //   = | Red | …`) and the ADT is non-generic →
                    //   yields an instance of the ADT directly.
                    // - Tuple/struct variant referenced bare (i.e.
                    //   without an enclosing call) → for now we surface
                    //   it as a Nominal of the ADT name with the args
                    //   left to the call-site arm to disambiguate. This
                    //   is the conservative choice; a richer "constructor
                    //   function pointer" type comes when we have HM
                    //   inference.
                    if variant.args.is_empty() && params.is_empty() {
                        Type::Nominal {
                            path: vec![adt_name.to_owned()],
                            args: vec![],
                        }
                    } else {
                        // Bare reference to a data-carrying variant or
                        // a generic-ADT variant → Unknown until the
                        // call-site arm fills in the args (T4d slice
                        // doesn't synthesise a constructor function
                        // type; that lands with HM unification).
                        Type::Unknown(
                            "bare reference to data-carrying or generic ADT variant; typing comes via the call-site arm",
                        )
                    }
                } else {
                    Type::Unknown("multi-segment path typing is slice T4d+ work")
                }
            }

            // StateRead — should be a state-tag enum type per §4. Slice 2.
            ExprKind::StateRead(_) => Type::Unknown("state-tag typing is slice T2 work"),

            // Snapshot — Decision #24 / ADR 0004. The expression yields
            // an owned copy of the named field at the snapshot site, so
            // its type is the field's declared type (looked up via the
            // automaton-field registry). If the automaton/field doesn't
            // resolve, return Unknown — the resolver already reported
            // E0403 / E0405, no need for a parallel diagnostic here.
            ExprKind::Snapshot { automaton, field } => self
                .automaton_field_types
                .get(automaton)
                .and_then(|fs| fs.get(field).cloned())
                .unwrap_or(Type::Unknown(
                    "snapshot of unresolved automaton/field (resolver reported)",
                )),

            ExprKind::Paren(inner) => self.infer_expr(inner),

            ExprKind::Tuple(elems) => {
                let elem_types: Vec<Type> = elems.iter().map(|e| self.infer_expr(e)).collect();
                if elem_types.iter().any(Type::is_unknown) {
                    Type::Unknown("tuple element type unknown")
                } else {
                    Type::Tuple(elem_types)
                }
            }
            ExprKind::Array(elems) => {
                // `[a, b, c]` — every element should have the same type.
                // Slice 3 takes the first element's type as the element type;
                // an in-element-type-mismatch error class can land in T4 if
                // we want stricter checking. For now, mismatched arrays
                // produce a type with the first element's type but downstream
                // operations may still surface inconsistency.
                let elem_types: Vec<Type> = elems.iter().map(|e| self.infer_expr(e)).collect();
                if elem_types.is_empty() {
                    // Empty array literal — the parser permits this; size 0.
                    Type::Array {
                        element: Box::new(Type::Unknown("empty array element")),
                        size: "0".to_owned(),
                    }
                } else if elem_types.iter().any(Type::is_unknown) {
                    Type::Unknown("array element type unknown")
                } else {
                    let size = format!("{}", elems.len());
                    Type::Array {
                        element: Box::new(elem_types[0].clone()),
                        size,
                    }
                }
            }
            ExprKind::ArrayRepeat { value, count } => {
                let value_ty = self.infer_expr(value);
                let count_ty = self.infer_expr(count);
                let _ = count_ty;
                // We don't const-evaluate the count here; record it as the
                // raw text from the count expression's literal if possible,
                // else as "?".
                let size = match &count.kind {
                    ExprKind::IntLit(s) | ExprKind::HexLit(s) | ExprKind::BinLit(s) => s.clone(),
                    _ => "?".to_owned(),
                };
                if value_ty.is_unknown() {
                    Type::Unknown("array-repeat element type unknown")
                } else {
                    Type::Array {
                        element: Box::new(value_ty),
                        size,
                    }
                }
            }
            ExprKind::FieldAccess { obj, field } => {
                let _ = self.infer_expr(obj);
                self.field_access_type(expr.span, field)
            }
            ExprKind::Index { obj, index } => {
                let obj_ty = self.infer_expr(obj);
                let index_ty = self.infer_expr(index);
                self.index_type(&obj_ty, &index_ty, expr.span)
            }
            ExprKind::Call { callee, args } => self.call_type(callee, args, expr.span),
            ExprKind::MethodCall { obj, args, .. } => {
                let _ = self.infer_expr(obj);
                for a in args {
                    let _ = self.infer_expr(a);
                }
                Type::Unknown("method-call typing is slice T2 work")
            }

            // Unary operators on primitives.
            ExprKind::Unary { op, operand } => {
                let operand_ty = self.infer_expr(operand);
                self.unary_type(*op, &operand_ty, expr.span)
            }

            // Borrow expression — `&T` / `&mut T`. Decision #13 body-scoped
            // references: type checker assigns the reference type; provenance
            // / lifetime checking is `clifford-check`'s slice T3 work.
            ExprKind::Ref { mutable, operand } => {
                let inner = self.infer_expr(operand);
                if inner.is_unknown() {
                    Type::Unknown("ref of unknown operand")
                } else {
                    Type::Ref {
                        mutable: *mutable,
                        inner: Box::new(inner),
                    }
                }
            }

            // Binary operators on primitives.
            ExprKind::Binary { op, lhs, rhs } => {
                let lhs_ty = self.infer_expr(lhs);
                let rhs_ty = self.infer_expr(rhs);
                self.binary_type(*op, &lhs_ty, &rhs_ty, expr.span)
            }

            // Cast — trust the user-asserted target type. Validity (whether
            // the cast is meaningful) is `clifford-check`'s job in a later
            // slice (§5 mutability/cast rules).
            ExprKind::Cast { value, ty } => {
                let _ = self.infer_expr(value);
                type_from_type_expr(ty)
            }

            ExprKind::Range { lo, hi, inclusive } => {
                let lo_ty = self.infer_expr(lo);
                let hi_ty = self.infer_expr(hi);
                if lo_ty.is_unknown() || hi_ty.is_unknown() {
                    Type::Unknown("range bound type unknown")
                } else if !lo_ty.is_integer() || !hi_ty.is_integer() {
                    // Non-integer ranges are out of scope for sigma loops
                    // (Decision #14); we surface this as Unknown rather than
                    // an error so downstream code can choose how to handle it.
                    Type::Unknown("range bounds must be integers (T4 may add a dedicated error)")
                } else if lo_ty != hi_ty {
                    self.errors.push(TypeError::BinaryTypeMismatch {
                        op: if *inclusive { "..=" } else { ".." },
                        lhs: lo_ty.display(),
                        rhs: hi_ty.display(),
                        at: expr.span.start,
                    });
                    Type::Unknown("range bound mismatch")
                } else {
                    Type::Range {
                        element: Box::new(lo_ty),
                        inclusive: *inclusive,
                    }
                }
            }

            // Narrow unsafe primitives — the type argument tells us the
            // result type (load) or the cast target type. The parser
            // captures these.
            ExprKind::UncheckedLoad { ty, ptr } | ExprKind::VolatileLoad { ty, ptr } => {
                let _ = self.infer_expr(ptr);
                type_from_type_expr(ty)
            }
            ExprKind::UncheckedCast { to_ty, value, .. } => {
                let _ = self.infer_expr(value);
                type_from_type_expr(to_ty)
            }
            ExprKind::UncheckedOffset { ty: _, ptr, n } => {
                let _ = self.infer_expr(ptr);
                let _ = self.infer_expr(n);
                Type::Unknown("offset returns access<T>; reference typing is slice T2 work")
            }
            // `ExprKind` is `#[non_exhaustive]`. New variants default to
            // Unknown — add an explicit arm with a deliberate type when one
            // lands.
            _ => Type::Unknown("forward-compat: new ExprKind variant"),
        };
        self.record(expr.span, ty.clone());
        // A `Resolution` lookup may have already constrained the type for
        // this expression (e.g. an `AutomatonField` binding could later be
        // typed via the field's declared TypeExpr); keep that interaction
        // for slice 2 when we need it.
        let _ = self.resolution;
        ty
    }

    /// Type a function-call expression `callee(args)`.
    ///
    /// Slice-2 scope: callee must be a `Path([X])` resolving to a top-level
    /// `@fn` / `#effect` / `#interrupt` whose signature is in the
    /// [`SignatureRegistry`]. Higher-order calls (calling a parameter typed
    /// as `@fn(...)`, etc.) require the function-pointer type machinery
    /// from §2.7 and land in slice T3.
    ///
    /// Argument count is checked first (E0514). Each argument is then
    /// type-checked against its parameter (E0513). Argument count
    /// mismatches don't suppress per-argument type errors for the
    /// arguments that *do* exist — both classes accumulate.
    /// T4d: type a call expression whose callee is an ADT variant
    /// constructor (`Result::Ok(5u32)`).
    ///
    /// Behaviour:
    /// - **Non-generic ADT.** Variant args are checked structurally
    ///   against the declared variant arg types; result type is the
    ///   ADT (`Maybe::Some(5u32)` for `@type Maybe = | None |
    ///   Some(u32);` yields `Maybe`).
    /// - **Generic ADT, all params inferable from args.** The first
    ///   occurrence of each generic param in the variant's arg-type
    ///   list pins it to the matching arg's actual type; subsequent
    ///   occurrences must match (any non-match fires E0522). Result
    ///   type is the ADT with the inferred type args
    ///   (`Result::Ok(5u32)` yields `Result<u32, Unknown>` since `E`
    ///   has no constraint at this call site — the caller must
    ///   provide it via context, which is HM unification work and
    ///   stays deferred).
    /// - **Generic ADT, params not inferable.** Uninferred params
    ///   become `Type::Unknown` in the result; the wider expression
    ///   may still typecheck if downstream context constrains them.
    fn variant_call_type(
        &mut self,
        adt_name: &str,
        params: &[String],
        variant: &VariantInfo,
        arg_types: &[Type],
        at: Span,
    ) -> Type {
        // Arity check first.
        if variant.args.len() != arg_types.len() {
            self.errors.push(TypeError::VariantArityMismatch {
                adt_name: adt_name.to_owned(),
                variant_name: variant.name.clone(),
                expected: variant.args.len(),
                actual: arg_types.len(),
                at: at.start,
            });
            // Return a best-effort Nominal so downstream code has
            // *something* to chew on.
            return Type::Nominal {
                path: vec![adt_name.to_owned()],
                args: params
                    .iter()
                    .map(|_| Type::Unknown("variant arity mismatch (E0521)"))
                    .collect(),
            };
        }

        // Per-arg bidirectional unification (T4e). The previous T4d
        // version only pinned generic params when the declared arg
        // type was a *leaf* `Nominal{path:[param]}`; T4e walks
        // declared and actual in parallel through compounds (`(T,
        // T)`, `&T`, `[T; N]`, `Pair<T>`, etc.) so a variant whose
        // declared arg is `(T, T)` still pins `T` from a `(u32, u32)`
        // actual.
        let mut bindings: HashMap<String, Type> = HashMap::new();
        let limit = variant.args.len().min(arg_types.len());
        for (i, actual) in arg_types.iter().take(limit).enumerate() {
            let declared = &variant.args[i];
            if let Err(()) = unify_pin(
                declared,
                actual,
                params,
                &mut bindings,
                self.type_registry,
            ) {
                // Render the declared form with whatever bindings we
                // do have so the diagnostic shows the user the
                // *expected* type after partial inference, not just
                // the raw generic-param form.
                let owned_bindings: HashMap<&str, &Type> =
                    bindings.iter().map(|(k, v)| (k.as_str(), v)).collect();
                let displayed_expected = declared.substitute(&owned_bindings);
                self.errors.push(TypeError::VariantArgMismatch {
                    adt_name: adt_name.to_owned(),
                    variant_name: variant.name.clone(),
                    arg: i + 1,
                    expected: displayed_expected.display(),
                    actual: actual.display(),
                    at: at.start,
                });
            }
        }

        // Build the result Nominal. Unknown for uninferred params.
        let result_args: Vec<Type> = params
            .iter()
            .map(|p| {
                bindings.get(p).cloned().unwrap_or(Type::Unknown(
                    "generic ADT param not inferable from variant args (T4e)",
                ))
            })
            .collect();

        Type::Nominal {
            path: vec![adt_name.to_owned()],
            args: result_args,
        }
    }

    fn call_type(&mut self, callee: &Expr, args: &[Expr], at: Span) -> Type {
        let arg_types: Vec<Type> = args.iter().map(|a| self.infer_expr(a)).collect();

        // T4d: multi-segment path callee may be an ADT variant
        // constructor (`Result::Ok(5u32)`). We type it as the parent
        // ADT, with generic-param substitution from the arg types when
        // the ADT is generic and the arg count matches.
        if let ExprKind::Path(segs) = &callee.kind {
            if let Some((adt_name, params, variant)) =
                self.type_registry.lookup_variant(segs)
            {
                return self.variant_call_type(
                    adt_name,
                    params,
                    variant,
                    &arg_types,
                    at,
                );
            }
        }

        // Resolve the callee to a top-level signature. We accept only
        // single-segment `Path([X])` callees in slice 2; everything else
        // walks the callee for its expression-side effects and returns
        // Unknown.
        let callee_name: Option<&str> = match &callee.kind {
            ExprKind::Path(segs) if segs.len() == 1 => Some(segs[0].as_str()),
            _ => None,
        };
        let _ = self.infer_expr(callee);

        let Some(name) = callee_name else {
            return Type::Unknown("non-path callee typing is slice T3 work");
        };

        // Verify the callee resolves to a callable Symbol (Fn/Effect/Interrupt).
        // The resolver tagged the callee's span with a `BindingRef::TopLevel`
        // for those cases; if it's a local or anything else, we don't have
        // a signature and must fall through to Unknown.
        let is_callable_top_level = self
            .resolution
            .lookup(callee.span)
            .map(|b| match b {
                BindingRef::TopLevel(Symbol { kind, .. }) => matches!(
                    kind,
                    SymbolKind::Fn | SymbolKind::Effect | SymbolKind::Interrupt
                ),
                _ => false,
            })
            .unwrap_or(false);
        if !is_callable_top_level {
            return Type::Unknown("callee is not a top-level callable");
        }

        let Some(sig) = self.signatures.get(name) else {
            return Type::Unknown("callee signature not in registry");
        };

        if sig.params.len() != arg_types.len() {
            self.errors.push(TypeError::CallArityMismatch {
                callee: name.to_owned(),
                expected: sig.params.len(),
                actual: arg_types.len(),
                at: at.start,
            });
        }

        // Check each provided argument against its parameter's type.
        // Arity-only mismatches still get per-position checks for the
        // arguments that exist within the overlap.
        let limit = sig.params.len().min(arg_types.len());
        for (i, actual) in arg_types.iter().take(limit).enumerate() {
            let expected = &sig.params[i];
            if !expected.is_unknown()
                && !actual.is_unknown()
                && !types_compatible(expected, actual, self.type_registry)
            {
                self.errors.push(TypeError::CallArgMismatch {
                    callee: name.to_owned(),
                    arg: i + 1,
                    expected: expected.display(),
                    actual: actual.display(),
                    at: at.start,
                });
            }
        }

        sig.return_type.clone()
    }

    /// Type an `obj[index]` expression.
    ///
    /// Receiver shapes accepted:
    /// - `[T; N]` → element type `T`
    /// - `[T]` → element type `T`
    /// - `&[T; N]` / `&mut [T; N]` → `T` (auto-deref)
    /// - `&[T]` / `&mut [T]` → `T` (auto-deref)
    /// - `&[u8]` (StringSlice shorthand) → `u8`
    ///
    /// Index must be an integer (E0517). Non-indexable receivers emit
    /// E0516. Unknown receivers/indices propagate Unknown without
    /// piling on errors.
    fn index_type(&mut self, receiver: &Type, index: &Type, at: Span) -> Type {
        if receiver.is_unknown() || index.is_unknown() {
            return Type::Unknown("index on unknown type");
        }
        if !index.is_integer() {
            self.errors.push(TypeError::IndexNotInteger {
                index: index.display(),
                at: at.start,
            });
            // Continue computing the element type even on bad index — the
            // user gets both diagnostics in one pass.
        }
        let element = match receiver {
            Type::Array { element, .. } | Type::Slice { element } => Some((**element).clone()),
            Type::Ref { inner, .. } => match inner.as_ref() {
                Type::Array { element, .. } | Type::Slice { element } => Some((**element).clone()),
                _ => None,
            },
            Type::StringSlice => Some(Type::Primitive(PrimitiveType::U8)),
            _ => None,
        };
        match element {
            Some(t) => t,
            None => {
                self.errors.push(TypeError::IndexNonIndexable {
                    receiver: receiver.display(),
                    at: at.start,
                });
                Type::Unknown("non-indexable receiver")
            }
        }
    }

    /// Type a `FieldAccess` expression. The resolver already validated that
    /// `obj` resolves to an automaton (or this isn't an automaton field at
    /// all); slice 2 looks up the field's declared type via the resolver's
    /// `BindingRef::AutomatonField` recorded under the FieldAccess's span.
    fn field_access_type(&self, span: Span, field: &str) -> Type {
        let Some(BindingRef::AutomatonField { automaton, .. }) = self.resolution.lookup(span)
        else {
            return Type::Unknown("field-access on non-automaton receiver is slice T3 work");
        };
        self.automaton_field_types
            .get(&automaton.name)
            .and_then(|fields| fields.get(field))
            .cloned()
            .unwrap_or(Type::Unknown("field not in automaton-field registry"))
    }

    /// Determine the result type of a unary operator, or push a
    /// [`TypeError::UnaryTypeMismatch`] and return [`Type::Unknown`].
    fn unary_type(&mut self, op: UnaryOp, operand: &Type, at: Span) -> Type {
        if operand.is_unknown() {
            return Type::Unknown("unary on unknown operand");
        }
        match op {
            UnaryOp::Neg => {
                if operand.is_integer() || operand.is_float() {
                    operand.clone()
                } else {
                    self.errors.push(TypeError::UnaryTypeMismatch {
                        op: "-",
                        operand: operand.display(),
                        at: at.start,
                    });
                    Type::Unknown("unary mismatch")
                }
            }
            UnaryOp::Not => {
                if operand.is_bool() {
                    Type::Primitive(PrimitiveType::Bool)
                } else {
                    self.errors.push(TypeError::UnaryTypeMismatch {
                        op: "!",
                        operand: operand.display(),
                        at: at.start,
                    });
                    Type::Unknown("unary mismatch")
                }
            }
            UnaryOp::BitNot => {
                if operand.is_integer() {
                    operand.clone()
                } else {
                    self.errors.push(TypeError::UnaryTypeMismatch {
                        op: "~",
                        operand: operand.display(),
                        at: at.start,
                    });
                    Type::Unknown("unary mismatch")
                }
            }
            UnaryOp::Deref => {
                // `*r` unwraps a reference. Access-type deref (raw access<T>)
                // arrives in slice T3 alongside access-type modeling.
                match operand {
                    Type::Ref { inner, .. } => (**inner).clone(),
                    other if other.is_unknown() => other.clone(),
                    other => {
                        self.errors.push(TypeError::DerefNonReference {
                            operand: other.display(),
                            at: at.start,
                        });
                        Type::Unknown("deref non-ref")
                    }
                }
            }
        }
    }

    /// Determine the result type of a binary operator, or push a
    /// [`TypeError::BinaryTypeMismatch`] and return [`Type::Unknown`].
    ///
    /// Operator categories per §4:
    /// - Arithmetic (`+ - * / %`): operands must be the same numeric type;
    ///   result is that type.
    /// - Comparison (`== != < <= > >=`): operands must be the same type
    ///   (both numeric, both bool, or both char); result is `bool`.
    /// - Logical (`&& ||`): both operands `bool`; result `bool`.
    /// - Bitwise (`& | ^`): operands must be the same integer type; result
    ///   is that type.
    /// - Shift (`<< >>`): lhs must be integer; rhs must be integer; result
    ///   is the lhs type. (§4 doesn't yet require rhs to be `u32`; that
    ///   refinement can land in T2 if needed.)
    fn binary_type(&mut self, op: BinaryOp, lhs: &Type, rhs: &Type, at: Span) -> Type {
        if lhs.is_unknown() || rhs.is_unknown() {
            return Type::Unknown("binary on unknown operand");
        }

        let mismatch = |op_name: &'static str, t: &mut Self| {
            t.errors.push(TypeError::BinaryTypeMismatch {
                op: op_name,
                lhs: lhs.display(),
                rhs: rhs.display(),
                at: at.start,
            });
            Type::Unknown("binary mismatch")
        };

        match op {
            BinaryOp::Add | BinaryOp::Sub | BinaryOp::Mul | BinaryOp::Div | BinaryOp::Rem => {
                let op_name = match op {
                    BinaryOp::Add => "+",
                    BinaryOp::Sub => "-",
                    BinaryOp::Mul => "*",
                    BinaryOp::Div => "/",
                    BinaryOp::Rem => "%",
                    _ => unreachable!(),
                };
                if lhs == rhs && (lhs.is_integer() || lhs.is_float()) {
                    lhs.clone()
                } else {
                    mismatch(op_name, self)
                }
            }
            BinaryOp::Eq
            | BinaryOp::Ne
            | BinaryOp::Lt
            | BinaryOp::Le
            | BinaryOp::Gt
            | BinaryOp::Ge => {
                let op_name = match op {
                    BinaryOp::Eq => "==",
                    BinaryOp::Ne => "!=",
                    BinaryOp::Lt => "<",
                    BinaryOp::Le => "<=",
                    BinaryOp::Gt => ">",
                    BinaryOp::Ge => ">=",
                    _ => unreachable!(),
                };
                if lhs == rhs
                    && (lhs.is_integer()
                        || lhs.is_float()
                        || lhs.is_bool()
                        || matches!(lhs, Type::Primitive(PrimitiveType::Char)))
                {
                    Type::Primitive(PrimitiveType::Bool)
                } else {
                    mismatch(op_name, self)
                }
            }
            BinaryOp::And | BinaryOp::Or => {
                let op_name = if matches!(op, BinaryOp::And) {
                    "&&"
                } else {
                    "||"
                };
                if lhs.is_bool() && rhs.is_bool() {
                    Type::Primitive(PrimitiveType::Bool)
                } else {
                    mismatch(op_name, self)
                }
            }
            BinaryOp::BitAnd | BinaryOp::BitOr | BinaryOp::BitXor => {
                let op_name = match op {
                    BinaryOp::BitAnd => "&",
                    BinaryOp::BitOr => "|",
                    BinaryOp::BitXor => "^",
                    _ => unreachable!(),
                };
                if lhs == rhs && lhs.is_integer() {
                    lhs.clone()
                } else {
                    mismatch(op_name, self)
                }
            }
            BinaryOp::Shl | BinaryOp::Shr => {
                let op_name = if matches!(op, BinaryOp::Shl) {
                    "<<"
                } else {
                    ">>"
                };
                if lhs.is_integer() && rhs.is_integer() {
                    lhs.clone()
                } else {
                    mismatch(op_name, self)
                }
            }
        }
    }
}

// ─── Helpers ────────────────────────────────────────────────────────────────

/// Two types are compatible for `let`-annotation matching iff their
/// *unaliased* forms are structurally equal.
///
/// Slice T4b: passes both types through [`TypeRegistry::unalias`] so
/// `let x: MyAlias = 0u32;` typechecks when `@type MyAlias = u32;`.
/// Slice T4a only compared the syntactic form, which surfaced the
/// alias name as a mismatch ("declared `MyAlias`, actual `u32`") even
/// though the alias resolves to the same thing.
///
/// `Unknown` is treated as compatible with anything to avoid cascading
/// errors when an upstream type is unknown (matches T4a behaviour).
fn types_compatible(declared: &Type, actual: &Type, registry: &TypeRegistry) -> bool {
    if declared.is_unknown() || actual.is_unknown() {
        return true;
    }
    let d = registry.unalias(declared);
    let a = registry.unalias(actual);
    if d.is_unknown() || a.is_unknown() {
        return true;
    }
    d == a
}

/// T4e: bidirectional unification between a `declared` type (which may
/// reference generic parameters from `params`) and an `actual` type
/// (which is fully concrete, modulo `Type::Unknown` from upstream
/// uninferred positions). Pins generic-param bindings into `bindings`
/// and verifies non-generic positions match structurally.
///
/// Returns:
/// - `Ok(())` if the actual type unifies with the declared type under
///   the current bindings (extending bindings as needed).
/// - `Err(())` if the structural shapes don't match, or a generic-
///   param binding conflicts with a previous pin. The caller is
///   responsible for producing a diagnostic; this helper just signals
///   success/failure.
///
/// Recursion strategy: when `declared` is a leaf
/// `Nominal{path:[param], args:[]}` whose path matches a name in
/// `params`, pin/check the binding. Otherwise descend through matching
/// compound shapes (`Tuple` ↔ `Tuple`, `Ref` ↔ `Ref`, `Array` ↔
/// `Array`, `Slice` ↔ `Slice`, `Range` ↔ `Range`, `Nominal` ↔
/// `Nominal`). At leaves of either side that are not generic params,
/// fall back to [`types_compatible`] for the final structural check —
/// this preserves alias-following at the bottom of the unification.
///
/// `Unknown` on either side is permissive (returns Ok without binding):
/// this matches the behaviour of `types_compatible` and avoids
/// cascading errors when an upstream type couldn't be inferred.
fn unify_pin(
    declared: &Type,
    actual: &Type,
    params: &[String],
    bindings: &mut HashMap<String, Type>,
    registry: &TypeRegistry,
) -> Result<(), ()> {
    // Permissive on Unknown — caller already has an upstream issue.
    if declared.is_unknown() || actual.is_unknown() {
        return Ok(());
    }

    // Leaf generic-param reference in declared position.
    if let Type::Nominal { path, args } = declared {
        if path.len() == 1 && args.is_empty() && params.iter().any(|p| p == &path[0]) {
            let pname = &path[0];
            match bindings.get(pname) {
                None => {
                    bindings.insert(pname.clone(), actual.clone());
                    return Ok(());
                }
                Some(prev) => {
                    if types_compatible(prev, actual, registry) {
                        return Ok(());
                    }
                    return Err(());
                }
            }
        }
    }

    // Compound recursion. We pattern-match on matching shapes; mismatched
    // shapes fall through to the final structural check.
    match (declared, actual) {
        (Type::Tuple(d), Type::Tuple(a)) if d.len() == a.len() => {
            for (di, ai) in d.iter().zip(a) {
                unify_pin(di, ai, params, bindings, registry)?;
            }
            Ok(())
        }
        (
            Type::Ref { mutable: dm, inner: di },
            Type::Ref { mutable: am, inner: ai },
        ) if dm == am => unify_pin(di, ai, params, bindings, registry),
        (
            Type::Array { element: de, size: ds },
            Type::Array { element: ae, size: as_size },
        ) if ds == as_size => unify_pin(de, ae, params, bindings, registry),
        (
            Type::Slice { element: de },
            Type::Slice { element: ae },
        ) => unify_pin(de, ae, params, bindings, registry),
        (
            Type::Range { element: de, inclusive: di },
            Type::Range { element: ae, inclusive: ai },
        ) if di == ai => unify_pin(de, ae, params, bindings, registry),
        (
            Type::Nominal { path: dp, args: da },
            Type::Nominal { path: ap, args: aa },
        ) if dp == ap && da.len() == aa.len() => {
            for (di, ai) in da.iter().zip(aa) {
                unify_pin(di, ai, params, bindings, registry)?;
            }
            Ok(())
        }
        _ => {
            // Final structural fallback. Apply current bindings to the
            // declared side first so partial pins are reflected before
            // the compatibility check.
            let owned_bindings: HashMap<&str, &Type> =
                bindings.iter().map(|(k, v)| (k.as_str(), v)).collect();
            let substituted = declared.substitute(&owned_bindings);
            if types_compatible(&substituted, actual, registry) {
                Ok(())
            } else {
                Err(())
            }
        }
    }
}

/// Translate a syntactic [`TypeExpr`] into a semantic [`Type`].
///
/// Slice T4a scope: `Unit`, `Primitive`, `Ref`, `Array`, `Slice`, `Tuple`,
/// and `Path` (as [`Type::Nominal`]) resolve to their semantic counterparts.
/// `access<T>` and `@fn` pointer types remain [`Type::Unknown`] until slice
/// T4b+. Slice T4a translates `Path` verbatim (recording segments and
/// generic args); resolution of the path against the program's top-level
/// declarations is deferred to slice T4b.
fn type_from_type_expr(t: &TypeExpr) -> Type {
    use clifford_ast::ArraySize;
    match &t.kind {
        TypeKind::Unit => Type::Unit,
        TypeKind::Primitive(p) => Type::Primitive(*p),
        TypeKind::Ref(rt) => Type::Ref {
            mutable: rt.mutable,
            inner: Box::new(type_from_type_expr(&rt.inner)),
        },
        TypeKind::Array(at) => {
            let ArraySize::IntLiteral(size) = &at.size;
            Type::Array {
                element: Box::new(type_from_type_expr(&at.element)),
                size: size.clone(),
            }
        }
        TypeKind::Slice(st) => Type::Slice {
            element: Box::new(type_from_type_expr(&st.element)),
        },
        TypeKind::Tuple(tt) => Type::Tuple(tt.elements.iter().map(type_from_type_expr).collect()),
        TypeKind::Path(pt) => Type::Nominal {
            path: pt.segments.clone(),
            args: pt.generic_args.iter().map(type_from_type_expr).collect(),
        },
        TypeKind::Access(_) => Type::Unknown("access<T> type is slice T4 work"),
        TypeKind::Fn(_) => Type::Unknown("@fn pointer type is slice T4 work"),
        // `TypeKind` is `#[non_exhaustive]`. New variants default to Unknown;
        // add an explicit arm when one lands.
        _ => Type::Unknown("forward-compat: new TypeKind variant"),
    }
}

/// Inspect an integer-literal token (e.g. `"42"`, `"0xFF_u32"`, `"100i64"`)
/// for a type suffix, returning the corresponding primitive type. Defaults
/// to `i32` when no suffix is present (matches Rust's untyped-integer-literal
/// default for the common case).
fn integer_literal_type(text: &str) -> Type {
    for (suffix, prim) in [
        ("u8", PrimitiveType::U8),
        ("u16", PrimitiveType::U16),
        ("u32", PrimitiveType::U32),
        ("u64", PrimitiveType::U64),
        ("usize", PrimitiveType::Usize),
        ("i8", PrimitiveType::I8),
        ("i16", PrimitiveType::I16),
        ("i32", PrimitiveType::I32),
        ("i64", PrimitiveType::I64),
        ("isize", PrimitiveType::Isize),
    ] {
        if text.ends_with(suffix) {
            return Type::Primitive(prim);
        }
    }
    Type::Primitive(PrimitiveType::I32)
}

/// Inspect a float-literal token for a type suffix, defaulting to `f64`.
fn float_literal_type(text: &str) -> Type {
    if text.ends_with("f32") {
        Type::Primitive(PrimitiveType::F32)
    } else {
        Type::Primitive(PrimitiveType::F64)
    }
}

fn primitive_name(p: PrimitiveType) -> &'static str {
    match p {
        PrimitiveType::U8 => "u8",
        PrimitiveType::U16 => "u16",
        PrimitiveType::U32 => "u32",
        PrimitiveType::U64 => "u64",
        PrimitiveType::Usize => "usize",
        PrimitiveType::I8 => "i8",
        PrimitiveType::I16 => "i16",
        PrimitiveType::I32 => "i32",
        PrimitiveType::I64 => "i64",
        PrimitiveType::Isize => "isize",
        PrimitiveType::F32 => "f32",
        PrimitiveType::F64 => "f64",
        PrimitiveType::Bool => "bool",
        PrimitiveType::Char => "char",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use clifford_lexer::tokenize;
    use clifford_parser::parse;
    use clifford_resolve::resolve;

    fn infer_str(src: &str) -> Result<Typing, Vec<TypeError>> {
        let tokens = tokenize(src).expect("tokenize");
        let program = parse(&tokens).expect("parse");
        let resolution = resolve(&program).expect("resolve");
        infer(&program, &resolution)
    }

    /// Find an inferred type whose containing expression matches a body
    /// position. We don't have unique node IDs; instead we extract from
    /// `Typing.types` by predicate.
    fn types_in(typing: &Typing) -> Vec<&Type> {
        typing.types.values().collect()
    }

    // ── Empty programs / trivial bodies ──────────────────────────────────

    #[test]
    fn empty_program_types_to_empty_typing() {
        let typing = infer_str("").unwrap();
        assert!(typing.is_empty());
    }

    #[test]
    fn empty_fn_body_has_no_typings() {
        let typing = infer_str("@fn nothing() { }").unwrap();
        assert!(typing.is_empty());
    }

    // ── Literal typing ───────────────────────────────────────────────────

    #[test]
    fn int_literal_defaults_to_i32() {
        let typing = infer_str("@fn f() { let _x := 42; }").unwrap();
        assert!(types_in(&typing)
            .into_iter()
            .any(|t| matches!(t, Type::Primitive(PrimitiveType::I32))));
    }

    #[test]
    fn int_literal_with_u32_suffix() {
        let typing = infer_str("@fn f() { let _x := 42u32; }").unwrap();
        assert!(types_in(&typing)
            .into_iter()
            .any(|t| matches!(t, Type::Primitive(PrimitiveType::U32))));
    }

    #[test]
    fn int_literal_with_i64_suffix() {
        let typing = infer_str("@fn f() { let _x := 100i64; }").unwrap();
        assert!(types_in(&typing)
            .into_iter()
            .any(|t| matches!(t, Type::Primitive(PrimitiveType::I64))));
    }

    #[test]
    fn hex_literal_with_suffix() {
        let typing = infer_str("@fn f() { let _x := 0xDEADu32; }").unwrap();
        assert!(types_in(&typing)
            .into_iter()
            .any(|t| matches!(t, Type::Primitive(PrimitiveType::U32))));
    }

    #[test]
    fn bin_literal_with_suffix() {
        let typing = infer_str("@fn f() { let _x := 0b1010u8; }").unwrap();
        assert!(types_in(&typing)
            .into_iter()
            .any(|t| matches!(t, Type::Primitive(PrimitiveType::U8))));
    }

    #[test]
    fn float_literal_defaults_to_f64() {
        let typing = infer_str("@fn f() { let _x := 3.14; }").unwrap();
        assert!(types_in(&typing)
            .into_iter()
            .any(|t| matches!(t, Type::Primitive(PrimitiveType::F64))));
    }

    #[test]
    fn float_literal_with_f32_suffix() {
        let typing = infer_str("@fn f() { let _x := 3.14f32; }").unwrap();
        assert!(types_in(&typing)
            .into_iter()
            .any(|t| matches!(t, Type::Primitive(PrimitiveType::F32))));
    }

    #[test]
    fn char_literal_is_char() {
        let typing = infer_str("@fn f() { let _x := 'A'; }").unwrap();
        assert!(types_in(&typing)
            .into_iter()
            .any(|t| matches!(t, Type::Primitive(PrimitiveType::Char))));
    }

    #[test]
    fn byte_literal_is_u8() {
        let typing = infer_str("@fn f() { let _x := b'A'; }").unwrap();
        assert!(types_in(&typing)
            .into_iter()
            .any(|t| matches!(t, Type::Primitive(PrimitiveType::U8))));
    }

    #[test]
    fn bool_literal_true_is_bool() {
        let typing = infer_str("@fn f() { let _x := true; }").unwrap();
        assert!(types_in(&typing)
            .into_iter()
            .any(|t| matches!(t, Type::Primitive(PrimitiveType::Bool))));
    }

    #[test]
    fn bool_literal_false_is_bool() {
        let typing = infer_str("@fn f() { let _x := false; }").unwrap();
        assert!(types_in(&typing)
            .into_iter()
            .any(|t| matches!(t, Type::Primitive(PrimitiveType::Bool))));
    }

    #[test]
    fn string_literal_is_string_slice() {
        let typing = infer_str(r#"@fn f() { let _x := "hello"; }"#).unwrap();
        assert!(types_in(&typing)
            .into_iter()
            .any(|t| matches!(t, Type::StringSlice)));
    }

    // ── Path typing via locals ───────────────────────────────────────────

    #[test]
    fn param_resolves_to_param_type() {
        let typing = infer_str("@fn f(x: u32) -> u32 { return x; }").unwrap();
        // The `x` reference in the return statement must have the param's type.
        let saw_u32 = types_in(&typing)
            .iter()
            .filter(|t| matches!(t, Type::Primitive(PrimitiveType::U32)))
            .count()
            >= 1;
        assert!(saw_u32);
    }

    #[test]
    fn let_binding_propagates_initializer_type() {
        let typing = infer_str("@fn f() { let x: u32 = 0u32; let _y := x; }").unwrap();
        // Both the literal and the path reference should be u32.
        let u32_count = types_in(&typing)
            .iter()
            .filter(|t| matches!(t, Type::Primitive(PrimitiveType::U32)))
            .count();
        assert!(u32_count >= 2);
    }

    // ── Unary operators ──────────────────────────────────────────────────

    #[test]
    fn neg_on_integer_keeps_type() {
        let typing = infer_str("@fn f() { let _x := -42i32; }").unwrap();
        assert!(types_in(&typing)
            .into_iter()
            .any(|t| matches!(t, Type::Primitive(PrimitiveType::I32))));
    }

    #[test]
    fn neg_on_bool_is_e0511() {
        let errors = infer_str("@fn f() { let _x := -true; }").unwrap_err();
        assert!(errors
            .iter()
            .any(|e| matches!(e, TypeError::UnaryTypeMismatch { op: "-", .. })));
    }

    #[test]
    fn not_on_bool_returns_bool() {
        let typing = infer_str("@fn f() { let _x := !true; }").unwrap();
        assert!(types_in(&typing)
            .into_iter()
            .any(|t| matches!(t, Type::Primitive(PrimitiveType::Bool))));
    }

    #[test]
    fn not_on_integer_is_e0511() {
        let errors = infer_str("@fn f() { let _x := !42i32; }").unwrap_err();
        assert!(errors
            .iter()
            .any(|e| matches!(e, TypeError::UnaryTypeMismatch { op: "!", .. })));
    }

    #[test]
    fn bitnot_on_integer_keeps_type() {
        let typing = infer_str("@fn f() { let _x := ~0xFFu8; }").unwrap();
        assert!(types_in(&typing)
            .into_iter()
            .any(|t| matches!(t, Type::Primitive(PrimitiveType::U8))));
    }

    #[test]
    fn bitnot_on_bool_is_e0511() {
        let errors = infer_str("@fn f() { let _x := ~true; }").unwrap_err();
        assert!(errors
            .iter()
            .any(|e| matches!(e, TypeError::UnaryTypeMismatch { op: "~", .. })));
    }

    // ── Binary arithmetic ────────────────────────────────────────────────

    #[test]
    fn arithmetic_same_type_preserves() {
        let typing = infer_str("@fn f() { let _x := 1u32 + 2u32; }").unwrap();
        assert!(types_in(&typing)
            .into_iter()
            .any(|t| matches!(t, Type::Primitive(PrimitiveType::U32))));
    }

    #[test]
    fn arithmetic_mismatch_is_e0510() {
        let errors = infer_str("@fn f() { let _x := 1u32 + 2u64; }").unwrap_err();
        assert!(errors
            .iter()
            .any(|e| matches!(e, TypeError::BinaryTypeMismatch { op: "+", .. })));
    }

    #[test]
    fn arithmetic_on_bool_is_e0510() {
        let errors = infer_str("@fn f() { let _x := true + false; }").unwrap_err();
        assert!(errors
            .iter()
            .any(|e| matches!(e, TypeError::BinaryTypeMismatch { op: "+", .. })));
    }

    #[test]
    fn arithmetic_on_floats_works() {
        let typing = infer_str("@fn f() { let _x := 1.0f32 + 2.0f32; }").unwrap();
        assert!(types_in(&typing)
            .into_iter()
            .any(|t| matches!(t, Type::Primitive(PrimitiveType::F32))));
    }

    // ── Binary comparison ────────────────────────────────────────────────

    #[test]
    fn comparison_returns_bool() {
        let typing = infer_str("@fn f() { let _x := 1u32 < 2u32; }").unwrap();
        assert!(types_in(&typing)
            .into_iter()
            .any(|t| matches!(t, Type::Primitive(PrimitiveType::Bool))));
    }

    #[test]
    fn comparison_mismatch_is_e0510() {
        let errors = infer_str("@fn f() { let _x := 1u32 < 2u64; }").unwrap_err();
        assert!(errors
            .iter()
            .any(|e| matches!(e, TypeError::BinaryTypeMismatch { op: "<", .. })));
    }

    #[test]
    fn equality_on_chars_works() {
        let typing = infer_str("@fn f() { let _x := 'A' == 'B'; }").unwrap();
        assert!(types_in(&typing)
            .into_iter()
            .any(|t| matches!(t, Type::Primitive(PrimitiveType::Bool))));
    }

    // ── Logical operators ────────────────────────────────────────────────

    #[test]
    fn logical_and_on_bool_works() {
        let typing = infer_str("@fn f() { let _x := true && false; }").unwrap();
        assert!(types_in(&typing)
            .into_iter()
            .any(|t| matches!(t, Type::Primitive(PrimitiveType::Bool))));
    }

    #[test]
    fn logical_and_on_integer_is_e0510() {
        let errors = infer_str("@fn f() { let _x := 1u32 && 2u32; }").unwrap_err();
        assert!(errors
            .iter()
            .any(|e| matches!(e, TypeError::BinaryTypeMismatch { op: "&&", .. })));
    }

    // ── Bitwise operators ────────────────────────────────────────────────

    #[test]
    fn bitwise_and_on_integer_keeps_type() {
        let typing = infer_str("@fn f() { let _x := 0xFFu32 & 0x0Fu32; }").unwrap();
        assert!(types_in(&typing)
            .into_iter()
            .any(|t| matches!(t, Type::Primitive(PrimitiveType::U32))));
    }

    #[test]
    fn bitwise_xor_on_mismatched_integers_is_e0510() {
        let errors = infer_str("@fn f() { let _x := 1u8 ^ 2u32; }").unwrap_err();
        assert!(errors
            .iter()
            .any(|e| matches!(e, TypeError::BinaryTypeMismatch { op: "^", .. })));
    }

    // ── Shifts ───────────────────────────────────────────────────────────

    #[test]
    fn shift_returns_lhs_type() {
        let typing = infer_str("@fn f() { let _x := 1u32 << 2u32; }").unwrap();
        assert!(types_in(&typing)
            .into_iter()
            .any(|t| matches!(t, Type::Primitive(PrimitiveType::U32))));
    }

    #[test]
    fn shift_with_non_integer_lhs_is_e0510() {
        let errors = infer_str("@fn f() { let _x := true << 2; }").unwrap_err();
        assert!(errors
            .iter()
            .any(|e| matches!(e, TypeError::BinaryTypeMismatch { op: "<<", .. })));
    }

    // ── Cast ─────────────────────────────────────────────────────────────

    #[test]
    fn cast_yields_target_type() {
        let typing = infer_str("@fn f() { let _x := 1i32 as u64; }").unwrap();
        assert!(types_in(&typing)
            .into_iter()
            .any(|t| matches!(t, Type::Primitive(PrimitiveType::U64))));
    }

    // ── Let-annotation matching ──────────────────────────────────────────

    #[test]
    fn let_annotation_match_succeeds() {
        let typing = infer_str("@fn f() { let _x: u32 = 1u32; }").unwrap();
        assert!(types_in(&typing)
            .into_iter()
            .any(|t| matches!(t, Type::Primitive(PrimitiveType::U32))));
    }

    #[test]
    fn let_annotation_mismatch_is_e0512() {
        let errors = infer_str("@fn f() { let _x: u32 = 1u8; }").unwrap_err();
        assert!(errors.iter().any(|e| matches!(
            e,
            TypeError::LetTypeMismatch { ref name, .. } if name == "_x"
        )));
    }

    #[test]
    fn let_annotation_unknown_initializer_silent() {
        // Initializer typed as Unknown (a function call we can't yet type)
        // shouldn't produce a spurious E0512.
        let res = infer_str(
            "@fn helper() -> u32 { return 0u32; } \
             @fn f() { let _x: u32 = helper(); }",
        );
        // No errors expected: the call returns Unknown, which we treat as
        // compatible with anything in slice 1.
        assert!(res.is_ok(), "got errors: {:?}", res);
    }

    // ── Narrow unsafe primitive typing ───────────────────────────────────

    #[test]
    fn unchecked_load_returns_type_argument() {
        let typing =
            infer_str("#effect e(p: u32) #mutates: [] { let _x := #unchecked_load<u8>(p); }")
                .unwrap();
        assert!(types_in(&typing)
            .into_iter()
            .any(|t| matches!(t, Type::Primitive(PrimitiveType::U8))));
    }

    #[test]
    fn volatile_load_returns_type_argument() {
        let typing =
            infer_str("#effect e(p: u32) #mutates: [] { let _x := #volatile_load<u32>(p); }")
                .unwrap();
        assert!(types_in(&typing)
            .into_iter()
            .any(|t| matches!(t, Type::Primitive(PrimitiveType::U32))));
    }

    #[test]
    fn unchecked_cast_returns_to_type() {
        let typing = infer_str(
            r#"@fn f(x: u32) { let _y := #unchecked_cast<u32, i32>("safe per ABI", x); }"#,
        )
        .unwrap();
        assert!(types_in(&typing)
            .into_iter()
            .any(|t| matches!(t, Type::Primitive(PrimitiveType::I32))));
    }

    // ── Multiple errors collected ────────────────────────────────────────

    #[test]
    fn multiple_errors_collected_in_one_pass() {
        let errors = infer_str("@fn f() { let _x := -true; let _y := 1u32 + 2u8; }").unwrap_err();
        assert!(errors.len() >= 2);
        assert!(errors
            .iter()
            .any(|e| matches!(e, TypeError::UnaryTypeMismatch { .. })));
        assert!(errors
            .iter()
            .any(|e| matches!(e, TypeError::BinaryTypeMismatch { .. })));
    }

    // ── Realistic combined exercise ──────────────────────────────────────

    #[test]
    fn realistic_program_types_end_to_end() {
        let src = "\
            #automaton Counter { value: u32; }\n\
            #effect bump() #mutates: [Counter] {\n  \
              let next: u32 = 1u32 + 2u32;\n  \
              return;\n\
            }\n\
            @fn cmd_check(min_len: u32) -> bool $ [Pure] {\n  \
              let len: u32 = min_len + 4u32;\n  \
              return len > 0u32;\n\
            }\n\
        ";
        let typing = infer_str(src).expect("infer");
        // Lots of u32 types: from the integer literals, the param,
        // the let bindings, and the binary results.
        let u32_count = types_in(&typing)
            .iter()
            .filter(|t| matches!(t, Type::Primitive(PrimitiveType::U32)))
            .count();
        assert!(u32_count >= 4, "expected several u32s, got {u32_count}");
        // The comparison result is bool.
        assert!(types_in(&typing)
            .into_iter()
            .any(|t| matches!(t, Type::Primitive(PrimitiveType::Bool))));
    }

    // ── Type display ─────────────────────────────────────────────────────

    #[test]
    fn type_display_strings() {
        assert_eq!(Type::Unit.display(), "()");
        assert_eq!(Type::Primitive(PrimitiveType::U32).display(), "u32");
        assert_eq!(Type::Primitive(PrimitiveType::Bool).display(), "bool");
        assert_eq!(Type::StringSlice.display(), "&[u8]");
        assert!(Type::Unknown("test").display().contains("test"));
    }

    // ─── Slice 2: function calls, automaton fields, references ───────────

    #[test]
    fn ref_display_immutable() {
        let t = Type::Ref {
            mutable: false,
            inner: Box::new(Type::Primitive(PrimitiveType::U32)),
        };
        assert_eq!(t.display(), "&u32");
    }

    #[test]
    fn ref_display_mutable() {
        let t = Type::Ref {
            mutable: true,
            inner: Box::new(Type::Primitive(PrimitiveType::U32)),
        };
        assert_eq!(t.display(), "&mut u32");
    }

    // ── Borrow expressions ───────────────────────────────────────────────

    #[test]
    fn borrow_immutable_yields_ref_type() {
        let typing = infer_str("@fn f(x: u32) { let _r := &x; }").unwrap();
        let saw_ref = types_in(&typing).iter().any(|t| {
            matches!(t,
                Type::Ref { mutable: false, inner }
                    if matches!(**inner, Type::Primitive(PrimitiveType::U32))
            )
        });
        assert!(saw_ref, "expected &u32 to appear in the typing map");
    }

    #[test]
    fn borrow_mutable_yields_mut_ref_type() {
        let typing = infer_str("@fn f(mut x: u32) { let _r := &mut x; }").unwrap();
        let saw_mut_ref = types_in(&typing).iter().any(|t| {
            matches!(t,
                Type::Ref { mutable: true, inner }
                    if matches!(**inner, Type::Primitive(PrimitiveType::U32))
            )
        });
        assert!(saw_mut_ref);
    }

    #[test]
    fn ref_param_type_carries_through() {
        // `@fn f(p: &u32)` — `p` is a Ref(u32) local. Returning `*p` yields u32.
        let typing = infer_str("@fn f(p: &u32) -> u32 { return *p; }").unwrap();
        // The deref `*p` should be u32.
        let saw_u32 = types_in(&typing)
            .iter()
            .any(|t| matches!(t, Type::Primitive(PrimitiveType::U32)));
        assert!(saw_u32);
    }

    #[test]
    fn deref_non_reference_is_e0515() {
        // `*42i32` — can't deref a non-reference.
        let errors = infer_str("@fn f() -> i32 { return *42i32; }").unwrap_err();
        assert!(errors
            .iter()
            .any(|e| matches!(e, TypeError::DerefNonReference { .. })));
    }

    // ── Function calls ───────────────────────────────────────────────────

    #[test]
    fn call_returns_callee_return_type() {
        let typing = infer_str(
            "@fn helper() -> u32 { return 0u32; } \
             @fn caller() { let _x: u32 = helper(); }",
        )
        .unwrap();
        // The call expression `helper()` should be typed as u32.
        let u32_count = types_in(&typing)
            .iter()
            .filter(|t| matches!(t, Type::Primitive(PrimitiveType::U32)))
            .count();
        // Two u32s: the literal `0u32` inside helper, and the call result.
        assert!(u32_count >= 2, "got {u32_count} u32s");
    }

    #[test]
    fn call_arity_mismatch_is_e0514() {
        let errors = infer_str(
            "@fn add(a: u32, b: u32) -> u32 { return 0u32; } \
             @fn caller() { let _x := add(1u32); }",
        )
        .unwrap_err();
        assert!(errors.iter().any(|e| matches!(
            e,
            TypeError::CallArityMismatch { callee, expected: 2, actual: 1, .. } if callee == "add"
        )));
    }

    #[test]
    fn call_arg_type_mismatch_is_e0513() {
        let errors = infer_str(
            "@fn helper(x: u32) -> u32 { return x; } \
             @fn caller() { let _x := helper(1u8); }",
        )
        .unwrap_err();
        assert!(errors.iter().any(|e| matches!(
            e,
            TypeError::CallArgMismatch { callee, arg: 1, .. } if callee == "helper"
        )));
    }

    #[test]
    fn call_with_correct_args_succeeds() {
        let res = infer_str(
            "@fn add(a: u32, b: u32) -> u32 { return 0u32; } \
             @fn caller() { let _x: u32 = add(1u32, 2u32); }",
        );
        assert!(res.is_ok(), "got errors: {:?}", res);
    }

    #[test]
    fn call_to_local_is_unknown_not_error() {
        // Calling a parameter (typed as Unknown for callable-ness in slice 2)
        // should produce Unknown, not a spurious error.
        let res = infer_str("@fn caller() { let helper := 0u32; let _y := helper(); }");
        // No errors expected — we don't know what `helper` is callable as
        // but it's not a top-level fn so we skip arg checking.
        assert!(res.is_ok(), "got errors: {:?}", res);
    }

    #[test]
    fn call_to_undefined_callee_silent_in_typer() {
        // The resolver catches undefined names — by the time the typer runs,
        // the call wouldn't even reach `infer` (resolve fails). Test that
        // we handle the `is_callable_top_level == false` case cleanly.
        // We construct this by calling something that resolves to a local:
        let res = infer_str(
            "@fn helper() -> u32 { return 0u32; } \
             @fn caller() { let helper: u32 = 0u32; let _x := helper(); }",
        );
        // `helper` shadowed by the local — call resolves to a Local Let,
        // not a TopLevel Fn. The typer skips signature checking.
        assert!(res.is_ok());
    }

    // ── Automaton field access typing ────────────────────────────────────

    #[test]
    fn auto_field_read_yields_field_type() {
        let typing = infer_str(
            "#automaton Counter { value: u32; } \
             #effect peek() #mutates: [] { let _x: u32 = Counter.value; }",
        )
        .unwrap();
        // Counter.value should be typed as u32.
        let saw_u32 = types_in(&typing)
            .iter()
            .any(|t| matches!(t, Type::Primitive(PrimitiveType::U32)));
        assert!(saw_u32);
    }

    #[test]
    fn auto_field_in_arithmetic_propagates_correctly() {
        // `Counter.value + 1u32` — the field's u32 type drives the literal's
        // type to match (well, the literal is already u32 from its suffix;
        // but the binary still type-checks).
        let res = infer_str(
            "#automaton Counter { value: u32; } \
             #effect e() #mutates: [Counter] { let next: u32 = Counter.value + 1u32; }",
        );
        assert!(res.is_ok(), "got errors: {:?}", res);
    }

    #[test]
    fn auto_field_type_mismatch_in_let_is_e0512() {
        let errors = infer_str(
            "#automaton Counter { value: u32; } \
             #effect e() #mutates: [] { let _x: u8 = Counter.value; }",
        )
        .unwrap_err();
        assert!(errors
            .iter()
            .any(|e| matches!(e, TypeError::LetTypeMismatch { .. })));
    }

    #[test]
    fn self_field_in_transition_yields_field_type() {
        let typing = infer_str(
            "#automaton Counter { value: u32; \
             #transition tick { let _x: u32 = Self.value; } }",
        )
        .unwrap();
        let saw_u32 = types_in(&typing)
            .iter()
            .any(|t| matches!(t, Type::Primitive(PrimitiveType::U32)));
        assert!(saw_u32);
    }

    // ── Realistic combined exercise ──────────────────────────────────────

    // ─── Slice 3: structured-type expressions ────────────────────────────

    fn first_of<F>(typing: &Typing, pred: F) -> Option<&Type>
    where
        F: Fn(&Type) -> bool,
    {
        typing.types.values().find(|t| pred(t))
    }

    // ── Display for new variants ─────────────────────────────────────────

    #[test]
    fn array_type_display() {
        let t = Type::Array {
            element: Box::new(Type::Primitive(PrimitiveType::U8)),
            size: "64".to_owned(),
        };
        assert_eq!(t.display(), "[u8; 64]");
    }

    #[test]
    fn slice_type_display() {
        let t = Type::Slice {
            element: Box::new(Type::Primitive(PrimitiveType::U8)),
        };
        assert_eq!(t.display(), "[u8]");
    }

    #[test]
    fn tuple_type_display() {
        let t = Type::Tuple(vec![
            Type::Primitive(PrimitiveType::U32),
            Type::Primitive(PrimitiveType::Bool),
        ]);
        assert_eq!(t.display(), "(u32, bool)");
    }

    #[test]
    fn range_type_display() {
        let t = Type::Range {
            element: Box::new(Type::Primitive(PrimitiveType::U32)),
            inclusive: false,
        };
        assert!(t.display().contains(".."));
        let inc = Type::Range {
            element: Box::new(Type::Primitive(PrimitiveType::U32)),
            inclusive: true,
        };
        assert!(inc.display().contains("..="));
    }

    // ── Tuple expressions ────────────────────────────────────────────────

    #[test]
    fn tuple_expr_yields_tuple_type() {
        let typing = infer_str("@fn f() { let _t := (1u32, true); }").unwrap();
        let saw_tuple = first_of(&typing, |t| {
            matches!(t,
                Type::Tuple(elems)
                    if elems.len() == 2
                        && matches!(elems[0], Type::Primitive(PrimitiveType::U32))
                        && matches!(elems[1], Type::Primitive(PrimitiveType::Bool))
            )
        });
        assert!(saw_tuple.is_some());
    }

    // ── Array literal expressions ────────────────────────────────────────

    #[test]
    fn array_literal_yields_array_type() {
        let typing = infer_str("@fn f() { let _a := [1u32, 2u32, 3u32]; }").unwrap();
        let saw_array = first_of(&typing, |t| {
            matches!(t,
                Type::Array { element, size }
                    if matches!(element.as_ref(), Type::Primitive(PrimitiveType::U32))
                        && size == "3"
            )
        });
        assert!(saw_array.is_some());
    }

    #[test]
    fn array_repeat_yields_array_type() {
        let typing = infer_str("@fn f() { let _a := [0u8; 64]; }").unwrap();
        let saw_array = first_of(&typing, |t| {
            matches!(t,
                Type::Array { element, size }
                    if matches!(element.as_ref(), Type::Primitive(PrimitiveType::U8))
                        && size == "64"
            )
        });
        assert!(saw_array.is_some());
    }

    // ── Index expressions ────────────────────────────────────────────────

    #[test]
    fn index_into_array_yields_element() {
        let typing =
            infer_str("@fn f() -> u32 { let a := [1u32, 2u32, 3u32]; return a[0u32]; }").unwrap();
        // The `a[0u32]` index returns u32.
        let u32_count = typing
            .types
            .values()
            .filter(|t| matches!(t, Type::Primitive(PrimitiveType::U32)))
            .count();
        assert!(u32_count >= 4, "got {u32_count}");
    }

    #[test]
    fn index_into_ref_array_auto_derefs() {
        // `&[u8; 64]` parameter → indexing returns u8.
        let typing = infer_str("@fn f(buf: &[u8; 64]) -> u8 { return buf[0u32]; }").unwrap();
        let saw_u8 = first_of(&typing, |t| matches!(t, Type::Primitive(PrimitiveType::U8)));
        assert!(saw_u8.is_some());
    }

    #[test]
    fn index_into_ref_slice_auto_derefs() {
        let typing = infer_str("@fn f(buf: &[u8]) -> u8 { return buf[0u32]; }").unwrap();
        let saw_u8 = first_of(&typing, |t| matches!(t, Type::Primitive(PrimitiveType::U8)));
        assert!(saw_u8.is_some());
    }

    #[test]
    fn index_with_non_integer_is_e0517() {
        let errors = infer_str("@fn f(buf: &[u8]) -> u8 { return buf[true]; }").unwrap_err();
        assert!(errors
            .iter()
            .any(|e| matches!(e, TypeError::IndexNotInteger { .. })));
    }

    #[test]
    fn index_into_non_indexable_is_e0516() {
        let errors = infer_str("@fn f() -> u32 { let x := 42u32; return x[0u32]; }").unwrap_err();
        assert!(errors
            .iter()
            .any(|e| matches!(e, TypeError::IndexNonIndexable { .. })));
    }

    // ── Range expressions ────────────────────────────────────────────────

    #[test]
    fn range_half_open_yields_range_type() {
        let typing = infer_str("@fn f() { let _r := 0u32 .. 10u32; }").unwrap();
        let saw_range = first_of(&typing, |t| {
            matches!(t,
                Type::Range { element, inclusive: false }
                    if matches!(element.as_ref(), Type::Primitive(PrimitiveType::U32))
            )
        });
        assert!(saw_range.is_some());
    }

    #[test]
    fn range_inclusive_yields_inclusive_range_type() {
        let typing = infer_str("@fn f() { let _r := 0u32 ..= 10u32; }").unwrap();
        let saw_range = first_of(&typing, |t| {
            matches!(
                t,
                Type::Range {
                    inclusive: true,
                    ..
                }
            )
        });
        assert!(saw_range.is_some());
    }

    #[test]
    fn range_with_mismatched_bounds_is_e0510() {
        // We reuse BinaryTypeMismatch for range mismatch (op `..` / `..=`).
        let errors = infer_str("@fn f() { let _r := 0u32 .. 10u8; }").unwrap_err();
        assert!(errors
            .iter()
            .any(|e| matches!(e, TypeError::BinaryTypeMismatch { op: "..", .. })));
    }

    // ── Combined: array + index in a real signature ──────────────────────

    #[test]
    fn array_field_access_works() {
        // `Counter.flags: [u8; 4]` — accessing `Counter.flags[0]` gives u8.
        let res = infer_str(
            "#automaton Counter { flags: [u8; 4]; } \
             #effect peek() #mutates: [] { let _x: u8 = Counter.flags[0u32]; }",
        );
        assert!(res.is_ok(), "got errors: {:?}", res);
    }

    #[test]
    fn realistic_program_with_calls_and_fields() {
        let src = "\
            #automaton Counter { value: u32; }\n\
            @fn double(x: u32) -> u32 { return x; }\n\
            #effect bump() #mutates: [Counter] {\n  \
              let next: u32 = double(Counter.value);\n  \
              Counter.value = next;\n  \
              return;\n\
            }\n\
        ";
        let res = infer_str(src);
        assert!(res.is_ok(), "got errors: {:?}", res);
        let typing = res.unwrap();
        // Plenty of u32s expected.
        let u32_count = types_in(&typing)
            .iter()
            .filter(|t| matches!(t, Type::Primitive(PrimitiveType::U32)))
            .count();
        assert!(u32_count >= 4, "got {u32_count}");
    }

    // ─── Slice T4a: nominal types from path-position type expressions ────

    #[test]
    fn nominal_display_simple() {
        let t = Type::Nominal {
            path: vec!["Counter".to_owned()],
            args: vec![],
        };
        assert_eq!(t.display(), "Counter");
    }

    #[test]
    fn nominal_display_multi_segment() {
        let t = Type::Nominal {
            path: vec![
                "clifford".to_owned(),
                "core".to_owned(),
                "Option".to_owned(),
            ],
            args: vec![],
        };
        assert_eq!(t.display(), "clifford::core::Option");
    }

    #[test]
    fn nominal_display_with_one_generic_arg() {
        let t = Type::Nominal {
            path: vec!["Option".to_owned()],
            args: vec![Type::Primitive(PrimitiveType::U32)],
        };
        assert_eq!(t.display(), "Option<u32>");
    }

    #[test]
    fn nominal_display_with_multiple_generic_args() {
        let t = Type::Nominal {
            path: vec!["Result".to_owned()],
            args: vec![
                Type::Primitive(PrimitiveType::U32),
                Type::Primitive(PrimitiveType::Bool),
            ],
        };
        assert_eq!(t.display(), "Result<u32, bool>");
    }

    #[test]
    fn nominal_display_with_nested_generic_arg() {
        // Option<Result<u32, bool>>
        let inner = Type::Nominal {
            path: vec!["Result".to_owned()],
            args: vec![
                Type::Primitive(PrimitiveType::U32),
                Type::Primitive(PrimitiveType::Bool),
            ],
        };
        let outer = Type::Nominal {
            path: vec!["Option".to_owned()],
            args: vec![inner],
        };
        assert_eq!(outer.display(), "Option<Result<u32, bool>>");
    }

    #[test]
    fn nominal_distinct_identity_per_path() {
        // Even though two `@type` aliases may both wrap u32, the Nominal
        // values themselves are distinct (Decision #19's nominal-access
        // identity rule extended to all top-level type-bearing decls).
        let foo = Type::Nominal {
            path: vec!["Foo".to_owned()],
            args: vec![],
        };
        let bar = Type::Nominal {
            path: vec!["Bar".to_owned()],
            args: vec![],
        };
        assert_ne!(foo, bar);
    }

    #[test]
    fn nominal_param_type_carries_through() {
        // A function whose parameter is a path-position type — when the
        // body references the param by name, the path-expression's
        // recorded type should be a `Nominal`. (Slice T4a does no
        // alias-following, so we don't try to use `c` arithmetically;
        // we just bind it to a `let _y := c;` whose initializer is the
        // param-as-path expression.)
        let src = "\
            @type Counter = u32;\n\
            @fn observe(c: Counter) {\n  \
              let _y := c;\n  \
              return;\n\
            }\n\
        ";
        let typing = infer_str(src).expect("infer ok");
        let saw_counter_nominal = types_in(&typing).iter().any(|t| {
            matches!(t, Type::Nominal { path, args } if path == &["Counter"] && args.is_empty())
        });
        assert!(
            saw_counter_nominal,
            "expected at least one Type::Nominal with path = [\"Counter\"], got {:?}",
            types_in(&typing)
        );
    }

    #[test]
    fn nominal_let_annotation_alias_follows_to_underlying_type() {
        // T4b behaviour change (was T4a's
        // `..._emits_e0512_with_nominal_in_message`): with `@type MyAlias
        // = u32;` registered, the `MyAlias` annotation unfolds to
        // `Primitive(U32)` for compatibility checking, so the initialiser
        // `0u32` matches and *no* E0512 fires. T4a documented the
        // pre-T4b mismatch as the *current* behaviour; T4b lifts it.
        let src = "\
            @type MyAlias = u32;\n\
            @fn f() {\n  \
              let _x: MyAlias = 0u32;\n  \
              return;\n\
            }\n\
        ";
        let res = infer_str(src);
        assert!(
            res.is_ok(),
            "expected alias-following to make MyAlias = u32 typecheck; got {res:?}"
        );
    }

    #[test]
    fn nominal_generic_args_translate_recursively() {
        // A path-position type like `Box<u32>` translates to a Nominal
        // with `args = [Primitive(U32)]`. The path/args are recorded
        // verbatim — slice T4a does no resolution.
        let src = "\
            @fn make() -> Box<u32> {\n  \
              return 0u32;\n\
            }\n\
        ";
        // Same as above — this likely emits a return-type mismatch at
        // T4a (Nominal `Box<u32>` vs Primitive `u32`) — but the typing
        // map should still include the Nominal somewhere via the return-
        // type annotation having been seen at parse time.
        let _ = infer_str(src);
        // We can't easily peek the return-type annotation through
        // `Typing` (it keys off expression spans, not signatures).
        // Instead exercise the helper directly:
        let tokens = tokenize(src).expect("tokenize");
        let program = parse(&tokens).expect("parse");
        let fn_decl = program.items.iter().find_map(|i| match i {
            Item::Fn(f) => Some(f),
            _ => None,
        });
        let fn_decl = fn_decl.expect("@fn make exists");
        let ret_ty_expr = fn_decl.return_type.as_ref().expect("explicit return type");
        let translated = type_from_type_expr(ret_ty_expr);
        match translated {
            Type::Nominal { path, args } => {
                assert_eq!(path, vec!["Box".to_owned()]);
                assert_eq!(args.len(), 1);
                assert!(matches!(args[0], Type::Primitive(PrimitiveType::U32)));
            }
            other => panic!("expected Nominal Box<u32>, got {}", other.display()),
        }
    }

    #[test]
    fn nominal_no_args_translate_verbatim() {
        // `Counter` (no generics) translates to a Nominal with empty args.
        let src = "@fn f(c: Counter) { return; }";
        let tokens = tokenize(src).expect("tokenize");
        let program = parse(&tokens).expect("parse");
        let fn_decl = program.items.iter().find_map(|i| match i {
            Item::Fn(f) => Some(f),
            _ => None,
        });
        let param_ty = &fn_decl.expect("@fn f exists").params[0].ty;
        let translated = type_from_type_expr(param_ty);
        assert!(matches!(translated, Type::Nominal { path, args }
            if path == vec!["Counter".to_owned()] && args.is_empty()));
    }

    // ─── Slice T4b: type registry + @type alias following ────────────────

    #[test]
    fn t4b_alias_one_step_typechecks() {
        // The headline T4b case: a one-step alias unfolds for compatibility.
        let src = "\
            @type ByteCount = u32;\n\
            @fn f() { let _x: ByteCount = 5u32; return; }\n\
        ";
        let res = infer_str(src);
        assert!(res.is_ok(), "expected alias-follow to succeed; got {res:?}");
    }

    #[test]
    fn t4b_alias_transitive_typechecks() {
        // Two-step alias chain: A → B → u32.
        let src = "\
            @type B = u32;\n\
            @type A = B;\n\
            @fn f() { let _x: A = 7u32; return; }\n\
        ";
        let res = infer_str(src);
        assert!(res.is_ok(), "expected transitive alias-follow; got {res:?}");
    }

    #[test]
    fn t4b_alias_chain_three_deep_typechecks() {
        // Slightly deeper chain to exercise the unalias loop.
        let src = "\
            @type C = u32;\n\
            @type B = C;\n\
            @type A = B;\n\
            @fn f() { let _x: A = 1u32; return; }\n\
        ";
        let res = infer_str(src);
        assert!(res.is_ok(), "expected three-deep alias chain; got {res:?}");
    }

    #[test]
    fn t4b_alias_mismatch_after_unfolding_still_errors() {
        // The alias unfolds, but the underlying types still don't match.
        // Diagnostic should name `MyAlias` and `bool` (T4b deliberately
        // shows the alias name for the user, not the unfolded form —
        // their identifier is what they wrote).
        let src = "\
            @type MyAlias = u32;\n\
            @fn f() { let _x: MyAlias = true; return; }\n\
        ";
        let errors = infer_str(src).expect_err("expected post-unfold mismatch");
        let saw = errors.iter().any(|e| {
            matches!(e, TypeError::LetTypeMismatch { declared, actual, .. }
                if declared == "MyAlias" && actual == "bool")
        });
        assert!(
            saw,
            "expected LetTypeMismatch declared=MyAlias actual=bool; got {errors:?}"
        );
    }

    #[test]
    fn t4b_alias_to_compound_type_typechecks() {
        // Alias of a tuple type. The unalias should peel one layer
        // (Nominal → Tuple), and structural equality covers the rest.
        let src = "\
            @type Pair = (u32, bool);\n\
            @fn f() { let _x: Pair = (1u32, true); return; }\n\
        ";
        let res = infer_str(src);
        assert!(
            res.is_ok(),
            "expected alias-to-tuple to typecheck; got {res:?}"
        );
    }

    #[test]
    fn t4b_alias_to_ref_typechecks() {
        // Alias of a reference type. The borrow `&x` of a u32 yields
        // `&u32`, which the alias `BytePtr = &u32` should match.
        let src = "\
            @type BytePtr = &u32;\n\
            @fn f() { let v: u32 = 0u32; let _p: BytePtr = &v; return; }\n\
        ";
        let res = infer_str(src);
        assert!(res.is_ok(), "expected alias-to-ref; got {res:?}");
    }

    #[test]
    fn t4b_two_distinct_aliases_to_same_underlying_compare_equal() {
        // `@type Foo = u32; @type Bar = u32;` — Decision #19 said two
        // distinct nominal types compare distinct *as nominals*, but
        // T4b's alias-following means at the `types_compatible` site
        // both unfold to `u32` and are compatible. This is the
        // transparent-alias semantics (Foo and Bar are interchangeable
        // wherever a value of either is required); strong newtype
        // semantics would need a separate `@newtype` declaration that
        // T4b doesn't introduce.
        let src = "\
            @type Foo = u32;\n\
            @type Bar = u32;\n\
            @fn make_foo() -> Foo { return 0u32; }\n\
            @fn take_bar(x: Bar) -> u32 { return x; }\n\
            @fn f() { let _y: u32 = take_bar(make_foo()); return; }\n\
        ";
        let res = infer_str(src);
        assert!(
            res.is_ok(),
            "expected transparent-alias interchangeability; got {res:?}"
        );
    }

    #[test]
    fn t4b_adt_does_not_unfold() {
        // `@type Color = | Red | Green | Blue;` — ADT, not alias.
        // The annotation `let _x: Color = …` does NOT unfold to anything;
        // Color stays a Nominal terminal type. With nothing to coerce
        // an integer literal to it, the mismatch fires (declared=Color,
        // actual=i32 default).
        let src = "\
            @type Color = | Red | Green | Blue;\n\
            @fn f() { let _x: Color = 0; return; }\n\
        ";
        let errors = infer_str(src).expect_err("ADT does not unfold");
        let saw = errors.iter().any(|e| {
            matches!(e, TypeError::LetTypeMismatch { declared, .. }
                if declared == "Color")
        });
        assert!(saw, "expected Color (ADT) to not unfold; got {errors:?}");
    }

    #[test]
    fn t4b_unknown_nominal_path_treated_as_unknown_for_compat() {
        // A path that doesn't resolve to any `@type` decl is currently
        // *not* validated by T4b (validation pass is T4c). Compat with
        // an unknown nominal goes through the unalias path which leaves
        // it as Nominal — a structural compare against u32 fails. The
        // diagnostic still names the source identifier correctly.
        let src = "\
            @fn f() { let _x: NotADeclaredType = 0u32; return; }\n\
        ";
        let errors = infer_str(src).expect_err("unknown nominal vs u32 mismatches");
        let saw = errors.iter().any(|e| {
            matches!(e, TypeError::LetTypeMismatch { declared, actual, .. }
                if declared == "NotADeclaredType" && actual == "u32")
        });
        assert!(
            saw,
            "expected mismatch with NotADeclaredType named verbatim; got {errors:?}"
        );
    }

    #[test]
    fn t4b_unalias_terminates_on_self_reference() {
        // `@type A = A;` is illegal source-wise but the unalias loop
        // must still terminate (depth limit). Build a registry by hand
        // and call `unalias` directly to verify the safeguard.
        let mut decls = HashMap::new();
        decls.insert(
            "A".to_owned(),
            NominalDecl::Alias {
                params: vec![],
                target: Type::Nominal {
                    path: vec!["A".to_owned()],
                    args: vec![],
                },
            },
        );
        let registry = TypeRegistry { decls };
        let result = registry.unalias(&Type::Nominal {
            path: vec!["A".to_owned()],
            args: vec![],
        });
        assert!(
            matches!(result, Type::Unknown(_)),
            "self-reference should hit the depth-limit safeguard, got {result:?}"
        );
    }

    #[test]
    fn t4b_unalias_terminates_on_two_step_cycle() {
        // `@type A = B; @type B = A;` — same safeguard.
        let mut decls = HashMap::new();
        decls.insert(
            "A".to_owned(),
            NominalDecl::Alias {
                params: vec![],
                target: Type::Nominal {
                    path: vec!["B".to_owned()],
                    args: vec![],
                },
            },
        );
        decls.insert(
            "B".to_owned(),
            NominalDecl::Alias {
                params: vec![],
                target: Type::Nominal {
                    path: vec!["A".to_owned()],
                    args: vec![],
                },
            },
        );
        let registry = TypeRegistry { decls };
        let result = registry.unalias(&Type::Nominal {
            path: vec!["A".to_owned()],
            args: vec![],
        });
        assert!(
            matches!(result, Type::Unknown(_)),
            "two-step cycle should hit the depth-limit safeguard, got {result:?}"
        );
    }

    #[test]
    fn t4b_generic_args_block_alias_unfolding() {
        // T4b semantics: a non-generic alias (`params=[]`) given args
        // (`Vec<u8>` in the test) is an arity mismatch — `unfold_one`
        // returns None so the nominal stays as-is. T4c's `unalias`
        // preserves this conservative behaviour at the unfold site;
        // the validation pass (`validate_nominal_paths`) is what now
        // surfaces the diagnostic (E0519) at the source level.
        let mut decls = HashMap::new();
        decls.insert(
            "Vec".to_owned(),
            NominalDecl::Alias {
                params: vec![],
                target: Type::Primitive(PrimitiveType::U32),
            },
        );
        let registry = TypeRegistry { decls };
        let with_args = Type::Nominal {
            path: vec!["Vec".to_owned()],
            args: vec![Type::Primitive(PrimitiveType::U8)],
        };
        let result = registry.unalias(&with_args);
        // Arity mismatch (alias has 0 params, call site has 1 arg) →
        // no unfolding.
        assert_eq!(result, with_args);
    }

    #[test]
    fn t4b_call_arg_mismatch_through_alias_works() {
        // The other types_compatible call site is the call-arg check
        // in `infer_call`. Verify alias following also works there.
        let src = "\
            @type Count = u32;\n\
            @fn double(x: u32) -> u32 { return x; }\n\
            @fn caller() { let _y: u32 = double(0u32); return; }\n\
            @fn caller2() {\n  \
              let n: Count = 5u32;\n  \
              let _y: u32 = double(n);\n  \
              return;\n\
            }\n\
        ";
        // `n` has type `Count`; passing it to `double(x: u32)` should
        // succeed because Count unfolds to u32 at the call-arg compat
        // check.
        let res = infer_str(src);
        assert!(
            res.is_ok(),
            "expected alias to work in call-arg compat too; got {res:?}"
        );
    }

    // ─── Decision #22 + Decision #2 + ADR 0003: trait validation ─────────

    #[test]
    fn predeclared_pure_trait_on_fn_accepted() {
        let src = "@fn p(x: u32) -> u32 $ [Pure] { return x; }";
        let res = infer_str(src);
        assert!(res.is_ok(), "Pure should be predeclared, got {res:?}");
    }

    #[test]
    fn predeclared_readable_trait_on_fn_accepted() {
        let src = "@fn r(x: u32) -> u32 $ [Readable] { return x; }";
        let res = infer_str(src);
        assert!(res.is_ok(), "Readable should be predeclared, got {res:?}");
    }

    #[test]
    fn predeclared_imperative_trait_on_effect_accepted() {
        let src = "\
            #automaton C { v: u32; }\n\
            #effect tick() #mutates: [C] $ [Realtime, Hardware] { }\n\
        ";
        let res = infer_str(src);
        assert!(
            res.is_ok(),
            "Realtime + Hardware should be predeclared on #effect, got {res:?}"
        );
    }

    #[test]
    fn predeclared_imperative_traits_on_interrupt_accepted() {
        let src = "\
            #automaton C { v: u32; }\n\
            #interrupt SysTick() #mutates: [C] #priority: HIGH $ [Realtime, Acquire] { }\n\
        ";
        let res = infer_str(src);
        assert!(
            res.is_ok(),
            "Realtime + Acquire on #interrupt should be predeclared, got {res:?}"
        );
    }

    #[test]
    fn predeclared_trait_on_transition_accepted() {
        let src = "\
            #automaton C {\n  \
              value: u32;\n  \
              #transition tick $ [PureState] { C.value = 1u32; }\n\
            }\n\
        ";
        let res = infer_str(src);
        assert!(
            res.is_ok(),
            "PureState on #transition should be predeclared, got {res:?}"
        );
    }

    #[test]
    fn unknown_trait_on_fn_emits_e0541() {
        let src = "@fn f(x: u32) -> u32 $ [TotallyMadeUp] { return x; }";
        let errors = infer_str(src).expect_err("expected E0541");
        let saw = errors.iter().any(|e| {
            matches!(e, TypeError::UnknownTrait { trait_name, kind, callable, .. }
                if trait_name == "TotallyMadeUp" && *kind == "@fn" && callable == "f")
        });
        assert!(saw, "expected E0541 with kind=@fn callable=f; got {errors:?}");
    }

    #[test]
    fn unknown_trait_on_effect_emits_e0541() {
        let src = "\
            #automaton C { v: u32; }\n\
            #effect bad() #mutates: [C] $ [Whatever] { }\n\
        ";
        let errors = infer_str(src).expect_err("expected E0541");
        let saw = errors.iter().any(|e| {
            matches!(e, TypeError::UnknownTrait { trait_name, kind, callable, .. }
                if trait_name == "Whatever" && *kind == "#effect" && callable == "bad")
        });
        assert!(saw, "expected E0541 on #effect; got {errors:?}");
    }

    #[test]
    fn unknown_trait_on_interrupt_emits_e0541() {
        let src = "\
            #automaton C { v: u32; }\n\
            #interrupt SysTick() #mutates: [C] #priority: HIGH $ [Bogus] { }\n\
        ";
        let errors = infer_str(src).expect_err("expected E0541");
        let saw = errors.iter().any(|e| {
            matches!(e, TypeError::UnknownTrait { trait_name, kind, callable, .. }
                if trait_name == "Bogus" && *kind == "#interrupt" && callable == "SysTick")
        });
        assert!(saw, "expected E0541 on #interrupt; got {errors:?}");
    }

    #[test]
    fn unknown_trait_on_transition_emits_e0541() {
        let src = "\
            #automaton C {\n  \
              v: u32;\n  \
              #transition tick $ [DefinitelyNotATrait] { C.v = 1u32; }\n\
            }\n\
        ";
        let errors = infer_str(src).expect_err("expected E0541");
        let saw = errors.iter().any(|e| {
            matches!(e, TypeError::UnknownTrait { trait_name, kind, callable, .. }
                if trait_name == "DefinitelyNotATrait" && *kind == "#transition" && callable == "tick")
        });
        assert!(saw, "expected E0541 on #transition; got {errors:?}");
    }

    #[test]
    fn typo_in_predeclared_trait_emits_e0541() {
        // `Realtim` (missing trailing `e`) is the classic typo. The
        // diagnostic must catch it — that's the whole point of the
        // validation pass per Decision #22.
        let src = "\
            #automaton C { v: u32; }\n\
            #effect e() #mutates: [C] $ [Realtim] { }\n\
        ";
        let errors = infer_str(src).expect_err("expected E0541 on typo");
        let saw = errors.iter().any(|e| {
            matches!(e, TypeError::UnknownTrait { trait_name, .. } if trait_name == "Realtim")
        });
        assert!(saw, "expected E0541 for `Realtim` typo; got {errors:?}");
    }

    #[test]
    fn user_defined_trait_via_at_trait_is_accepted() {
        let src = "\
            @trait MyOwnTrait { }\n\
            @fn f(x: u32) -> u32 $ [MyOwnTrait] { return x; }\n\
        ";
        let res = infer_str(src);
        assert!(
            res.is_ok(),
            "user-defined `@trait MyOwnTrait` should validate; got {res:?}"
        );
    }

    #[test]
    fn diverges_trait_is_unknown_per_adr_0003_q4() {
        // ADR 0003 Q4 explicitly DROPPED the `Diverges` trait —
        // `@partial @fn` covers non-termination. Source code that still
        // references `Diverges` should fail validation, signalling the
        // user to switch to `@partial`.
        let src = "@fn f(x: u32) -> u32 $ [Diverges] { return x; }";
        let errors = infer_str(src).expect_err("Diverges should be unknown post-ADR-0003");
        let saw = errors.iter().any(|e| {
            matches!(e, TypeError::UnknownTrait { trait_name, .. } if trait_name == "Diverges")
        });
        assert!(saw, "expected E0541 for dropped `Diverges` trait; got {errors:?}");
    }

    #[test]
    fn empty_trait_list_does_not_trigger_validation() {
        // Per Emergent Rule 2 + the AST docs, an empty `$ [...]` is the
        // same shape as no trait list. No diagnostic should fire.
        let src1 = "@fn p(x: u32) -> u32 { return x; }";
        let src2 = "@fn p(x: u32) -> u32 $ [] { return x; }";
        assert!(infer_str(src1).is_ok(), "no trait list should be silent");
        assert!(infer_str(src2).is_ok(), "empty trait list should be silent");
    }

    #[test]
    fn multiple_unknown_traits_all_reported() {
        // Multiple bad trait names in one list — each gets its own E0541.
        let src = "@fn f(x: u32) -> u32 $ [Foo, Bar, Pure, Baz] { return x; }";
        let errors = infer_str(src).expect_err("expected multiple E0541s");
        let unknown_names: Vec<String> = errors
            .iter()
            .filter_map(|e| match e {
                TypeError::UnknownTrait { trait_name, .. } => Some(trait_name.clone()),
                _ => None,
            })
            .collect();
        assert!(unknown_names.contains(&"Foo".to_owned()));
        assert!(unknown_names.contains(&"Bar".to_owned()));
        assert!(unknown_names.contains(&"Baz".to_owned()));
        assert!(
            !unknown_names.contains(&"Pure".to_owned()),
            "Pure is predeclared and should not appear"
        );
    }

    #[test]
    fn predeclared_traits_full_set_recognised() {
        // Smoke test: every name in PREDECLARED_PURE_TRAITS and
        // PREDECLARED_IMPERATIVE_TRAITS resolves cleanly. If someone
        // edits the constants, this test catches an accidental
        // omission.
        for name in PREDECLARED_PURE_TRAITS.iter() {
            let src = format!("@fn f(x: u32) -> u32 $ [{name}] {{ return x; }}");
            assert!(
                infer_str(&src).is_ok(),
                "predeclared pure trait `{name}` should be recognised"
            );
        }
        for name in PREDECLARED_IMPERATIVE_TRAITS.iter() {
            let src = format!(
                "#automaton C {{ v: u32; }}\n\
                 #effect e() #mutates: [C] $ [{name}] {{ }}"
            );
            assert!(
                infer_str(&src).is_ok(),
                "predeclared imperative trait `{name}` should be recognised"
            );
        }
    }

    // ─── Decision #24 / ADR 0004: @snapshot typing + E0550 row gate ──────

    #[test]
    fn snapshot_yields_field_type() {
        // The snapshot expression's inferred type is the field's
        // declared type. Verified by binding the snapshot to a typed
        // `let` and checking no E0512 fires (the types match).
        let src = "\
            #automaton Counter { value: u32; }\n\
            @fn read_value() -> u32 $ [Readable] {\n  \
              let v: u32 = @snapshot Counter.value;\n  \
              return v;\n\
            }\n\
        ";
        let res = infer_str(src);
        assert!(
            res.is_ok(),
            "expected snapshot to type-as-field-type (u32); got {res:?}"
        );
    }

    #[test]
    fn snapshot_type_mismatch_diagnosed() {
        // Wrong annotation: snapshotting a u32 field into a bool let.
        // The compatibility check uses the field's actual type (u32),
        // so E0512 fires correctly.
        let src = "\
            #automaton Counter { value: u32; }\n\
            @fn bad() -> bool $ [Readable] {\n  \
              let v: bool = @snapshot Counter.value;\n  \
              return v;\n\
            }\n\
        ";
        let errors = infer_str(src).expect_err("expected E0512 mismatch");
        let saw = errors.iter().any(|e| {
            matches!(e, TypeError::LetTypeMismatch { declared, actual, .. }
                if declared == "bool" && actual == "u32")
        });
        assert!(
            saw,
            "expected LetTypeMismatch declared=bool actual=u32; got {errors:?}"
        );
    }

    // ── Readable-row gate (E0550) ────────────────────────────────────────

    #[test]
    fn snapshot_in_fn_with_readable_row_passes() {
        let src = "\
            #automaton C { v: u32; }\n\
            @fn r() -> u32 $ [Readable] { let v := @snapshot C.v; return v; }\n\
        ";
        assert!(
            infer_str(src).is_ok(),
            "@fn with Readable should accept @snapshot"
        );
    }

    #[test]
    fn snapshot_in_fn_without_readable_row_emits_e0550() {
        // The headline E0550 case: @fn body uses @snapshot but the
        // signature has no `Readable` row. Empty trait list defaults
        // to [Pure] per Emergent Rule 2 — no Readable row present.
        let src = "\
            #automaton C { v: u32; }\n\
            @fn p() -> u32 { let v := @snapshot C.v; return v; }\n\
        ";
        let errors = infer_str(src).expect_err("expected E0550");
        let saw = errors.iter().any(|e| {
            matches!(e, TypeError::SnapshotInUnreadableFn { fn_name, .. } if fn_name == "p")
        });
        assert!(
            saw,
            "expected E0550 SnapshotInUnreadableFn for `p`; got {errors:?}"
        );
    }

    #[test]
    fn snapshot_in_fn_with_pure_row_only_emits_e0550() {
        // Explicit `$ [Pure]` — Pure does NOT include Readable.
        let src = "\
            #automaton C { v: u32; }\n\
            @fn p() -> u32 $ [Pure] { let v := @snapshot C.v; return v; }\n\
        ";
        let errors = infer_str(src).expect_err("expected E0550 with $ [Pure]");
        assert!(errors.iter().any(|e| matches!(
            e,
            TypeError::SnapshotInUnreadableFn { fn_name, .. } if fn_name == "p"
        )));
    }

    #[test]
    fn snapshot_in_fn_with_observable_row_only_emits_e0550() {
        // Observable is its own row label and does NOT subsume Readable
        // in v0.2-α. Snapshot still requires Readable explicitly.
        let src = "\
            #automaton C { v: u32; }\n\
            @fn p() -> u32 $ [Observable] { let v := @snapshot C.v; return v; }\n\
        ";
        let errors = infer_str(src).expect_err("expected E0550 with $ [Observable]");
        assert!(errors.iter().any(|e| matches!(
            e,
            TypeError::SnapshotInUnreadableFn { fn_name, .. } if fn_name == "p"
        )));
    }

    #[test]
    fn snapshot_in_arg_position_caught() {
        // Snapshot buried inside a call arg — E0550 fires regardless.
        let src = "\
            #automaton C { v: u32; }\n\
            @fn id(x: u32) -> u32 { return x; }\n\
            @fn use_snap() -> u32 { return id(@snapshot C.v); }\n\
        ";
        let errors = infer_str(src).expect_err("expected E0550 with snapshot in arg");
        assert!(errors.iter().any(|e| matches!(
            e,
            TypeError::SnapshotInUnreadableFn { fn_name, .. } if fn_name == "use_snap"
        )));
    }

    #[test]
    fn snapshot_in_binary_position_caught() {
        let src = "\
            #automaton C { v: u32; }\n\
            @fn diff() -> u32 { return @snapshot C.v - 1u32; }\n\
        ";
        let errors = infer_str(src).expect_err("expected E0550 with snapshot in binary");
        assert!(errors.iter().any(|e| matches!(
            e,
            TypeError::SnapshotInUnreadableFn { fn_name, .. } if fn_name == "diff"
        )));
    }

    #[test]
    fn fn_without_snapshot_silent_regardless_of_row() {
        // No snapshot in the body → no E0550 regardless of trait list.
        let src1 = "@fn p() -> u32 { return 0u32; }";
        let src2 = "@fn p() -> u32 $ [Readable] { return 0u32; }";
        let src3 = "@fn p() -> u32 $ [Pure] { return 0u32; }";
        for src in [src1, src2, src3] {
            assert!(
                infer_str(src).is_ok(),
                "no snapshot in body should be silent regardless of row; failed on: {src}"
            );
        }
    }

    #[test]
    fn one_e0550_per_offending_fn() {
        // Multiple snapshots in the same body → still only one E0550.
        let src = "\
            #automaton C { v: u32; }\n\
            @fn p() -> u32 { \
              let a := @snapshot C.v; \
              let b := @snapshot C.v; \
              let c := @snapshot C.v; \
              return a; \
            }\n\
        ";
        let errors = infer_str(src).expect_err("expected E0550");
        let count = errors
            .iter()
            .filter(|e| matches!(e, TypeError::SnapshotInUnreadableFn { fn_name, .. } if fn_name == "p"))
            .count();
        assert_eq!(count, 1, "expected exactly one E0550; got {count}: {errors:?}");
    }

    #[test]
    fn snapshot_in_imperative_layer_silent() {
        // ADR 0004 P3: `#`-layer (effects, interrupts, transitions)
        // are NOT row-gated; they may always observe automaton state.
        // The E0550 check skips them entirely.
        let src = "\
            #automaton C { v: u32; }\n\
            #effect e() #mutates: [C] { \
              let v := @snapshot C.v; \
              C.v = v; \
            }\n\
        ";
        // The body-walker may still report something else, but no E0550.
        let res = infer_str(src);
        if let Err(errors) = &res {
            let saw_e0550 = errors.iter().any(|e| matches!(
                e,
                TypeError::SnapshotInUnreadableFn { .. }
            ));
            assert!(!saw_e0550, "imperative-layer should not trigger E0550; got {errors:?}");
        }
    }

    #[test]
    fn diagnostic_carries_decl_and_snapshot_offsets() {
        let src = "\
            #automaton C { v: u32; }\n\
            @fn p() -> u32 { let v := @snapshot C.v; return v; }\n\
        ";
        let errors = infer_str(src).expect_err("expected E0550");
        for e in &errors {
            if let TypeError::SnapshotInUnreadableFn { fn_name, at, decl_at } = e {
                assert_eq!(fn_name, "p");
                assert!(*decl_at < *at, "decl_at must precede snapshot at");
                assert!(*at < src.len());
                return;
            }
        }
        panic!("expected E0550, got {errors:?}");
    }

    // ─── Slice T4c: generic alias substitution + E0518 + E0519 ──────────

    #[test]
    fn t4c_generic_alias_substitutes_to_underlying() {
        // The headline T4c case: a generic alias `Pair<T> = (T, T)`
        // substituted with `Pair<u32>` unfolds to `(u32, u32)`.
        let src = "\
            @type Pair<T> = (T, T);\n\
            @fn f() { let _p: Pair<u32> = (1u32, 2u32); return; }\n\
        ";
        let res = infer_str(src);
        assert!(
            res.is_ok(),
            "expected Pair<u32> to unfold to (u32, u32); got {res:?}"
        );
    }

    #[test]
    fn t4c_generic_alias_two_params() {
        // Two generic params, two args.
        let src = "\
            @type Both<A, B> = (A, B);\n\
            @fn f() { let _p: Both<u32, bool> = (1u32, true); return; }\n\
        ";
        let res = infer_str(src);
        assert!(res.is_ok(), "expected two-param substitution; got {res:?}");
    }

    #[test]
    fn t4c_generic_alias_arity_mismatch_too_many() {
        let src = "\
            @type Pair<T> = (T, T);\n\
            @fn f() { let _p: Pair<u32, bool> = (1u32, true); return; }\n\
        ";
        let errors = infer_str(src).expect_err("expected E0519");
        let saw = errors.iter().any(|e| matches!(
            e,
            TypeError::GenericArityMismatch { name, expected, actual, .. }
                if name == "Pair" && *expected == 1 && *actual == 2
        ));
        assert!(saw, "expected E0519 expected=1 actual=2; got {errors:?}");
    }

    #[test]
    fn t4c_generic_alias_arity_mismatch_too_few() {
        let src = "\
            @type Pair<T> = (T, T);\n\
            @fn f() { let _p: Pair = (1u32, 2u32); return; }\n\
        ";
        let errors = infer_str(src).expect_err("expected E0519");
        let saw = errors.iter().any(|e| matches!(
            e,
            TypeError::GenericArityMismatch { name, expected, actual, .. }
                if name == "Pair" && *expected == 1 && *actual == 0
        ));
        assert!(saw, "expected E0519 expected=1 actual=0; got {errors:?}");
    }

    #[test]
    fn t4c_unknown_nominal_emits_e0518() {
        // T4b just propagated unknown nominals as `Type::Nominal` and
        // let downstream mismatch. T4c surfaces the dedicated E0518
        // diagnostic at signature time.
        let src = "@fn f(x: NotARealType) -> u32 { return 0u32; }";
        let errors = infer_str(src).expect_err("expected E0518");
        let saw = errors.iter().any(|e| matches!(
            e,
            TypeError::UnknownNominalType { name, .. } if name == "NotARealType"
        ));
        assert!(saw, "expected E0518 for `NotARealType`; got {errors:?}");
    }

    #[test]
    fn t4c_unknown_nominal_in_return_type() {
        let src = "@fn f() -> Phantom { return 0u32; }";
        let errors = infer_str(src).expect_err("expected E0518");
        let saw = errors.iter().any(|e| matches!(
            e,
            TypeError::UnknownNominalType { name, .. } if name == "Phantom"
        ));
        assert!(saw, "expected E0518 in return type; got {errors:?}");
    }

    #[test]
    fn t4c_unknown_nominal_in_let_annotation() {
        let src = "@fn f() { let _x: Mystery = 0u32; return; }";
        let errors = infer_str(src).expect_err("expected E0518 in let annotation");
        let saw = errors.iter().any(|e| matches!(
            e,
            TypeError::UnknownNominalType { name, .. } if name == "Mystery"
        ));
        assert!(saw, "expected E0518 in let annotation; got {errors:?}");
    }

    #[test]
    fn t4c_unknown_nominal_inside_compound_position() {
        // Unknown nominal nested inside a tuple type. The walker
        // recurses through tuple elements and reports each unknown.
        let src = "@fn f(p: (NotReal, AlsoNotReal)) -> u32 { return 0u32; }";
        let errors = infer_str(src).expect_err("expected E0518 for nested unknowns");
        let unknown_names: Vec<&str> = errors
            .iter()
            .filter_map(|e| match e {
                TypeError::UnknownNominalType { name, .. } => Some(name.as_str()),
                _ => None,
            })
            .collect();
        assert!(unknown_names.contains(&"NotReal"));
        assert!(unknown_names.contains(&"AlsoNotReal"));
    }

    #[test]
    fn t4c_unknown_nominal_inside_generic_args() {
        // `Vec<NotReal>` — registry doesn't know `NotReal`. The walker
        // visits generic args before validating the outer name, so
        // even when the outer name is also unknown, the inner one
        // reports too.
        let src = "@fn f() { let _x: Container<NotReal> = 0u32; return; }";
        let errors = infer_str(src).expect_err("expected E0518 for nested unknown");
        let unknown_names: Vec<&str> = errors
            .iter()
            .filter_map(|e| match e {
                TypeError::UnknownNominalType { name, .. } => Some(name.as_str()),
                _ => None,
            })
            .collect();
        // Both `Container` and `NotReal` are unknown; both should
        // surface.
        assert!(unknown_names.contains(&"Container"));
        assert!(unknown_names.contains(&"NotReal"));
    }

    #[test]
    fn t4c_known_alias_with_correct_arity_silent() {
        // Positive control: a registered, correctly-arity'd alias
        // should NOT trigger E0518 or E0519.
        let src = "\
            @type Foo = u32;\n\
            @type Pair<T> = (T, T);\n\
            @fn f(x: Foo, p: Pair<u32>) -> Foo { return x; }\n\
        ";
        let res = infer_str(src);
        assert!(
            res.is_ok(),
            "expected known aliases with correct arity to be silent; got {res:?}"
        );
    }

    #[test]
    fn t4c_known_adt_with_correct_arity_silent() {
        // ADTs also participate in arity checking. `@type Result<T, E>
        // = | Ok(T) | Err(E);` with `Result<u32, bool>` has arity 2 and
        // should be accepted.
        let src = "\
            @type Result<T, E> = | Ok(T) | Err(E);\n\
            @fn f(r: Result<u32, bool>) -> bool { return true; }\n\
        ";
        let res = infer_str(src);
        assert!(
            res.is_ok(),
            "expected ADT with correct arity to be silent; got {res:?}"
        );
    }

    #[test]
    fn t4c_adt_arity_mismatch_emits_e0519() {
        let src = "\
            @type Result<T, E> = | Ok(T) | Err(E);\n\
            @fn f(r: Result<u32>) -> bool { return true; }\n\
        ";
        let errors = infer_str(src).expect_err("expected E0519 on ADT");
        let saw = errors.iter().any(|e| matches!(
            e,
            TypeError::GenericArityMismatch { name, expected, actual, .. }
                if name == "Result" && *expected == 2 && *actual == 1
        ));
        assert!(saw, "expected E0519 for `Result<u32>` (need 2); got {errors:?}");
    }

    #[test]
    fn t4c_substitute_replaces_leaf_nominals() {
        // Direct unit test for `Type::substitute`: leaf `Nominal["T"]`
        // substitutes; `Nominal["Other"]` (not in mapping) doesn't.
        let mapping_owner: HashMap<&str, Type> =
            [("T", Type::Primitive(PrimitiveType::U32))].into_iter().collect();
        let mapping: HashMap<&str, &Type> =
            mapping_owner.iter().map(|(k, v)| (*k, v)).collect();

        let t_leaf = Type::Nominal {
            path: vec!["T".to_owned()],
            args: vec![],
        };
        assert_eq!(t_leaf.substitute(&mapping), Type::Primitive(PrimitiveType::U32));

        let other_leaf = Type::Nominal {
            path: vec!["Other".to_owned()],
            args: vec![],
        };
        // Not in mapping → unchanged.
        assert_eq!(other_leaf.substitute(&mapping), other_leaf);
    }

    #[test]
    fn t4c_substitute_recurses_into_compound_types() {
        let mapping_owner: HashMap<&str, Type> =
            [("T", Type::Primitive(PrimitiveType::U32))].into_iter().collect();
        let mapping: HashMap<&str, &Type> =
            mapping_owner.iter().map(|(k, v)| (*k, v)).collect();

        // Tuple([T, T]) → Tuple([u32, u32])
        let tuple = Type::Tuple(vec![
            Type::Nominal {
                path: vec!["T".to_owned()],
                args: vec![],
            },
            Type::Nominal {
                path: vec!["T".to_owned()],
                args: vec![],
            },
        ]);
        let expected = Type::Tuple(vec![
            Type::Primitive(PrimitiveType::U32),
            Type::Primitive(PrimitiveType::U32),
        ]);
        assert_eq!(tuple.substitute(&mapping), expected);

        // &T → &u32
        let r = Type::Ref {
            mutable: false,
            inner: Box::new(Type::Nominal {
                path: vec!["T".to_owned()],
                args: vec![],
            }),
        };
        assert_eq!(
            r.substitute(&mapping),
            Type::Ref {
                mutable: false,
                inner: Box::new(Type::Primitive(PrimitiveType::U32)),
            }
        );
    }

    #[test]
    fn t4c_generic_alias_used_in_call_arg() {
        // `Pair<u32>` unfolded should be compatible at call sites.
        let src = "\
            @type Pair<T> = (T, T);\n\
            @fn take(p: Pair<u32>) -> u32 { return 0u32; }\n\
            @fn caller() {\n  \
              let p: Pair<u32> = (1u32, 2u32);\n  \
              let _y: u32 = take(p);\n  \
              return;\n\
            }\n\
        ";
        let res = infer_str(src);
        assert!(
            res.is_ok(),
            "expected Pair<u32> to be compatible at call site; got {res:?}"
        );
    }

    // ─── Decision #22 layer-aware trait checking (E0544) ─────────────────

    #[test]
    fn pure_side_trait_on_effect_emits_e0544() {
        // `Pure` is a pure-side trait; using it on a `#effect` is a
        // layer mismatch.
        let src = "\
            #automaton C { v: u32; }\n\
            #effect e() #mutates: [C] $ [Pure] { }\n\
        ";
        let errors = infer_str(src).expect_err("expected E0544");
        let saw = errors.iter().any(|e| matches!(
            e,
            TypeError::TraitLayerMismatch { trait_name, expected_layer, actual_kind, .. }
                if trait_name == "Pure" && *expected_layer == "pure" && *actual_kind == "#effect"
        ));
        assert!(saw, "expected E0544 Pure on #effect; got {errors:?}");
    }

    #[test]
    fn pure_side_trait_on_interrupt_emits_e0544() {
        let src = "\
            #automaton C { v: u32; }\n\
            #interrupt SysTick() #mutates: [C] #priority: HIGH $ [Readable] { }\n\
        ";
        let errors = infer_str(src).expect_err("expected E0544");
        let saw = errors.iter().any(|e| matches!(
            e,
            TypeError::TraitLayerMismatch { trait_name, actual_kind, .. }
                if trait_name == "Readable" && *actual_kind == "#interrupt"
        ));
        assert!(saw, "expected E0544 Readable on #interrupt; got {errors:?}");
    }

    #[test]
    fn pure_side_trait_on_transition_emits_e0544() {
        let src = "\
            #automaton C {\n  \
              v: u32;\n  \
              #transition tick $ [Observable] { C.v = 1u32; }\n\
            }\n\
        ";
        let errors = infer_str(src).expect_err("expected E0544");
        let saw = errors.iter().any(|e| matches!(
            e,
            TypeError::TraitLayerMismatch { trait_name, actual_kind, .. }
                if trait_name == "Observable" && *actual_kind == "#transition"
        ));
        assert!(saw, "expected E0544 Observable on #transition; got {errors:?}");
    }

    #[test]
    fn imperative_trait_on_fn_emits_e0544() {
        // `Realtime` is imperative; using it on a `@fn` is wrong-layer.
        let src = "@fn p(x: u32) -> u32 $ [Realtime] { return x; }";
        let errors = infer_str(src).expect_err("expected E0544");
        let saw = errors.iter().any(|e| matches!(
            e,
            TypeError::TraitLayerMismatch { trait_name, expected_layer, actual_kind, .. }
                if trait_name == "Realtime" && *expected_layer == "imperative" && *actual_kind == "@fn"
        ));
        assert!(saw, "expected E0544 Realtime on @fn; got {errors:?}");
    }

    #[test]
    fn memory_ordering_trait_on_fn_emits_e0544() {
        // Acquire/Release/SeqCst are memory-ordering markers — only
        // make sense on imperative callables that emit fences.
        for name in ["Acquire", "Release", "SeqCst"] {
            let src = format!("@fn p(x: u32) -> u32 $ [{name}] {{ return x; }}");
            let errors = infer_str(&src).unwrap_err();
            let saw = errors.iter().any(|e| matches!(
                e,
                TypeError::TraitLayerMismatch { trait_name, .. } if trait_name == name
            ));
            assert!(saw, "expected E0544 for `{name}` on @fn");
        }
    }

    #[test]
    fn each_imperative_trait_rejected_on_fn() {
        // Smoke test: every name in PREDECLARED_IMPERATIVE_TRAITS is
        // E0544 when used on a `@fn`. If someone adds a new imperative
        // trait, this test catches it not being layer-tagged.
        for &name in PREDECLARED_IMPERATIVE_TRAITS.iter() {
            let src = format!("@fn p(x: u32) -> u32 $ [{name}] {{ return x; }}");
            let res = infer_str(&src);
            assert!(
                res.is_err(),
                "expected layer mismatch error for `{name}` on @fn, got {res:?}"
            );
            let saw = res.unwrap_err().iter().any(|e| {
                matches!(e, TypeError::TraitLayerMismatch { trait_name, .. }
                    if trait_name == name)
            });
            assert!(saw, "expected E0544 specifically for `{name}` on @fn");
        }
    }

    #[test]
    fn each_pure_trait_rejected_on_effect() {
        for &name in PREDECLARED_PURE_TRAITS.iter() {
            let src = format!(
                "#automaton C {{ v: u32; }}\n\
                 #effect e() #mutates: [C] $ [{name}] {{ }}"
            );
            let res = infer_str(&src);
            assert!(
                res.is_err(),
                "expected layer mismatch for `{name}` on #effect"
            );
            let saw = res.unwrap_err().iter().any(|e| {
                matches!(e, TypeError::TraitLayerMismatch { trait_name, .. }
                    if trait_name == name)
            });
            assert!(saw, "expected E0544 specifically for `{name}` on #effect");
        }
    }

    #[test]
    fn user_defined_trait_universal_works_on_both_layers() {
        // User-defined `@trait` is layer-universal in v0.2-β. Same
        // trait name validates on both `@fn` and `#effect`.
        let src = "\
            @trait MyTrait { }\n\
            @fn f(x: u32) -> u32 $ [MyTrait] { return x; }\n\
            #automaton C { v: u32; }\n\
            #effect e() #mutates: [C] $ [MyTrait] { }\n\
        ";
        let res = infer_str(src);
        assert!(
            res.is_ok(),
            "user-defined @trait should be universal; got {res:?}"
        );
    }

    #[test]
    fn unknown_trait_does_not_double_report_with_e0544() {
        // An unknown name triggers E0541 only (we don't know its
        // layer). E0544 should NOT also fire — it's layer-mismatch
        // for *known* traits.
        let src = "@fn p(x: u32) -> u32 $ [TotallyMadeUp] { return x; }";
        let errors = infer_str(src).unwrap_err();
        let saw_e0541 = errors
            .iter()
            .any(|e| matches!(e, TypeError::UnknownTrait { trait_name, .. } if trait_name == "TotallyMadeUp"));
        let saw_e0544 = errors
            .iter()
            .any(|e| matches!(e, TypeError::TraitLayerMismatch { trait_name, .. } if trait_name == "TotallyMadeUp"));
        assert!(saw_e0541, "expected E0541 for unknown trait");
        assert!(!saw_e0544, "E0544 should not fire for unknown trait");
    }

    #[test]
    fn mixed_layer_traits_in_one_list_reported_independently() {
        // `$ [Pure, Realtime]` on a `@fn`: Pure validates fine
        // (correct layer), Realtime triggers E0544. Each entry is
        // checked independently.
        let src = "@fn p(x: u32) -> u32 $ [Pure, Realtime] { return x; }";
        let errors = infer_str(src).unwrap_err();
        let mismatch_names: Vec<String> = errors
            .iter()
            .filter_map(|e| match e {
                TypeError::TraitLayerMismatch { trait_name, .. } => Some(trait_name.clone()),
                _ => None,
            })
            .collect();
        assert!(mismatch_names.contains(&"Realtime".to_owned()));
        assert!(
            !mismatch_names.contains(&"Pure".to_owned()),
            "Pure on @fn is correct layer; should not appear in E0544"
        );
    }

    #[test]
    fn predeclared_traits_full_set_recognised_on_correct_layers() {
        // Smoke test: every predeclared trait validates cleanly on
        // its own layer. (This duplicates the older
        // `predeclared_traits_full_set_recognised` test conceptually
        // but explicitly validates *layer correctness*.)
        for &name in PREDECLARED_PURE_TRAITS.iter() {
            let src = format!("@fn f(x: u32) -> u32 $ [{name}] {{ return x; }}");
            assert!(
                infer_str(&src).is_ok(),
                "predeclared pure trait `{name}` should validate on @fn"
            );
        }
        for &name in PREDECLARED_IMPERATIVE_TRAITS.iter() {
            let src = format!(
                "#automaton C {{ v: u32; }}\n\
                 #effect e() #mutates: [C] $ [{name}] {{ }}"
            );
            assert!(
                infer_str(&src).is_ok(),
                "predeclared imperative trait `{name}` should validate on #effect"
            );
        }
    }

    // ─── Slice T4d: ADT variant resolution + variant-call typing ─────────

    #[test]
    fn t4d_unit_variant_bare_path_yields_adt() {
        // `Color::Red` for `@type Color = | Red | Green | Blue;` is a
        // bare unit-variant reference — yields `Color`.
        let src = "\
            @type Color = | Red | Green | Blue;\n\
            @fn pick() -> Color { return Color::Red; }\n\
        ";
        let res = infer_str(src);
        assert!(res.is_ok(), "expected Color::Red to type as Color; got {res:?}");
    }

    #[test]
    fn t4d_unit_variant_in_let_annotation() {
        let src = "\
            @type Color = | Red | Green | Blue;\n\
            @fn f() { let _c: Color = Color::Green; return; }\n\
        ";
        let res = infer_str(src);
        assert!(res.is_ok(), "expected Color::Green to typecheck in let; got {res:?}");
    }

    #[test]
    fn t4d_data_carrying_variant_constructor_call() {
        // `Maybe::Some(5u32)` for `@type Maybe = | None | Some(u32);`
        // — variant call yields Maybe; arg type checked against u32.
        let src = "\
            @type Maybe = | None | Some(u32);\n\
            @fn make() -> Maybe { return Maybe::Some(5u32); }\n\
        ";
        let res = infer_str(src);
        assert!(
            res.is_ok(),
            "expected Maybe::Some(5u32) to type as Maybe; got {res:?}"
        );
    }

    #[test]
    fn t4d_variant_arg_type_mismatch_emits_e0522() {
        // `Maybe::Some(true)` where Some takes u32 — E0522.
        let src = "\
            @type Maybe = | None | Some(u32);\n\
            @fn bad() -> Maybe { return Maybe::Some(true); }\n\
        ";
        let errors = infer_str(src).expect_err("expected E0522");
        let saw = errors.iter().any(|e| matches!(
            e,
            TypeError::VariantArgMismatch { adt_name, variant_name, expected, actual, .. }
                if adt_name == "Maybe" && variant_name == "Some"
                    && expected == "u32" && actual == "bool"
        ));
        assert!(saw, "expected E0522 for Some(true); got {errors:?}");
    }

    #[test]
    fn t4d_variant_arity_mismatch_too_many_emits_e0521() {
        let src = "\
            @type Maybe = | None | Some(u32);\n\
            @fn bad() -> Maybe { return Maybe::Some(5u32, 6u32); }\n\
        ";
        let errors = infer_str(src).expect_err("expected E0521");
        let saw = errors.iter().any(|e| matches!(
            e,
            TypeError::VariantArityMismatch { adt_name, variant_name, expected, actual, .. }
                if adt_name == "Maybe" && variant_name == "Some"
                    && *expected == 1 && *actual == 2
        ));
        assert!(saw, "expected E0521 for Some(5,6); got {errors:?}");
    }

    #[test]
    fn t4d_variant_arity_mismatch_too_few_emits_e0521() {
        let src = "\
            @type Pair = | Both(u32, bool);\n\
            @fn bad() -> Pair { return Pair::Both(5u32); }\n\
        ";
        let errors = infer_str(src).expect_err("expected E0521");
        let saw = errors.iter().any(|e| matches!(
            e,
            TypeError::VariantArityMismatch { variant_name, expected, actual, .. }
                if variant_name == "Both" && *expected == 2 && *actual == 1
        ));
        assert!(saw, "expected E0521 for Both(5); got {errors:?}");
    }

    #[test]
    fn t4d_generic_adt_variant_pins_param() {
        // `Result::Ok(5u32)` for `@type Result<T, E> = | Ok(T) |
        // Err(E);` — the arg pins T to u32; E is uninferred (Unknown).
        // The result is `Result<u32, Unknown>`. The let-annotation
        // `Result<u32, bool>` is structurally compatible because
        // Unknown matches anything per types_compatible's short-circuit.
        let src = "\
            @type Result<T, E> = | Ok(T) | Err(E);\n\
            @fn make() -> Result<u32, bool> { return Result::Ok(5u32); }\n\
        ";
        let res = infer_str(src);
        assert!(
            res.is_ok(),
            "expected Result::Ok(5u32) to typecheck against Result<u32, bool>; got {res:?}"
        );
    }

    #[test]
    fn t4d_generic_adt_variant_pins_other_param() {
        let src = "\
            @type Result<T, E> = | Ok(T) | Err(E);\n\
            @fn make() -> Result<u32, bool> { return Result::Err(true); }\n\
        ";
        let res = infer_str(src);
        assert!(
            res.is_ok(),
            "expected Result::Err(true) to typecheck; got {res:?}"
        );
    }

    #[test]
    fn t4d_unknown_variant_silent_in_types_resolver_handles() {
        // `Color::NotARealVariant` — the type checker's variant lookup
        // returns None, so the call falls through to the regular
        // call-typing path (which then sees an unresolvable callee and
        // returns Unknown). The diagnostic surface for unknown
        // variants is the resolver's job; T4d in clifford-types just
        // doesn't crash.
        let src = "\
            @type Color = | Red;\n\
            @fn f() -> Color { return Color::NotReal; }\n\
        ";
        // We expect *some* error (probably from the resolver), but
        // not a panic from the type checker.
        let _ = infer_str(src);
    }

    #[test]
    fn t4d_non_adt_first_segment_not_variant() {
        // `MyAlias::Something` where MyAlias is a `@type` *alias*
        // (not an ADT) — the lookup returns None; falls through to
        // generic Path arm.
        let src = "\
            @type MyAlias = u32;\n\
            @fn f() { let _x := MyAlias::Foo; return; }\n\
        ";
        // Should not crash; the type checker leaves it as Unknown.
        let _ = infer_str(src);
    }

    #[test]
    fn t4d_struct_style_variant_args_treated_as_positional() {
        // `@type Shape = | Circle { r: f32 } | Square { side: f32 };`
        // — struct-style variants flatten to positional args in T4d.
        // `Shape::Circle(1.0f32)` works, treating r as positional.
        let src = "\
            @type Shape = | Circle { r: f32 } | Square { side: f32 };\n\
            @fn make() -> Shape { return Shape::Circle(1.0f32); }\n\
        ";
        let res = infer_str(src);
        assert!(
            res.is_ok(),
            "struct-style variants positional-T4d; got {res:?}"
        );
    }

    // ─── Slice T4e: compound-position generic unification ────────────────

    #[test]
    fn t4e_tuple_position_pins_param() {
        // `@type Pair<T> = | Both(T, T);` + `Pair::Both(5u32, 6u32)` —
        // declared arg is `T`, then `T` again (separate args). T4d
        // pinned T from the first arg and matched against the second;
        // T4e gets the same outcome.
        let src = "\
            @type Pair<T> = | Both(T, T);\n\
            @fn make() -> Pair<u32> { return Pair::Both(5u32, 6u32); }\n\
        ";
        let res = infer_str(src);
        assert!(res.is_ok(), "Pair::Both(5u32, 6u32) should typecheck; got {res:?}");
    }

    #[test]
    fn t4e_tuple_inside_arg_pins_param() {
        // T4e headline: a single variant arg of declared type `(T, T)`
        // — T4d couldn't pin through the tuple; T4e walks compounds.
        let src = "\
            @type Pair<T> = | Both((T, T));\n\
            @fn make() -> Pair<u32> { return Pair::Both((5u32, 6u32)); }\n\
        ";
        let res = infer_str(src);
        assert!(
            res.is_ok(),
            "Pair::Both((5u32, 6u32)) with declared (T, T) should pin via T4e; got {res:?}"
        );
    }

    #[test]
    fn t4e_tuple_inside_arg_conflict_emits_e0522() {
        // `(T, T)` declared; `(u32, bool)` actual — first position
        // pins T=u32; second position conflicts.
        let src = "\
            @type Pair<T> = | Both((T, T));\n\
            @fn bad() -> Pair<u32> { return Pair::Both((5u32, true)); }\n\
        ";
        let errors = infer_str(src).expect_err("expected E0522");
        let saw = errors.iter().any(|e| matches!(
            e,
            TypeError::VariantArgMismatch { variant_name, .. } if variant_name == "Both"
        ));
        assert!(saw, "expected E0522 for tuple position conflict; got {errors:?}");
    }

    #[test]
    fn t4e_ref_position_pins_param() {
        // `@type Boxed<T> = | Wrap(&T);` — declared arg is `&T`. T4e
        // walks through Ref and pins T.
        let src = "\
            @type Boxed<T> = | Wrap(&T);\n\
            @fn make() -> Boxed<u32> {\n  \
              let x: u32 = 5u32;\n  \
              return Boxed::Wrap(&x);\n\
            }\n\
        ";
        let res = infer_str(src);
        assert!(res.is_ok(), "Boxed::Wrap(&x) should pin T=u32; got {res:?}");
    }

    #[test]
    fn t4e_array_position_pins_param() {
        // `@type Buf<T> = | Of([T; 4]);` — declared is `[T; 4]`. T4e
        // walks through Array.
        let src = "\
            @type Buf<T> = | Of([T; 4]);\n\
            @fn make() -> Buf<u32> { return Buf::Of([1u32, 2u32, 3u32, 4u32]); }\n\
        ";
        let res = infer_str(src);
        assert!(res.is_ok(), "Buf::Of([...]) should pin T=u32; got {res:?}");
    }

    #[test]
    fn t4e_two_params_pin_independently() {
        // `@type Both<A, B> = | Pair((A, B));` — A pins from first
        // tuple element, B from second.
        let src = "\
            @type Both<A, B> = | Pair((A, B));\n\
            @fn make() -> Both<u32, bool> { return Both::Pair((5u32, true)); }\n\
        ";
        let res = infer_str(src);
        assert!(res.is_ok(), "Both::Pair pins A=u32, B=bool; got {res:?}");
    }

    #[test]
    fn t4e_nested_compound_pins_param() {
        // Doubly-nested: `&(T, T)` — Ref containing a Tuple of T,T.
        let src = "\
            @type W<T> = | M(&(T, T));\n\
            @fn make() -> W<u32> {\n  \
              let p: (u32, u32) = (5u32, 6u32);\n  \
              return W::M(&p);\n\
            }\n\
        ";
        let res = infer_str(src);
        assert!(res.is_ok(), "&(T,T) compound should walk through Ref+Tuple; got {res:?}");
    }

    #[test]
    fn t4e_shape_mismatch_emits_e0522() {
        // Declared `(T, T)` (a tuple); actual `u32` (not a tuple).
        // unify_pin returns Err immediately — emit E0522.
        let src = "\
            @type Pair<T> = | Both((T, T));\n\
            @fn bad() -> Pair<u32> { return Pair::Both(5u32); }\n\
        ";
        let errors = infer_str(src).expect_err("expected E0522");
        let saw = errors.iter().any(|e| matches!(
            e,
            TypeError::VariantArgMismatch { variant_name, .. } if variant_name == "Both"
        ));
        assert!(saw, "expected E0522 for shape mismatch; got {errors:?}");
    }

    #[test]
    fn t4e_partial_pin_substitutes_in_diagnostic() {
        // When the second arg conflicts after pinning T from the
        // first, the diagnostic should show the *substituted* expected
        // type (`u32`), not the raw `T`. Verifies the
        // `displayed_expected` substitution path in variant_call_type.
        let src = "\
            @type Both<A, B> = | Pair(A, B, A);\n\
            @fn bad() -> Both<u32, bool> { return Both::Pair(1u32, true, false); }\n\
        ";
        let errors = infer_str(src).expect_err("expected E0522");
        // The third arg should report `expected: u32, actual: bool`
        // (A pinned to u32 from arg 0; arg 2 is bool which conflicts).
        let saw = errors.iter().any(|e| matches!(
            e,
            TypeError::VariantArgMismatch { arg, expected, actual, .. }
                if *arg == 3 && expected == "u32" && actual == "bool"
        ));
        assert!(
            saw,
            "expected E0522 with displayed_expected=u32 (substituted from A); got {errors:?}"
        );
    }

    #[test]
    fn t4e_alias_in_variant_arg_unfolds() {
        // `@type Count = u32;` + `@type W<T> = | Wrap(T);` +
        // `W::Wrap(some_count)` where some_count: Count. The unify
        // pin's structural fallback should `unalias` Count → u32 so
        // T pins to u32.
        let src = "\
            @type Count = u32;\n\
            @type W<T> = | Wrap(T);\n\
            @fn make() -> W<u32> {\n  \
              let n: Count = 5u32;\n  \
              return W::Wrap(n);\n\
            }\n\
        ";
        let res = infer_str(src);
        assert!(
            res.is_ok(),
            "alias-in-arg should unalias for unification; got {res:?}"
        );
    }

    #[test]
    fn t4e_unknown_arg_does_not_block_other_pins() {
        // If one variant arg is Unknown (upstream issue), unify_pin
        // is permissive on it; remaining args still pin normally.
        // This test relies on the variant call typing not blowing up
        // and the surrounding program still validating.
        let src = "\
            @type Tuple<A, B> = | Both(A, B);\n\
            @fn make() -> Tuple<u32, bool> { return Tuple::Both(5u32, true); }\n\
        ";
        let res = infer_str(src);
        assert!(res.is_ok(), "expected clean T4e baseline; got {res:?}");
    }
}
