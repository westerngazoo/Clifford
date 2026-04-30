//! # cliffordc — the Clifford command-line driver
//!
//! Per CLAUDE.md §6 Phase 5: "The CLI driver is thin. Real logic lives in the
//! library crates." This binary wires together the pipeline:
//!
//! ```text
//! lexer → parser → ast → resolve → types → check → effect → ortho → codegen
//! ```
//!
//! Each phase is a separate library crate; this driver is mostly arg-parsing
//! and orchestration. Diagnostics use `codespan-reporting` for nice rendering
//! per CLAUDE.md §6 Phase 5.
//!
//! ## v0.1 subcommands
//!
//! ```text
//! cliffordc compile <file.cl>      Compile a Clifford source file to LLVM IR.
//!     --target <triple>            Target triple (default: host).
//!     --verbose-basis              Dump GA basis assignments to .basis.json.
//!     --verify-invariants=static   Attempt SMT discharge of #invariant clauses.
//!
//! cliffordc test                   Discover and run #test blocks.
//!
//! cliffordc lint                   Run lints.
//!     --max-cast-chain=N           Fail if any function has > N casts (Refinement #19b).
//!     --max-unsafe-ops=N           Fail if too many narrow unsafe primitives.
//!     --require-fsm-on-driver      Warn on automata with many effects but no #states.
//!
//! cliffordc audit                  Audit unsafe operations.
//!     --list-unsafe                Print every #unchecked_*/#volatile_*/#asm site
//!                                  with its reason string (Refinement #19a).
//!
//! cliffordc inspect                Introspection.
//!     --as-category <Automaton>    Render an automaton's category C_A as DOT.
//! ```

#![forbid(unsafe_code)]

use anyhow::Result;

fn main() -> Result<()> {
    // Phase 5 scaffolding — real CLI parsing lands during driver
    // implementation. For now, just print a banner so the binary does
    // something visible to a `cargo run`.
    eprintln!(
        "cliffordc {} — Clifford language compiler\n\
         Phase 0 scaffolding; see docs/CLIFFORD_SPEC.md and docs/DECISIONS.md.",
        env!("CARGO_PKG_VERSION"),
    );
    Ok(())
}
