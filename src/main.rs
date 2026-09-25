//! The `bartelang` command line interface.

use std::path::PathBuf;
use std::process::ExitCode;

use bartelang::loader::{LoadError, LoadErrorKind, Loader};
use bartelang::{Interp, emit_line, emit_prompt};

const USAGE: &str = "\
bartelang - a 1998 Visual Basic 6 dialect running on a Unix stream engine

USAGE:
    bartelang run [-I <dir>]... <file.btm> [args...]   Parse and execute a script
    bartelang parse [-I <dir>]... <file.btm>           Dump the parsed AST
    bartelang version                                  Print the version
    bartelang help                                     Show this message

INCLUDES:
    Include \"path.btm\" resolves relative to the including file first, then the
    -I directories in order, then $BARTELANG_PATH (colon-separated).

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

/// Pulls leading `-I <dir>` (or `-I<dir>`) options off an argument list,
/// leaving the script path and the script's own arguments untouched.
fn take_include_dirs(argv: &[String]) -> Result<(Vec<PathBuf>, &[String]), String> {
    let mut dirs = Vec::new();
    let mut index = 0;
    while index < argv.len() {
        let arg = &argv[index];
        if arg == "-I" || arg == "--include-dir" {
            let Some(dir) = argv.get(index + 1) else {
                return Err(format!("{arg} needs a directory"));
            };
            dirs.push(PathBuf::from(dir));
            index += 2;
        } else if let Some(dir) = arg.strip_prefix("-I") {
            dirs.push(PathBuf::from(dir));
            index += 1;
        } else {
            break;
        }
    }
    Ok((dirs, &argv[index..]))
}

/// The CLI's loader: `-I` directories first, then `$BARTELANG_PATH` entries.
fn loader_with(dirs: Vec<PathBuf>) -> Loader {
    let mut loader = Loader::new();
    for dir in dirs {
        loader = loader.include_dir(dir);
    }
    if let Ok(path) = std::env::var("BARTELANG_PATH") {
        for dir in path.split(':').filter(|dir| !dir.is_empty()) {
            loader = loader.include_dir(dir);
        }
    }
    loader
}

/// Prints a load failure and picks the exit code: an entry file that cannot be
/// read is a command-line problem (2), anything the source itself is to blame
/// for is 1.
fn report_load_error(error: &LoadError) -> ExitCode {
    match error.kind() {
        LoadErrorKind::Unreadable { .. } => {
            eprintln!("bartelang: {error}");
            ExitCode::from(2)
        }
        _ => {
            eprintln!("bartelang: {}", error.render());
            ExitCode::from(1)
        }
    }
}

fn run_script(argv: &[String]) -> ExitCode {
    let (include_dirs, argv) = match take_include_dirs(argv) {
        Ok(parts) => parts,
        Err(message) => {
            eprintln!("bartelang run: {message}\n");
            emit_prompt(USAGE);
            return ExitCode::from(2);
        }
    };
    let Some(path) = argv.first() else {
        eprintln!("bartelang run: missing script path\n");
        emit_prompt(USAGE);
        return ExitCode::from(2);
    };
    let loaded = match loader_with(include_dirs).load_file(path) {
        Ok(loaded) => loaded,
        Err(error) => return report_load_error(&error),
    };

    let mut interp = Interp::new(argv[1..].to_vec());
    match interp.run_loaded(&loaded) {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("bartelang: {}", error.render(&loaded.sources));
            ExitCode::from(1)
        }
    }
}

fn parse_script(argv: &[String]) -> ExitCode {
    let (include_dirs, argv) = match take_include_dirs(argv) {
        Ok(parts) => parts,
        Err(message) => {
            eprintln!("bartelang parse: {message}\n");
            emit_prompt(USAGE);
            return ExitCode::from(2);
        }
    };
    let Some(path) = argv.first() else {
        eprintln!("bartelang parse: missing script path\n");
        emit_prompt(USAGE);
        return ExitCode::from(2);
    };

    match loader_with(include_dirs).load_file(path) {
        Ok(loaded) => {
            emit_line(&format!("{:#?}", loaded.program));
            ExitCode::SUCCESS
        }
        Err(error) => report_load_error(&error),
    }
}
