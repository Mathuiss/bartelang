//! Bartelang - a 1998 Visual Basic 6 dialect with a Unix stream engine
//! underneath.
//!
//! Pipeline: source text -> [`lexer::tokenize`] -> [`parser::Parser`] ->
//! [`interp::Interp`].
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
pub mod objects;
pub mod parser;
mod random;
pub mod record;
mod records;
pub mod value;

pub use error::{BrtError, SyntaxError};
pub use interp::Interp;
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

/// A parsed, ready-to-run program together with its source lines (kept so that
/// errors can quote the offending line).
pub struct Loaded {
    /// The parsed program, ready for [`Interp::run`](crate::Interp::run).
    pub program: ast::Program,
    /// The source split into lines, so an error can quote the offending one.
    pub source_lines: Vec<String>,
}

/// Parses Bartelang source without running it.
pub fn load(source: &str) -> Result<Loaded, SyntaxError> {
    let (program, source_lines) = Parser::parse_source(source)?;
    Ok(Loaded {
        program,
        source_lines,
    })
}

/// Parses and runs Bartelang source, returning the interpreter's final state.
pub fn execute(
    source: &str,
    args: Vec<String>,
) -> Result<Interp, Box<dyn std::error::Error>> {
    let loaded = load(source)?;
    let mut interp = Interp::new(args);
    interp.run(&loaded.program)?;
    Ok(interp)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn load_parses_a_program_and_keeps_the_source_lines() {
        let loaded = match load("Dim x\nLet x = 1\n") {
            Ok(loaded) => loaded,
            Err(error) => panic!("expected a clean parse: {error}"),
        };
        assert_eq!(loaded.program.statements.len(), 2);
        assert_eq!(loaded.source_lines.len(), 3);
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
