//! # cliffordc — the Clifford command-line driver
//!
//! Per CLAUDE.md §6 Phase 5: "The CLI driver is thin. Real logic lives in the
//! library crates." This binary wires together the pipeline:
//!
//! ```text
//! lexer → parser → ast → resolve → types → codegen
//! ```
//!
//! Each phase is a separate library crate; this driver is mostly arg-parsing
//! and orchestration.
//!
//! ## v0.1 subcommand surface
//!
//! ```text
//! cliffordc compile <file.cl> [-o <out.ll>] [--module-name <name>]
//!     Compile a single Clifford source file to LLVM IR text.
//!     Default output is `<file_stem>.ll` next to the input.
//!     Default module-name is the input file's basename.
//!
//! cliffordc --version | -V        Print version.
//! cliffordc --help    | -h        Print help.
//! ```
//!
//! v0.2+ subcommands (`test`, `lint`, `audit`, `inspect`) are sketched in the
//! `usage()` text but not yet wired.
//!
//! ## Exit codes
//!
//! - `0` — success
//! - `1` — compilation error (lex / parse / resolve / type / codegen)
//! - `2` — usage error (bad arguments)
//! - `3` — I/O error (input unreadable, output unwritable)

#![forbid(unsafe_code)]

use std::path::{Path, PathBuf};
use std::process::ExitCode;

use clifford_check::check;
use clifford_codegen::lower;
use clifford_effect::{extract_call_graph, extract_categories, extract_mutation_profiles};
use clifford_lexer::tokenize;
use clifford_ortho::verify as verify_ortho;
use clifford_parser::parse;
use clifford_resolve::resolve;
use clifford_types::infer;
use codespan_reporting::diagnostic::{Diagnostic, Label};
use codespan_reporting::files::SimpleFile;
use codespan_reporting::term;
use codespan_reporting::term::termcolor::{ColorChoice, StandardStream};

/// Top-level CLI invocation, parsed from `std::env::args`.
#[derive(Debug, PartialEq, Eq)]
enum Cli {
    /// `cliffordc compile <input> [-o <output>] [--module-name <name>]`.
    Compile {
        input: PathBuf,
        output: Option<PathBuf>,
        module_name: Option<String>,
    },
    /// `cliffordc --version` / `-V`.
    Version,
    /// `cliffordc --help` / `-h` / no args.
    Help,
    /// Anything we don't recognise; printed to stderr alongside `Help`.
    Unknown(String),
}

fn main() -> ExitCode {
    let argv: Vec<String> = std::env::args().skip(1).collect();
    match parse_argv(&argv) {
        Cli::Compile {
            input,
            output,
            module_name,
        } => match run_compile(&input, output.as_deref(), module_name.as_deref()) {
            Ok(()) => ExitCode::SUCCESS,
            Err(CompileError::Io(msg)) => {
                eprintln!("cliffordc: I/O error: {msg}");
                ExitCode::from(3)
            }
            // Slice 16: phase errors are now rendered inside
            // `run_compile` (which has the source text in scope and
            // can pass it to codespan-reporting). By this point
            // they've already been emitted to stderr.
            Err(CompileError::Phase { .. }) => ExitCode::from(1),
        },
        Cli::Version => {
            println!("cliffordc {}", env!("CARGO_PKG_VERSION"));
            ExitCode::SUCCESS
        }
        Cli::Help => {
            print!("{USAGE}");
            ExitCode::SUCCESS
        }
        Cli::Unknown(arg) => {
            eprintln!("cliffordc: unrecognised argument: `{arg}`\n");
            eprint!("{USAGE}");
            ExitCode::from(2)
        }
    }
}

/// Help text printed for `--help`, no args, and unrecognised arguments.
const USAGE: &str = "\
cliffordc — Clifford language compiler

USAGE:
    cliffordc compile <input.cl> [-o <output.ll>] [--module-name <name>]
    cliffordc --version | -V
    cliffordc --help    | -h

COMPILE OPTIONS:
    -o <path>             Output file (default: input with `.ll` extension)
    --module-name <name>  LLVM IR `ModuleID` (default: input file basename)

EXIT CODES:
    0  success
    1  compilation error (lex / parse / resolve / type / codegen)
    2  usage error (bad arguments)
    3  I/O error (input unreadable, output unwritable)
";

/// Parse `argv` (without the program name) into a [`Cli`] variant. The
/// parser is intentionally hand-rolled — we own a stable surface and the
/// args are too few to justify a clap dependency.
fn parse_argv(argv: &[String]) -> Cli {
    if argv.is_empty() {
        return Cli::Help;
    }
    match argv[0].as_str() {
        "--help" | "-h" => Cli::Help,
        "--version" | "-V" => Cli::Version,
        "compile" => parse_compile(&argv[1..]),
        other => Cli::Unknown(other.to_owned()),
    }
}

/// Parse the args following `compile`. Returns `Cli::Unknown` for any
/// unparseable shape so `main` can route the message uniformly.
fn parse_compile(args: &[String]) -> Cli {
    let mut input: Option<PathBuf> = None;
    let mut output: Option<PathBuf> = None;
    let mut module_name: Option<String> = None;

    let mut i = 0;
    while i < args.len() {
        let arg = &args[i];
        match arg.as_str() {
            "-o" => {
                i += 1;
                let Some(val) = args.get(i) else {
                    return Cli::Unknown("-o (missing path)".to_owned());
                };
                output = Some(PathBuf::from(val));
            }
            "--module-name" => {
                i += 1;
                let Some(val) = args.get(i) else {
                    return Cli::Unknown("--module-name (missing value)".to_owned());
                };
                module_name = Some(val.clone());
            }
            _ if !arg.starts_with('-') && input.is_none() => {
                input = Some(PathBuf::from(arg));
            }
            _ => {
                return Cli::Unknown(format!("compile: unexpected argument `{arg}`"));
            }
        }
        i += 1;
    }
    match input {
        Some(input) => Cli::Compile {
            input,
            output,
            module_name,
        },
        None => Cli::Unknown("compile: missing input file".to_owned()),
    }
}

/// Outcome of `run_compile`. `Phase` carries one or more structured
/// per-error diagnostics so the caller can render them via
/// `codespan-reporting` with the source file in scope; `Io` carries
/// a system-level error.
#[derive(Debug)]
enum CompileError {
    Phase {
        /// Phase identifier — e.g. `"parse"`, `"resolve"`, `"check"`.
        /// Used as the diagnostic's `code` (`error[parse]: …`).
        name: &'static str,
        /// One entry per error reported by the phase. Each carries the
        /// raw error message (already containing the `EXXXX:` code
        /// from `thiserror`) plus an optional source byte offset
        /// extracted from the message text.
        diags: Vec<PhaseDiag>,
    },
    Io(String),
}

/// One error diagnostic, with the byte offset extracted (if the
/// underlying error message included one) so codespan-reporting can
/// render the source line + caret. Slice 16.
#[derive(Debug)]
struct PhaseDiag {
    /// Human-readable error message including the `EXXXX:` code.
    message: String,
    /// Source byte offset extracted from the message via
    /// [`byte_offset_from_msg`]. `None` for errors that don't carry
    /// a position (e.g. `E0205 unexpected end of input`,
    /// `E0500 ortho not yet implemented`).
    primary_offset: Option<usize>,
}

impl PhaseDiag {
    /// Build a diagnostic from a `Display`-able error, automatically
    /// extracting the byte offset via [`byte_offset_from_msg`].
    fn from_error<E: std::fmt::Display>(e: &E) -> Self {
        let message = format!("{e}");
        let primary_offset = byte_offset_from_msg(&message);
        PhaseDiag {
            message,
            primary_offset,
        }
    }

    /// Build a diagnostic from a single pre-formatted message, without
    /// trying to extract a byte offset. Used by phases that produce
    /// just one error (e.g. lex / parse) so we can stay symmetric
    /// with the multi-error phases.
    fn from_single<E: std::fmt::Display>(e: &E) -> Vec<Self> {
        vec![PhaseDiag::from_error(e)]
    }
}

/// Slice 16: regex-free extraction of the first `byte N` decimal in
/// an error message. `clifford-*` errors universally include
/// `at byte 1234` (or variants like `(at byte 1234)`,
/// `referenced at byte 1234`); this helper finds the literal text
/// `byte ` followed by an ASCII digit run and parses out the
/// number.
///
/// Returns `None` for messages that don't include a `byte N`
/// pattern — those errors are rendered without a source line
/// (e.g. cycle reports, "not yet implemented" stubs).
fn byte_offset_from_msg(msg: &str) -> Option<usize> {
    let needle = "byte ";
    let mut search_from = 0;
    while let Some(idx) = msg[search_from..].find(needle) {
        let start = search_from + idx + needle.len();
        let bytes = msg.as_bytes();
        let mut end = start;
        while end < bytes.len() && bytes[end].is_ascii_digit() {
            end += 1;
        }
        if end > start {
            if let Ok(n) = msg[start..end].parse::<usize>() {
                return Some(n);
            }
        }
        // Found "byte " but no digits after it; advance past this
        // occurrence and keep looking.
        search_from = start;
    }
    None
}

/// Run the full compile pipeline for one source file. Reads the source,
/// runs every phase, and writes the resulting IR text to disk. Returns
/// the first phase failure if any phase fails.
fn run_compile(
    input: &Path,
    output: Option<&Path>,
    module_name: Option<&str>,
) -> Result<(), CompileError> {
    // Read the source file.
    let source = std::fs::read_to_string(input).map_err(|e| {
        CompileError::Io(format!("could not read `{}`: {e}", input.display()))
    })?;

    // Resolve defaults: output path = input with `.ll` extension; module
    // name = input file's stem (no extension, no parent dirs).
    let output_path: PathBuf = output
        .map(Path::to_path_buf)
        .unwrap_or_else(|| default_output_path(input));
    let module_name_owned: String = module_name
        .map(str::to_owned)
        .unwrap_or_else(|| default_module_name(input));

    // Slice 16: render phase errors via codespan-reporting before
    // bubbling the error up. main() doesn't have the source text
    // in scope; doing the render here keeps the source string
    // alive across the codespan-reporting borrow.
    let ir = match compile_source(&source, &module_name_owned) {
        Ok(ir) => ir,
        Err(CompileError::Phase { name, diags }) => {
            render_phase_error(&input.display().to_string(), &source, name, &diags);
            return Err(CompileError::Phase { name, diags });
        }
        Err(e) => return Err(e),
    };

    std::fs::write(&output_path, ir).map_err(|e| {
        CompileError::Io(format!(
            "could not write `{}`: {e}",
            output_path.display()
        ))
    })?;

    eprintln!(
        "cliffordc: wrote {} ({} bytes from {})",
        output_path.display(),
        std::fs::metadata(&output_path)
            .map(|m| m.len())
            .unwrap_or(0),
        input.display(),
    );
    Ok(())
}

/// Run the full pipeline against a source string:
///
/// ```text
/// tokenize → parse → resolve → infer → check → effect → codegen
/// ```
///
/// Slice 15 wired the four upstream gates (`check`, `extract_categories`,
/// `extract_mutation_profiles`, `extract_call_graph`) into the CLI so
/// programs that violate sigil-layer / mutation-authorisation / category
/// / call-graph invariants are rejected with structured diagnostics
/// before codegen runs. `clifford-ortho` is still scaffolding (only
/// `outer_product` exists; no top-level orthogonality verifier yet),
/// so it remains a stub — the v0.1 release ships with the gates that
/// exist today and earmarks ortho integration for the next slice that
/// implements §7.
///
/// Errors are pre-formatted with phase prefixes so the caller can
/// `eprintln!` them verbatim. The phase-prefix taxonomy is:
///
/// - `error[lex]:`     — tokenisation failure
/// - `error[parse]:`   — syntactic error
/// - `error[resolve]:` — name resolution / mutability violations
/// - `error[types]:`   — type inference / annotation mismatches
/// - `error[check]:`   — sigil-layer / mutation auth / totality
/// - `error[effect]:`  — categorical / mutation-profile / call-graph
/// - `error[ortho]:`   — §7 GA orthogonality (write-write race detection)
/// - `error[codegen]:` — IR-emission gap (NotYetImplemented surface)
/// Slice 40: textual heuristic — does the source declare or
/// reference any `#audit` automaton? If yes, the CLI prepends
/// the canonical `clifford::audit` module (interface + default
/// `ShadowSanitizer`) so the user doesn't have to copy-paste
/// the contract.
///
/// The check is intentionally simple: a substring match for
/// `#audit`. The only contexts in which `#audit` can appear in
/// well-formed Clifford source are:
///   - the `#audit` modifier on `#automaton`
///   - inside a comment / string literal (false positive — but
///     prepending the audit module in that case is harmless;
///     the prepended source compiles cleanly on its own).
///
/// Returns `false` for the vast majority of programs — the
/// auto-include is opt-in by way of the `#audit` keyword.
fn needs_audit_stdlib(source: &str) -> bool {
    // Bail out quickly if the user's source already declares
    // its own `PointerAuditor` interface — they're providing
    // their own; auto-prepending would conflict.
    if source.contains("#interface PointerAuditor") {
        return false;
    }
    source.contains("#audit")
}

fn compile_source(source: &str, module_name: &str) -> Result<String, CompileError> {
    // Slice 40: auto-include the `clifford::audit` module if the
    // user's source contains an `#audit` automaton. Detection is
    // textual: we look for the `#audit` keyword. Any false positive
    // (e.g. inside a string literal or comment) is harmless — the
    // prepended source is the canonical PointerAuditor interface
    // + ShadowSanitizer permissive default, which can sit unused.
    //
    // This is the v0.2 stop-gap pending a real module-import system.
    // Once `use clifford::audit::*;` lands, this auto-detection
    // becomes opt-out and eventually retired.
    //
    // We re-export the source-with-prepend as the active source for
    // the rest of the pipeline. The `module_name` reflects the user
    // intent; codegen sees the prepended audit module as part of
    // the same translation unit.
    let prepended;
    let active_source: &str = if needs_audit_stdlib(source) {
        prepended = clifford_stdlib::audit_module_source() + "\n" + source;
        &prepended
    } else {
        source
    };

    let tokens = tokenize(active_source).map_err(|e| CompileError::Phase {
        name: "lex",
        diags: PhaseDiag::from_single(&e),
    })?;
    let program = parse(&tokens).map_err(|e| CompileError::Phase {
        name: "parse",
        diags: PhaseDiag::from_single(&e),
    })?;
    let resolution = resolve(&program).map_err(|errs| CompileError::Phase {
        name: "resolve",
        diags: errs.iter().map(PhaseDiag::from_error).collect(),
    })?;
    let typing = infer(&program, &resolution).map_err(|errs| CompileError::Phase {
        name: "types",
        diags: errs.iter().map(PhaseDiag::from_error).collect(),
    })?;

    // Slice 15 — semantic gates between typing and codegen.

    // §5.5 sigil-layer + §5.4 mutation-auth + Decision #23 totality.
    check(&program, &resolution).map_err(|errs| CompileError::Phase {
        name: "check",
        diags: errs.iter().map(PhaseDiag::from_error).collect(),
    })?;

    // Decision #5 categorical structure of every #automaton.
    extract_categories(&program).map_err(|errs| CompileError::Phase {
        name: "effect",
        diags: errs.iter().map(PhaseDiag::from_error).collect(),
    })?;

    // Per-callable mutation profiles + #mutates / #cannot_mutate
    // validation per §6. Held alive across this scope so the
    // ortho verifier can consume them downstream.
    let profiles =
        extract_mutation_profiles(&program, &resolution).map_err(|errs| CompileError::Phase {
            name: "effect",
            diags: errs.iter().map(PhaseDiag::from_error).collect(),
        })?;

    // Proc-call graph cycle detection (catches mutual #> recursion).
    extract_call_graph(&program, &resolution).map_err(|errs| CompileError::Phase {
        name: "effect",
        diags: errs.iter().map(PhaseDiag::from_error).collect(),
    })?;

    // §7 GA orthogonality engine: write-write race detection across
    // every pair of concurrent callables via wedge-product check on
    // their behaviour blades. Decoder names shared fields by source
    // identifier per Emergent Rule 1 (no raw `e_n` indices).
    verify_ortho(&program, &profiles).map_err(|errs| CompileError::Phase {
        name: "ortho",
        diags: errs.iter().map(PhaseDiag::from_error).collect(),
    })?;

    lower(&program, &resolution, &typing, module_name).map_err(|errs| CompileError::Phase {
        name: "codegen",
        diags: errs.iter().map(PhaseDiag::from_error).collect(),
    })
}

/// Slice 16: render a `Phase` error to stderr using
/// `codespan-reporting`. Each diagnostic with a known byte offset
/// gets a labelled source-line snippet (line number + caret); those
/// without an offset render as a plain "error[phase]: message"
/// banner.
///
/// `file_name` is the displayed path (only used for the banner);
/// `source` is the actual source text passed to `codespan-reporting`
/// for line/column resolution.
fn render_phase_error(file_name: &str, source: &str, name: &str, diags: &[PhaseDiag]) {
    let file = SimpleFile::new(file_name.to_owned(), source.to_owned());
    let writer = StandardStream::stderr(ColorChoice::Auto);
    let config = term::Config::default();
    let mut writer_lock = writer.lock();

    for diag in diags {
        let mut diagnostic = Diagnostic::error()
            .with_code(format!("[{name}]"))
            .with_message(&diag.message);
        if let Some(off) = diag.primary_offset {
            // Default to a 1-byte span at the offset. Errors with
            // a more precise span (start..end) would render a wider
            // caret; today we don't have per-error spans plumbed
            // from the source crates so we default to point-spans.
            let end = (off + 1).min(source.len());
            let start = off.min(source.len());
            if start <= end {
                diagnostic = diagnostic.with_labels(vec![
                    Label::primary((), start..end).with_message("here"),
                ]);
            }
        }
        // Render via codespan-reporting. Failure to write to
        // stderr is non-fatal; we fall back to a plain eprintln.
        if let Err(_render_err) =
            term::emit(&mut writer_lock, &config, &file, &diagnostic)
        {
            eprintln!("error[{name}]: {}", diag.message);
        }
    }
}

/// Default output path: input file with its extension replaced by `.ll`.
/// If the input has no extension, append `.ll`.
fn default_output_path(input: &Path) -> PathBuf {
    let mut out = input.to_path_buf();
    if !out.set_extension("ll") {
        // `set_extension` returns false on paths with no file_name (e.g.
        // `/`). In that pathological case, fall back to `out.ll` in cwd.
        out = PathBuf::from("out.ll");
    }
    out
}

/// Default module name: the input file's stem (file name without
/// extension or parent directories). Falls back to `"module"` if the
/// path has no usable stem.
fn default_module_name(input: &Path) -> String {
    input
        .file_stem()
        .and_then(|s| s.to_str())
        .map(str::to_owned)
        .unwrap_or_else(|| "module".to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn argv(args: &[&str]) -> Vec<String> {
        args.iter().map(|s| (*s).to_owned()).collect()
    }

    #[test]
    fn empty_argv_prints_help() {
        assert_eq!(parse_argv(&argv(&[])), Cli::Help);
    }

    #[test]
    fn dash_h_and_long_help_are_help() {
        assert_eq!(parse_argv(&argv(&["-h"])), Cli::Help);
        assert_eq!(parse_argv(&argv(&["--help"])), Cli::Help);
    }

    #[test]
    fn version_flags() {
        assert_eq!(parse_argv(&argv(&["-V"])), Cli::Version);
        assert_eq!(parse_argv(&argv(&["--version"])), Cli::Version);
    }

    #[test]
    fn unknown_top_level_arg_is_unknown() {
        assert!(matches!(
            parse_argv(&argv(&["doctor"])),
            Cli::Unknown(_)
        ));
    }

    #[test]
    fn compile_minimum_args() {
        let cli = parse_argv(&argv(&["compile", "hello.cl"]));
        assert_eq!(
            cli,
            Cli::Compile {
                input: PathBuf::from("hello.cl"),
                output: None,
                module_name: None,
            }
        );
    }

    #[test]
    fn compile_with_output_flag() {
        let cli = parse_argv(&argv(&["compile", "hello.cl", "-o", "out.ll"]));
        assert_eq!(
            cli,
            Cli::Compile {
                input: PathBuf::from("hello.cl"),
                output: Some(PathBuf::from("out.ll")),
                module_name: None,
            }
        );
    }

    #[test]
    fn compile_with_module_name() {
        let cli = parse_argv(&argv(&[
            "compile",
            "hello.cl",
            "--module-name",
            "myMod",
        ]));
        assert_eq!(
            cli,
            Cli::Compile {
                input: PathBuf::from("hello.cl"),
                output: None,
                module_name: Some("myMod".to_owned()),
            }
        );
    }

    #[test]
    fn compile_with_output_before_input() {
        // `-o out.ll compile/foo.cl` — the order of args after
        // `compile` is flexible; the first non-flag token is the
        // input.
        let cli = parse_argv(&argv(&["compile", "-o", "out.ll", "hello.cl"]));
        assert_eq!(
            cli,
            Cli::Compile {
                input: PathBuf::from("hello.cl"),
                output: Some(PathBuf::from("out.ll")),
                module_name: None,
            }
        );
    }

    #[test]
    fn compile_missing_input_is_unknown() {
        assert!(matches!(
            parse_argv(&argv(&["compile"])),
            Cli::Unknown(_)
        ));
    }

    #[test]
    fn compile_missing_output_value_is_unknown() {
        assert!(matches!(
            parse_argv(&argv(&["compile", "hello.cl", "-o"])),
            Cli::Unknown(_)
        ));
    }

    #[test]
    fn compile_unrecognised_flag_is_unknown() {
        assert!(matches!(
            parse_argv(&argv(&["compile", "hello.cl", "--target", "thumbv7em"])),
            Cli::Unknown(_)
        ));
    }

    #[test]
    fn default_output_path_swaps_extension() {
        assert_eq!(
            default_output_path(Path::new("foo.cl")),
            PathBuf::from("foo.ll")
        );
        // No extension: appends `.ll`.
        assert_eq!(
            default_output_path(Path::new("foo")),
            PathBuf::from("foo.ll")
        );
        // Subdir paths preserve the directory.
        assert_eq!(
            default_output_path(Path::new("src/foo.cl")),
            PathBuf::from("src/foo.ll")
        );
    }

    #[test]
    fn default_module_name_uses_stem() {
        assert_eq!(default_module_name(Path::new("hello.cl")), "hello");
        assert_eq!(
            default_module_name(Path::new("path/to/uart.cl")),
            "uart"
        );
        assert_eq!(default_module_name(Path::new("noext")), "noext");
    }

    #[test]
    fn compile_source_lowers_minimal_program() {
        // Smoke: an empty program produces a valid IR header.
        let ir = compile_source("", "smoke").expect("empty program lowers");
        assert!(
            ir.contains("ModuleID = 'smoke'"),
            "expected module ID; got:\n{ir}"
        );
    }

    #[test]
    fn compile_source_lowers_real_firmware_shape() {
        // End-to-end smoke on a small multi-state automaton.
        let src = "\
            #automaton Counter {\n  \
              #states: [Idle, Counting];\n  \
              count: u32;\n  \
              #transition start -> Counting { Counter.count = 0u32; }\n\
            }\n\
            #effect bump() #mutates: [Counter] { Counter.count += 1u32; }\n\
        ";
        let ir = compile_source(src, "fw").expect("firmware program lowers");
        assert!(
            ir.contains("%struct.Counter = type { i32, i32 }"),
            "expected multi-state struct; got:\n{ir}"
        );
        assert!(
            ir.contains("define void @Counter_start()"),
            "expected transition fn; got:\n{ir}"
        );
        assert!(
            ir.contains("define void @bump()"),
            "expected effect fn; got:\n{ir}"
        );
    }

    #[test]
    fn compile_source_surfaces_parse_error_with_prefix() {
        // Garbled source produces a parse error.
        let err = compile_source("@fn ;", "test").expect_err("expected parse error");
        match err {
            CompileError::Phase { name, diags } => {
                assert_eq!(name, "parse", "expected parse phase; got {name}");
                assert!(!diags.is_empty(), "expected at least one diagnostic");
                assert!(
                    diags[0].message.starts_with("E02"),
                    "expected E02xx parse error code; got {:?}",
                    diags[0].message
                );
            }
            CompileError::Io(_) => panic!("unexpected I/O error"),
        }
    }

    #[test]
    fn compile_source_surfaces_check_phase_error() {
        // Slice 15: sigil-layer violation — calling `#> proc()` from
        // inside an `@fn` body. Some upstream gate (resolve, check,
        // or effect) rejects this; verifies one of them does.
        let src = "\
            #automaton C { v: u32; }\n\
            #effect bump() #mutates: [C] { return; }\n\
            @fn caller() { #> bump(); return; }\n\
        ";
        let err = compile_source(src, "test").expect_err("expected gate error");
        match err {
            CompileError::Phase { name, .. } => {
                assert!(
                    matches!(name, "resolve" | "check" | "effect"),
                    "expected resolve/check/effect; got {name}"
                );
            }
            CompileError::Io(_) => panic!("unexpected I/O error"),
        }
    }

    #[test]
    fn compile_source_surfaces_effect_phase_error_for_undeclared_mutates() {
        // Slice 15: effect mutates Counter without declaring it.
        let src = "\
            #automaton Counter { v: u32; }\n\
            #effect bump() #mutates: [] { Counter.v += 1u32; }\n\
        ";
        let err = compile_source(src, "test").expect_err("expected gate error");
        match err {
            CompileError::Phase { name, .. } => {
                assert!(
                    matches!(name, "resolve" | "check" | "effect"),
                    "expected resolve/check/effect; got {name}"
                );
            }
            CompileError::Io(_) => panic!("unexpected I/O error"),
        }
    }

    #[test]
    fn compile_source_passes_all_gates_for_real_firmware() {
        // Slice 15: positive integration — a realistic firmware
        // shape passes lex/parse/resolve/types/check/effect and
        // produces IR. This is the canonical "the gates don't
        // break the v0.1 firmware path" check.
        let src = "\
            #automaton Counter {\n  \
              #states: [Idle, Counting];\n  \
              count: u32;\n  \
              #transition start -> Counting { Counter.count = 0u32; }\n\
            }\n\
            #effect bump() #mutates: [Counter] { Counter.count += 1u32; }\n\
            #effect peek() -> u32 #mutates: [Counter] {\n  \
              if Counter.count > 100u32 { return 100u32; }\n  \
              return Counter.count;\n\
            }\n\
        ";
        let ir = compile_source(src, "fw").expect("real firmware passes all gates");
        assert!(
            ir.contains("define void @bump()"),
            "expected bump fn; got:\n{ir}"
        );
        assert!(
            ir.contains("define i32 @peek()"),
            "expected peek fn; got:\n{ir}"
        );
        assert!(
            ir.contains("br i1"),
            "expected if-conditional branch; got:\n{ir}"
        );
    }

    // ─── Slice 16: source-line diagnostics ────────────────────────────

    #[test]
    fn byte_offset_from_msg_extracts_basic_form() {
        // Most clifford-* errors include "at byte N" verbatim.
        assert_eq!(
            byte_offset_from_msg("E0204: expected `;`, found Eq at byte 42"),
            Some(42)
        );
    }

    #[test]
    fn byte_offset_from_msg_handles_parenthesized_form() {
        // "(at byte N)" is the parenthesised variant some errors use.
        assert_eq!(
            byte_offset_from_msg("E0101: imperative construct in @fn (at byte 7)"),
            Some(7)
        );
    }

    #[test]
    fn byte_offset_from_msg_handles_referenced_form() {
        // Resolver errors say "referenced at byte N".
        assert_eq!(
            byte_offset_from_msg("E0405: `Counter` has no field `bogus` (referenced at byte 123)"),
            Some(123)
        );
    }

    #[test]
    fn byte_offset_from_msg_returns_first_offset_when_multiple() {
        // E0401 mentions "at byte X; first declared at byte Y" —
        // we pick X (the current location, not the historical one).
        assert_eq!(
            byte_offset_from_msg(
                "E0401: duplicate item `foo` at byte 50; first declared at byte 10"
            ),
            Some(50)
        );
    }

    #[test]
    fn byte_offset_from_msg_returns_none_for_no_offset() {
        assert_eq!(byte_offset_from_msg("E0500: GA engine not yet implemented"), None);
        assert_eq!(byte_offset_from_msg("just a plain message"), None);
        assert_eq!(byte_offset_from_msg(""), None);
    }

    #[test]
    fn byte_offset_from_msg_skips_byte_without_digits() {
        // The literal word "byte" in a non-offset context shouldn't
        // confuse the helper. (e.g. "wrote byte data" is rare in
        // clifford errors but defensive.)
        assert_eq!(
            byte_offset_from_msg("the byte was bad and at byte 99 we found it"),
            Some(99)
        );
    }

    #[test]
    fn phase_diag_from_error_extracts_offset_when_present() {
        struct E;
        impl std::fmt::Display for E {
            fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                write!(f, "E0204: expected `;` at byte 42")
            }
        }
        let d = PhaseDiag::from_error(&E);
        assert_eq!(d.primary_offset, Some(42));
        assert!(d.message.contains("E0204"));
    }

    #[test]
    fn phase_diag_from_error_offset_none_when_absent() {
        struct E;
        impl std::fmt::Display for E {
            fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                write!(f, "E0500: GA engine not yet implemented")
            }
        }
        let d = PhaseDiag::from_error(&E);
        assert_eq!(d.primary_offset, None);
    }

    #[test]
    fn non_firmware_example_crc32_compiles_cleanly() {
        // Per CLAUDE.md §10 v0.1 criteria: "Also: a non-firmware
        // example (e.g., a small CLI tool or a numerical kernel)
        // to demonstrate the language is not embedded-only."
        // examples/crc32.cl is that example — pure-functional,
        // zero `#`-layer constructs, links with any host C harness.
        // This test just asserts cliffordc accepts the file and
        // produces the expected entry points.
        let src = std::fs::read_to_string("../../examples/crc32.cl")
            .expect("read examples/crc32.cl");
        let ir = compile_source(&src, "crc32").expect("crc32 compiles");
        for needle in [
            "define i32 @crc32_init()",
            "define i32 @crc32_byte(i32 %crc, i8 %byte)",
            "define i32 @crc32_finalize(i32 %crc)",
            "define i32 @crc32_test_vector()",
        ] {
            assert!(
                ir.contains(needle),
                "missing entry point `{needle}` in IR; got:\n{ir}"
            );
        }
        // Sanity: zero `#`-layer artefacts in the IR (no
        // %struct.<Auto>, no @<Auto>.state, no `section ".interrupts"`).
        assert!(
            !ir.contains("%struct."),
            "non-firmware example should not emit automaton structs; got:\n{ir}"
        );
        assert!(
            !ir.contains(".state ="),
            "non-firmware example should not emit automaton globals; got:\n{ir}"
        );
        assert!(
            !ir.contains("section \".interrupts\""),
            "non-firmware example should not emit interrupt section; got:\n{ir}"
        );
    }

    #[test]
    fn compile_source_phase_error_carries_offsets_for_renderable_diagnostics() {
        // A real parse error carries an "at byte N" offset that the
        // CLI extracts for codespan-rendering. Verifies the
        // extraction works on actual upstream errors, not just
        // synthetic Display impls.
        let err = compile_source("@fn ;", "test").expect_err("expected parse error");
        match err {
            CompileError::Phase { diags, .. } => {
                assert!(!diags.is_empty(), "expected diagnostics");
                // Parse errors all carry "at byte N"; the offset
                // should be Some for at least the first one.
                assert!(
                    diags[0].primary_offset.is_some(),
                    "expected primary_offset on parse error; got {:?}",
                    diags[0]
                );
            }
            CompileError::Io(_) => panic!("unexpected I/O error"),
        }
    }

    // ─── Slice 34: integration — every example .cl compiles cleanly ─────

    /// Locate the workspace root by walking up from this source
    /// file until we find `examples/`. Returns the absolute path
    /// to `examples/` or panics if not found within 10 levels.
    fn examples_dir() -> std::path::PathBuf {
        let mut p = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"));
        for _ in 0..10 {
            let candidate = p.join("examples");
            if candidate.is_dir() {
                return candidate;
            }
            if !p.pop() {
                break;
            }
        }
        panic!("could not locate examples/ relative to CARGO_MANIFEST_DIR");
    }

    /// Read every `.cl` file in `examples/` and invoke
    /// `compile_source` on it. The full pipeline (lex → parse →
    /// resolve → types → check → effect → ortho → codegen) runs
    /// per file; if any phase emits an error the test fails with
    /// the file's name and the diagnostic. Locks in the v0.2
    /// "every shipped example compiles cleanly" invariant.
    #[test]
    fn s34_every_example_cl_file_compiles_cleanly() {
        let dir = examples_dir();
        let mut entries: Vec<std::path::PathBuf> = std::fs::read_dir(&dir)
            .expect("read examples/")
            .filter_map(|e| e.ok())
            .map(|e| e.path())
            .filter(|p| p.extension().and_then(|s| s.to_str()) == Some("cl"))
            .collect();
        entries.sort();
        assert!(
            !entries.is_empty(),
            "no .cl files found in {}",
            dir.display()
        );
        for path in &entries {
            let src = std::fs::read_to_string(path)
                .unwrap_or_else(|e| panic!("read {}: {e}", path.display()));
            let module = path
                .file_stem()
                .and_then(|s| s.to_str())
                .unwrap_or("example");
            let res = compile_source(&src, module);
            if let Err(e) = res {
                panic!(
                    "example {} failed to compile: {e:?}",
                    path.display()
                );
            }
        }
    }

    // ─── Slice 40: auto-include `clifford::audit` for #audit programs ───

    #[test]
    fn s40_audit_program_compiles_without_user_supplied_interface() {
        // The user's source declares an `#audit` register-block
        // automaton + an effect that pokes it. The user does NOT
        // supply `#interface PointerAuditor`. Slice 40's
        // auto-include prepends the canonical interface +
        // ShadowSanitizer so this still compiles cleanly.
        let src = "\
            #audit #automaton Uart {\n  \
              #address: 0x4000_4000;\n  \
              tx_data: u32 #offset: 0x00;\n\
            }\n\
            #effect send(b: u32) #mutates: [Uart] { Uart.tx_data = b; return; }\n\
        ";
        let ir = compile_source(src, "fw").expect("audit-using program compiles");
        // Both stdlib and user code present.
        assert!(
            ir.contains("%struct.ShadowSanitizer"),
            "expected ShadowSanitizer struct from auto-included stdlib; got:\n{ir}"
        );
        assert!(
            ir.contains("define void @send"),
            "expected user effect; got:\n{ir}"
        );
        // The slice-23 audit marker fires for the Uart write.
        assert!(
            ir.contains("audit-wrap site for Uart"),
            "expected audit marker for Uart; got:\n{ir}"
        );
    }

    #[test]
    fn s40_non_audit_program_does_not_trigger_auto_include() {
        // Mirror of the above without `#audit` — verifies the
        // auto-include is opt-in. The IR must NOT contain any
        // ShadowSanitizer / PointerAuditor artefacts.
        let src = "\
            #automaton Uart {\n  \
              #address: 0x4000_4000;\n  \
              tx_data: u32 #offset: 0x00;\n\
            }\n\
            #effect send(b: u32) #mutates: [Uart] { Uart.tx_data = b; return; }\n\
        ";
        let ir = compile_source(src, "fw").expect("non-audit compiles");
        assert!(
            !ir.contains("ShadowSanitizer"),
            "non-audit program must not get ShadowSanitizer; got:\n{ir}"
        );
        assert!(
            !ir.contains("PointerAuditor"),
            "non-audit program must not get PointerAuditor; got:\n{ir}"
        );
    }

    #[test]
    fn s40_user_supplied_pointer_auditor_suppresses_auto_include() {
        // If the user declares their own `#interface
        // PointerAuditor { … }`, the auto-include must back off
        // to avoid a duplicate-symbol conflict. The user's
        // interface is the one used.
        let src = "\
            #interface PointerAuditor { effect record_alloc(p: access<u8>, n: u32); }\n\
            #automaton MySanitizer { }\n\
            #impl PointerAuditor for MySanitizer { effect record_alloc(p: access<u8>, n: u32) { return; } }\n\
            #audit #automaton Buf { v: u32; }\n\
        ";
        // Should compile (user supplies a single-method
        // interface so the impl matches their version, not
        // ours).
        let res = compile_source(src, "fw");
        assert!(
            res.is_ok(),
            "user-supplied PointerAuditor should compile cleanly; got {res:?}"
        );
    }

    #[test]
    fn s40_needs_audit_stdlib_textual_heuristic() {
        // Direct unit test on the heuristic.
        assert!(needs_audit_stdlib("#audit #automaton X { }"));
        assert!(needs_audit_stdlib("// some comment\n#audit #automaton X { }"));
        assert!(!needs_audit_stdlib("#automaton X { }"));
        assert!(!needs_audit_stdlib(""));
        // User-supplied PointerAuditor suppresses auto-include.
        assert!(!needs_audit_stdlib(
            "#interface PointerAuditor { } #audit #automaton X { }"
        ));
    }
}
