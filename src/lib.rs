//! Bartelang - a 1998 Visual Basic 6 dialect with a Unix stream engine
//! underneath.
//!
//! Pipeline: source text -> [`lexer::tokenize`] -> [`parser::Parser`] ->
//! [`loader::Loader`] (which resolves `Include` and keeps every file for
//! diagnostics) -> [`interp::Interp`].
//!
//! The crate documentation *is* the README, so the language reference and the
//! code cannot drift apart: any Rust example in it is compiled by `cargo test`.
#![doc = include_str!("../README.md")]

pub mod ast;
pub mod builtins;
mod datetime;
pub mod error;
mod format;
pub mod interp;
pub mod lexer;
pub mod loader;
pub mod objects;
pub mod parser;
mod random;
pub mod record;
mod records;
pub mod value;

pub use error::{BrtError, SyntaxError};
pub use interp::Interp;
pub use loader::{LoadError, Loader, SourceMap};
pub use parser::Parser;
pub use value::Variant;

/// Writes one line to stdout, tolerating a closed or broken stream.
///
/// `println!` panics on `EPIPE`, which would turn `bartelang run x.btm | head`
/// into an interpreter crash rather than a quiet Unix exit.
pub fn emit_line(text: &str) {
    use std::io::Write;
    let stdout = std::io::stdout();
    let mut lock = stdout.lock();
    let _ = writeln!(lock, "{text}");
    let _ = lock.flush();
}

/// Writes a prompt with no trailing newline and flushes, for `InputBox`.
pub fn emit_prompt(text: &str) {
    use std::io::Write;
    let stdout = std::io::stdout();
    let mut lock = stdout.lock();
    let _ = write!(lock, "{text}");
    let _ = lock.flush();
}

/// A parsed, ready-to-run program together with every source file it came
/// from, so that errors can name the file and quote the offending line.
#[derive(Debug)]
pub struct Loaded {
    /// The parsed program, ready for [`Interp::run_loaded`](crate::Interp::run_loaded).
    /// `Include` statements have already been resolved and expanded.
    pub program: ast::Program,
    /// The source units, indexed by the unit ids statements are tagged with.
    pub sources: SourceMap,
}

/// Parses Bartelang source without running it.
///
/// The string carries no file context, so a program that contains `Include`
/// cannot be resolved this way: use [`load_file`] or [`Loader::load_source`],
/// which know where the file is and can read what it includes.
pub fn load(source: &str) -> Result<Loaded, SyntaxError> {
    let (program, _) = Parser::parse_source(source)?;
    if let Some(statement) = program
        .statements
        .iter()
        .find(|statement| matches!(ast::peel(statement), ast::Stmt::Include { .. }))
    {
        let line = match statement {
            ast::Stmt::Located { line, .. } => *line,
            _ => 1,
        };
        return Err(SyntaxError::new(
            "Include needs a file to resolve against; load the program from a path \
             with load_file, or through a Loader",
            line,
            1,
        ));
    }
    Ok(Loaded {
        program,
        sources: SourceMap::from_source(source),
    })
}

/// Loads a program from a file, resolving every `Include` it contains.
///
/// Includes resolve relative to the including file's directory first, then any
/// directories given with [`Loader::include_dir`].
///
/// ```no_run
/// let loaded = bartelang::load_file("report.btm")?;
/// let mut interp = bartelang::Interp::new(vec![]);
/// interp.run_loaded(&loaded)?;
/// # Ok::<(), Box<dyn std::error::Error>>(())
/// ```
pub fn load_file(path: impl AsRef<std::path::Path>) -> Result<Loaded, LoadError> {
    Loader::new().load_file(path)
}

/// Parses and runs Bartelang source, returning the interpreter's final state.
pub fn execute(source: &str, args: Vec<String>) -> Result<Interp, Box<dyn std::error::Error>> {
    let loaded = load(source)?;
    let mut interp = Interp::new(args);
    interp.run_loaded(&loaded)?;
    Ok(interp)
}

/// Loads and runs a program from a file, resolving its `Include` statements.
pub fn execute_file(
    path: impl AsRef<std::path::Path>,
    args: Vec<String>,
) -> Result<Interp, Box<dyn std::error::Error>> {
    let loaded = load_file(path)?;
    let mut interp = Interp::new(args);
    interp.run_loaded(&loaded)?;
    Ok(interp)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn load_parses_a_program_and_keeps_the_source_map() {
        let loaded = match load("Dim x\nLet x = 1\n") {
            Ok(loaded) => loaded,
            Err(error) => panic!("expected a clean parse: {error}"),
        };
        assert_eq!(loaded.program.statements.len(), 2);
        assert_eq!(loaded.sources.len(), 1);
        assert_eq!(loaded.sources.line(0, 3), Some(""));
    }

    #[test]
    fn a_string_source_cannot_resolve_an_include() {
        match load("Include \"lib.btm\"\n") {
            Ok(_) => panic!("expected the include to be rejected"),
            Err(error) => {
                assert_eq!(error.line, 1);
                assert!(error.message.contains("load_file"), "{error}");
            }
        }
    }

    #[test]
    fn load_reports_syntax_errors_instead_of_panicking() {
        match load("If x\n") {
            Ok(_) => panic!("expected a syntax error"),
            Err(error) => {
                assert_eq!(error.line, 1);
                assert!(error.message.contains("then"), "{error}");
            }
        }
    }

    #[test]
    fn execute_runs_a_program_and_exposes_its_final_state() {
        let interp = match execute("Dim code\nLet code = `exit 4`\n", Vec::new()) {
            Ok(interp) => interp,
            Err(error) => panic!("expected the script to run: {error}"),
        };
        assert_eq!(interp.last_exit_code(), 4);
    }

    #[test]
    fn execute_surfaces_runtime_errors() {
        match execute("Let boom = 1 / 0\n", Vec::new()) {
            Ok(_) => panic!("expected a runtime error"),
            Err(error) => assert_eq!(error.to_string(), "error 11: Division by zero"),
        }
    }
}
