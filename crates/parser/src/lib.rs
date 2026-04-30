//! # clifford-parser
//!
//! Recursive-descent parser for the Clifford language. Implements §2 (Grammar)
//! and §3 (Parser Behavior) of `docs/CLIFFORD_SPEC.md`. Phase 0 of the
//! implementation roadmap (§11).
//!
//! ## Approach
//!
//! Recursive descent with one-token lookahead, augmented by sigil-driven
//! dispatch (§3 of the spec):
//!
//! 1. Sigil dispatch at item position: the leading sigil (`@`, `#`) selects
//!    which item-grammar to enter.
//! 2. Sigil dispatch at statement position: inside a `#`-context body, leading
//!    `#mutate`, `#>`, narrow unsafe primitives, etc. select the statement form.
//! 3. Generic vs. less-than disambiguation: bounded backtracking when `<`
//!    could begin a generic argument list.
//! 4. Inline effect metadata: `#effect`/`#interrupt` declarations consume zero
//!    or more `effect_meta` clauses before the body block.
//! 5. `#states` omission default (Decision #5): missing `#states` ⇒ inserted
//!    synthetic `[Ready]` and the AST is marked as a *monoid automaton*.
//! 6. Register-block automaton dispatch (Decision #6): `#address` clause marks
//!    the AST node as a register block; every field requires `#offset`.
//! 7. Call-site context classification (Refinement #5b generalisation):
//!    `#> name(args)` callees are tagged Transition / Identity / Generic per
//!    callee kind during name resolution.
//! 8. Interface-method dispatch (Decision #16): `#> Name::method(args)` where
//!    `Name` is a generic parameter is recorded as a `Generic` call site.
//! 9. Sigma-loop parsing (Decision #14): the `sigma` keyword opens a
//!    `sigma_expr`; bound annotations attached to the iteration variable.
//!
//! ## Error recovery
//!
//! Per CLAUDE.md §6 Phase 0, the parser produces a partial AST and reports
//! all errors, not just the first. Resync points are at item boundaries,
//! statement separators, and closing braces.
//!
//! ## Round-trip property
//!
//! `source → AST → pretty-print → AST` is identity modulo whitespace
//! (CLAUDE.md §6 Phase 0 property test requirement).

#![forbid(unsafe_code)]
#![warn(missing_docs)]

use clifford_ast::Program;
use clifford_lexer::Token;
use thiserror::Error;

/// Errors produced during parsing.
///
/// Per CLAUDE.md §3.4, every error carries a stable error code. The parser
/// reserves the `E02xx` range.
#[derive(Debug, Error)]
pub enum ParseError {
    /// Placeholder for Phase 0 scaffolding.
    #[error("E0200: parser not yet implemented")]
    NotYetImplemented,
}

/// Parse a token stream into a [`Program`] (the root AST node).
///
/// Returns the constructed AST on success. Phase 0 scaffolding always returns
/// an empty program; the real parser lands per §2 of the spec.
///
/// # Examples
///
/// ```
/// use clifford_lexer::tokenize;
/// use clifford_parser::parse;
///
/// let tokens = tokenize("").unwrap();
/// let program = parse(&tokens).unwrap();
/// // Empty input → empty program.
/// # let _ = program;
/// ```
///
/// # Errors
///
/// Returns the first [`ParseError`] encountered. Phase 0 scaffolding always
/// succeeds with an empty program.
pub fn parse(_tokens: &[Token]) -> Result<Program, ParseError> {
    // Phase 0 scaffolding: real implementation lands per §2 / §3.
    Ok(Program::default())
}

#[cfg(test)]
mod tests {
    use super::*;
    use clifford_lexer::tokenize;

    #[test]
    fn empty_input_parses_to_empty_program() {
        let tokens = tokenize("").expect("tokenize empty");
        let _program = parse(&tokens).expect("parse empty token stream");
    }
}
