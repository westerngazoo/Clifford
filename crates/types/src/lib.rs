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
//! **Slice 1 (this PR):** literal-type inference + simple primitive expression
//! typing. The public entry point [`infer`] walks every `@fn` / `#effect` /
//! `#interrupt` / `#transition` body and assigns a [`Type`] to each
//! expression node. Integer literal suffix recognition (`0u32` → `u32`),
//! Path → local-type resolution via the `Resolution` from `clifford-resolve`,
//! unary and binary operations on primitives with operand-type compatibility
//! checking, and `let`-annotation-vs-initializer compatibility checking are
//! all in scope. Function-call typing, field-access typing, generic
//! instantiation, trait satisfaction, and full HM unification are deferred
//! to subsequent slices.

#![forbid(unsafe_code)]
#![warn(missing_docs)]

use std::collections::HashMap;

use clifford_ast::{
    Block, EffectDecl, Expr, ExprKind, FnDecl, InterruptDecl, Item, PrimitiveType, Program, Stmt,
    StmtKind, TransitionDecl, TypeExpr, TypeKind, UnaryOp,
};
use clifford_ast::{AutomatonDecl, BinaryOp};
use clifford_lexer::Span;
use clifford_resolve::Resolution;
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
}

/// A fully-resolved Clifford type — the abstract counterpart to `TypeExpr`
/// from the AST.
///
/// `TypeExpr` is the *syntactic* form (carries source spans, raw identifier
/// segments, etc.); `Type` is the *semantic* form (canonical, comparable,
/// produced by the type checker once names have been resolved).
///
/// Slice-1 scope: only [`Type::Unit`], [`Type::Primitive`], and
/// [`Type::Unknown`] (placeholder for expressions whose type the slice-1
/// engine cannot yet compute). Generic parameters, references, slices,
/// arrays, tuples, function-pointer types, ADTs, and access types arrive
/// in subsequent slices alongside their specific use cases.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum Type {
    /// `()` — the unit type.
    Unit,
    /// One of the predeclared primitive types from §4.1.
    Primitive(PrimitiveType),
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
    let mut walker = Inferer {
        resolution,
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

// ─── Internal walker ────────────────────────────────────────────────────────

struct Inferer<'a> {
    resolution: &'a Resolution,
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
            ExprKind::FieldAccess { obj, .. } => {
                let _ = self.infer_expr(obj);
                Type::Unknown("field-access typing is slice T2 work")
            }
            ExprKind::Index { obj, index } => {
                let _ = self.infer_expr(obj);
                let _ = self.infer_expr(index);
                Type::Unknown("index typing is slice T2 work")
            }
            ExprKind::Call { callee, args } => {
                let _ = self.infer_expr(callee);
                for a in args {
                    let _ = self.infer_expr(a);
                }
                Type::Unknown("function-call typing is slice T2 work")
            }
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

            // Borrow expression — `&T` / `&mut T`. Slice 2 introduces
            // reference types; for now record the operand and return Unknown.
            ExprKind::Ref { operand, .. } => {
                let _ = self.infer_expr(operand);
                Type::Unknown("reference typing is slice T2 work")
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
                // Reference / access types are slice 2.
                Type::Unknown("deref typing is slice T2 work")
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
        TypeKind::Path(_) => Type::Unknown("nominal-path type lookup is slice T2 work"),
        TypeKind::Ref(_) => Type::Unknown("reference type is slice T2 work"),
        TypeKind::Access(_) => Type::Unknown("access<T> type is slice T2 work"),
        TypeKind::Array(_) => Type::Unknown("array type is slice T2 work"),
        TypeKind::Slice(_) => Type::Unknown("slice type is slice T2 work"),
        TypeKind::Tuple(_) => Type::Unknown("tuple type is slice T2 work"),
        TypeKind::Fn(_) => Type::Unknown("@fn pointer type is slice T2 work"),
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
}
