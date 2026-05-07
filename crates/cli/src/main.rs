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

use clifford_codegen::lower;
use clifford_lexer::tokenize;
use clifford_parser::parse;
use clifford_resolve::resolve;
use clifford_types::infer;

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
            Err(CompileError::Phase(msg)) => {
                eprintln!("{msg}");
                ExitCode::from(1)
            }
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

/// Outcome of `run_compile`. `Phase` carries pre-formatted text for stderr;
/// `Io` carries a system-level error.
#[derive(Debug)]
enum CompileError {
    Phase(String),
    Io(String),
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

    let ir = compile_source(&source, &module_name_owned)?;

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

/// Run the lex → parse → resolve → types → codegen pipeline against a
/// source string. Errors are pre-formatted with phase prefixes so the
/// caller can print them verbatim.
fn compile_source(source: &str, module_name: &str) -> Result<String, CompileError> {
    let tokens = tokenize(source)
        .map_err(|e| CompileError::Phase(format!("error[lex]: {e}")))?;
    let program = parse(&tokens)
        .map_err(|e| CompileError::Phase(format!("error[parse]: {e}")))?;
    let resolution = resolve(&program).map_err(|errs| {
        CompileError::Phase(format_phase_errors("resolve", &errs))
    })?;
    let typing = infer(&program, &resolution).map_err(|errs| {
        CompileError::Phase(format_phase_errors("types", &errs))
    })?;
    lower(&program, &resolution, &typing, module_name).map_err(|errs| {
        CompileError::Phase(format_phase_errors("codegen", &errs))
    })
}

/// Format a vector of errors from a single phase as one block of stderr
/// text, one error per line.
fn format_phase_errors<E: std::fmt::Display>(phase: &str, errors: &[E]) -> String {
    let mut out = String::new();
    for (i, e) in errors.iter().enumerate() {
        if i > 0 {
            out.push('\n');
        }
        out.push_str(&format!("error[{phase}]: {e}"));
    }
    out
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
            CompileError::Phase(msg) => {
                assert!(
                    msg.starts_with("error[parse]:"),
                    "expected parse-prefixed error; got: {msg}"
                );
            }
            CompileError::Io(_) => panic!("unexpected I/O error"),
        }
    }

    #[test]
    fn format_phase_errors_joins_multiple_with_newlines() {
        let errs = vec!["first", "second", "third"];
        let s = format_phase_errors("xyz", &errs);
        assert!(s.contains("error[xyz]: first"));
        assert!(s.contains("error[xyz]: second"));
        assert!(s.contains("error[xyz]: third"));
        // Three errors separated by two newlines.
        assert_eq!(s.matches("error[xyz]:").count(), 3);
    }
}
