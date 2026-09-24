//! The `bartelang` command line interface.

use std::process::ExitCode;

use bartelang::error::render_syntax_error;
use bartelang::{Interp, Parser, emit_line, emit_prompt, load};

const USAGE: &str = "\
bartelang - a 1998 Visual Basic 6 dialect running on a Unix stream engine

USAGE:
    bartelang run <file.btm> [args...]    Parse and execute a script
    bartelang parse <file.btm>            Dump the parsed AST
    bartelang version                     Print the version
    bartelang help                        Show this message

EXAMPLES:
    bartelang run examples/sys_fetch.btm Delft
    bartelang parse examples/tour.btm
";

fn main() -> ExitCode {
    restore_default_sigpipe();

    let args: Vec<String> = std::env::args().skip(1).collect();

    // Run on a thread with a generous stack: Bartelang scripts are allowed to
    // recurse, and the interpreter should report a clean error rather than
    // aborting the process.
    let worker = std::thread::Builder::new()
        .name("bartelang-interpreter".to_string())
        .stack_size(64 * 1024 * 1024)
        .spawn(move || dispatch(args));

    match worker {
        Ok(handle) => match handle.join() {
            Ok(code) => code,
            Err(_) => {
                eprintln!("bartelang: interpreter thread panicked");
                ExitCode::from(70)
            }
        },
        Err(e) => {
            eprintln!("bartelang: could not start interpreter thread: {e}");
            ExitCode::from(70)
        }
    }
}

/// Rust ignores `SIGPIPE` at startup, which turns `bartelang run x.btm | head`
/// into an aborting process instead of a quietly exiting one.  Bartelang is
/// meant to be a first-class Unix citizen, so put the normal behaviour back.
#[cfg(unix)]
fn restore_default_sigpipe() {
    unsafe {
        libc::signal(libc::SIGPIPE, libc::SIG_DFL);
    }
}

#[cfg(not(unix))]
fn restore_default_sigpipe() {}

fn dispatch(args: Vec<String>) -> ExitCode {
    let Some(command) = args.first().map(String::as_str) else {
        emit_prompt(USAGE);
        return ExitCode::from(2);
    };

    match command {
        "run" => run_script(&args[1..]),
        "parse" => parse_script(&args[1..]),
        "help" | "--help" | "-h" => {
            emit_prompt(USAGE);
            ExitCode::SUCCESS
        }
        "version" | "--version" | "-V" => {
            emit_line(&format!("bartelang {}", env!("CARGO_PKG_VERSION")));
            ExitCode::SUCCESS
        }
        other => {
            // Convenience: `bartelang script.btm` behaves like `run`.
            if other.ends_with(".btm") || std::path::Path::new(other).is_file() {
                run_script(&args)
            } else {
                eprintln!("bartelang: unknown command '{other}'\n");
                emit_prompt(USAGE);
                ExitCode::from(2)
            }
        }
    }
}

fn read_source(path: &str) -> Result<String, ExitCode> {
    std::fs::read_to_string(path).map_err(|e| {
        eprintln!("bartelang: cannot read {path}: {e}");
        ExitCode::from(2)
    })
}

fn split_source(source: &str) -> Vec<String> {
    source
        .split('\n')
        .map(|line| line.strip_suffix('\r').unwrap_or(line).to_string())
        .collect()
}

fn run_script(argv: &[String]) -> ExitCode {
    let Some(path) = argv.first() else {
        eprintln!("bartelang run: missing script path\n");
        emit_prompt(USAGE);
        return ExitCode::from(2);
    };
    let source = match read_source(path) {
        Ok(source) => source,
        Err(code) => return code,
    };

    let loaded = match load(&source) {
        Ok(loaded) => loaded,
        Err(error) => {
            eprintln!(
                "bartelang: {}",
                render_syntax_error(&error, &split_source(&source))
            );
            return ExitCode::from(1);
        }
    };

    let mut interp = Interp::new(argv[1..].to_vec());
    match interp.run(&loaded.program) {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("bartelang: {}", error.render(&loaded.source_lines));
            ExitCode::from(1)
        }
    }
}

fn parse_script(argv: &[String]) -> ExitCode {
    let Some(path) = argv.first() else {
        eprintln!("bartelang parse: missing script path\n");
        emit_prompt(USAGE);
        return ExitCode::from(2);
    };
    let source = match read_source(path) {
        Ok(source) => source,
        Err(code) => return code,
    };

    match Parser::parse_source(&source) {
        Ok((program, _)) => {
            emit_line(&format!("{program:#?}"));
            ExitCode::SUCCESS
        }
        Err(error) => {
            eprintln!(
                "bartelang: {}",
                render_syntax_error(&error, &split_source(&source))
            );
            ExitCode::from(1)
        }
    }
}
