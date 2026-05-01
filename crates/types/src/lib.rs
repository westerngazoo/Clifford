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
//! **Slice 2 (this PR):** function-call typing, automaton-field typing,
//! reference types. A `SignatureRegistry` precomputes every top-level
//! `@fn` / `#effect` / `#interrupt` signature; `Expr::Call` arguments
//! are checked against the callee's parameter types (E0513) and the call
//! result is the declared return type. Field-access on automaton symbols
//! consumes the resolver's `AutomatonField` bindings to look up the
//! declared field type. New `Type::Ref { mutable, inner }` variant for
//! `&x` / `&mut x` borrow expressions; deref `*r` unwraps it. Index,
//! tuple, range, method-call, generic instantiation, and trait satisfaction
//! all remain deferred to slice T3.

#![forbid(unsafe_code)]
#![warn(missing_docs)]

use std::collections::HashMap;

use clifford_ast::{
    AutomatonDecl, BinaryOp, Block, EffectDecl, Expr, ExprKind, FnDecl, InterruptDecl, Item,
    Param, PrimitiveType, Program, Stmt, StmtKind, TransitionDecl, TypeExpr, TypeKind, UnaryOp,
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
    #[error("E0512: `let {name}: {declared}` does not match initializer type {actual} (at byte {at})")]
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
    #[error("E0514: call to `{callee}` expected {expected} argument(s), got {actual} (at byte {at})")]
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
}

/// A fully-resolved Clifford type — the abstract counterpart to `TypeExpr`
/// from the AST.
///
/// `TypeExpr` is the *syntactic* form (carries source spans, raw identifier
/// segments, etc.); `Type` is the *semantic* form (canonical, comparable,
/// produced by the type checker once names have been resolved).
///
/// Slice-2 scope: [`Type::Unit`], [`Type::Primitive`], [`Type::Ref`],
/// [`Type::StringSlice`], and [`Type::Unknown`]. Slice arrays, tuples,
/// fn-pointer types, ADTs, and access types arrive in subsequent slices
/// alongside their specific use cases.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum Type {
    /// `()` — the unit type.
    Unit,
    /// One of the predeclared primitive types from §4.1.
    Primitive(PrimitiveType),
    /// `&T` (immutable) or `&mut T` — a body-scoped reference per Decision #13.
    /// Slice 2 introduces this variant for borrow-expression typing and
    /// deref-typing. Slice / array reference subtleties land in slice T3.
    Ref {
        /// `true` for `&mut T`, `false` for `&T`.
        mutable: bool,
        /// The referenced type.
        inner: Box<Type>,
    },
    /// `&[u8]` for string literals — modelled here so slice 1 doesn't need
    /// the full slice-type machinery from §4. When real slice/reference
    /// types land, this collapses into the general representation.
    StringSlice,
    /// The type checker could not yet compute this expression's type.
    /// Carries a brief reason string for diagnostics. Downstream phases
    /// that consume `Unknown` should treat it as "type information not
    /// available" rather than as a default — emitting their own error
    /// message that points at the specific limitation.
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
            Self::StringSlice => "&[u8]".to_owned(),
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
        matches!(self, Self::Primitive(PrimitiveType::F32 | PrimitiveType::F64))
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

    let mut walker = Inferer {
        resolution,
        signatures: &signatures,
        automaton_field_types: &automaton_field_types,
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

    if walker.errors.is_empty() {
        Ok(Typing { types: walker.types })
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
                map.insert(decl.name.clone(), signature_from_params(&decl.params, decl.return_type.as_ref()));
            }
            Item::Effect(decl) => {
                map.insert(decl.name.clone(), signature_from_params(&decl.params, decl.return_type.as_ref()));
            }
            Item::Interrupt(decl) => {
                map.insert(decl.name.clone(), signature_from_params(&decl.params, decl.return_type.as_ref()));
            }
            _ => {}
        }
    }
    map
}

fn signature_from_params(params: &[Param], return_type: Option<&TypeExpr>) -> Signature {
    Signature {
        params: params.iter().map(|p| type_from_type_expr(&p.ty)).collect(),
        return_type: return_type
            .map(type_from_type_expr)
            .unwrap_or(Type::Unit),
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
                        && !types_compatible(&declared, &value_ty)
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
            // Top-level symbols don't have a useful expression-level type
            // in slice 1 (calling functions is slice 2 work).
            ExprKind::Path(segments) => {
                if segments.len() == 1 {
                    self.lookup_local(&segments[0])
                        .cloned()
                        .unwrap_or(Type::Unknown(
                            "name does not resolve to a local; top-level typing is slice T2 work",
                        ))
                } else {
                    Type::Unknown("multi-segment path typing is slice T2+ work")
                }
            }

            // StateRead — should be a state-tag enum type per §4. Slice 2.
            ExprKind::StateRead(_) => Type::Unknown("state-tag typing is slice T2 work"),

            ExprKind::Paren(inner) => self.infer_expr(inner),

            // Compound forms whose type slice 1 doesn't compute.
            ExprKind::Tuple(elems) | ExprKind::Array(elems) => {
                for e in elems {
                    let _ = self.infer_expr(e);
                }
                Type::Unknown("tuple/array type construction is slice T2 work")
            }
            ExprKind::ArrayRepeat { value, count } => {
                let _ = self.infer_expr(value);
                let _ = self.infer_expr(count);
                Type::Unknown("array-repeat type construction is slice T2 work")
            }
            ExprKind::FieldAccess { obj, field } => {
                let _ = self.infer_expr(obj);
                self.field_access_type(expr.span, field)
            }
            ExprKind::Index { obj, index } => {
                let _ = self.infer_expr(obj);
                let _ = self.infer_expr(index);
                Type::Unknown("index typing is slice T2 work")
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

            ExprKind::Range { lo, hi, .. } => {
                let _ = self.infer_expr(lo);
                let _ = self.infer_expr(hi);
                Type::Unknown("range typing is slice T2+ work")
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
    fn call_type(&mut self, callee: &Expr, args: &[Expr], at: Span) -> Type {
        let arg_types: Vec<Type> = args.iter().map(|a| self.infer_expr(a)).collect();

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
                && !types_compatible(expected, actual)
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

    /// Type a `FieldAccess` expression. The resolver already validated that
    /// `obj` resolves to an automaton (or this isn't an automaton field at
    /// all); slice 2 looks up the field's declared type via the resolver's
    /// `BindingRef::AutomatonField` recorded under the FieldAccess's span.
    fn field_access_type(&self, span: Span, field: &str) -> Type {
        let Some(BindingRef::AutomatonField { automaton, .. }) =
            self.resolution.lookup(span)
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
            BinaryOp::Eq | BinaryOp::Ne | BinaryOp::Lt | BinaryOp::Le | BinaryOp::Gt | BinaryOp::Ge => {
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
                let op_name = if matches!(op, BinaryOp::And) { "&&" } else { "||" };
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
                let op_name = if matches!(op, BinaryOp::Shl) { "<<" } else { ">>" };
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

/// Two types are compatible for `let`-annotation matching iff they're
/// structurally equal. (`Unknown` is treated as compatible with anything to
/// avoid cascading errors when an upstream type is unknown.)
fn types_compatible(declared: &Type, actual: &Type) -> bool {
    if declared.is_unknown() || actual.is_unknown() {
        return true;
    }
    declared == actual
}

/// Translate a syntactic [`TypeExpr`] into a semantic [`Type`].
///
/// Slice-1 scope: only `Unit` and `Primitive(...)` shapes resolve cleanly.
/// References, slices, arrays, tuples, function types, paths, and access
/// types all return [`Type::Unknown`] for now — those land in slice T2+
/// alongside their use cases.
fn type_from_type_expr(t: &TypeExpr) -> Type {
    match &t.kind {
        TypeKind::Unit => Type::Unit,
        TypeKind::Primitive(p) => Type::Primitive(*p),
        TypeKind::Ref(rt) => {
            // The inner type is recursively translated; a `&[u8]` parameter
            // (the most common shape in real code) translates to
            // `Ref { inner: Unknown("slice...") }` for now since
            // `Slice` typing itself is slice T3 work. That's still useful:
            // the "this is a reference" property is preserved.
            Type::Ref {
                mutable: rt.mutable,
                inner: Box::new(type_from_type_expr(&rt.inner)),
            }
        }
        TypeKind::Path(_) => Type::Unknown("nominal-path type lookup is slice T3 work"),
        TypeKind::Access(_) => Type::Unknown("access<T> type is slice T3 work"),
        TypeKind::Array(_) => Type::Unknown("array type is slice T3 work"),
        TypeKind::Slice(_) => Type::Unknown("slice type is slice T3 work"),
        TypeKind::Tuple(_) => Type::Unknown("tuple type is slice T3 work"),
        TypeKind::Fn(_) => Type::Unknown("@fn pointer type is slice T3 work"),
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
        assert!(errors.iter().any(|e| matches!(
            e,
            TypeError::UnaryTypeMismatch { op: "-", .. }
        )));
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
        assert!(errors.iter().any(|e| matches!(
            e,
            TypeError::UnaryTypeMismatch { op: "!", .. }
        )));
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
        assert!(errors.iter().any(|e| matches!(
            e,
            TypeError::UnaryTypeMismatch { op: "~", .. }
        )));
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
        assert!(errors.iter().any(|e| matches!(
            e,
            TypeError::BinaryTypeMismatch { op: "+", .. }
        )));
    }

    #[test]
    fn arithmetic_on_bool_is_e0510() {
        let errors = infer_str("@fn f() { let _x := true + false; }").unwrap_err();
        assert!(errors.iter().any(|e| matches!(
            e,
            TypeError::BinaryTypeMismatch { op: "+", .. }
        )));
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
        assert!(errors.iter().any(|e| matches!(
            e,
            TypeError::BinaryTypeMismatch { op: "<", .. }
        )));
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
        assert!(errors.iter().any(|e| matches!(
            e,
            TypeError::BinaryTypeMismatch { op: "&&", .. }
        )));
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
        assert!(errors.iter().any(|e| matches!(
            e,
            TypeError::BinaryTypeMismatch { op: "^", .. }
        )));
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
        assert!(errors.iter().any(|e| matches!(
            e,
            TypeError::BinaryTypeMismatch { op: "<<", .. }
        )));
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
        let typing = infer_str(
            "#effect e(p: u32) #mutates: [] { let _x := #unchecked_load<u8>(p); }",
        )
        .unwrap();
        assert!(types_in(&typing)
            .into_iter()
            .any(|t| matches!(t, Type::Primitive(PrimitiveType::U8))));
    }

    #[test]
    fn volatile_load_returns_type_argument() {
        let typing = infer_str(
            "#effect e(p: u32) #mutates: [] { let _x := #volatile_load<u32>(p); }",
        )
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
        let errors =
            infer_str("@fn f() { let _x := -true; let _y := 1u32 + 2u8; }").unwrap_err();
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
        let res = infer_str(
            "@fn caller() { let helper := 0u32; let _y := helper(); }",
        );
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
}
