//! The `bartelang` command line interface.

use std::path::PathBuf;
use std::process::ExitCode;

use clap::{Arg, ArgAction, ArgMatches, Command};

use bartelang::loader::{LoadError, LoadErrorKind, Loader};
use bartelang::{Interp, emit_line};

/// What clap cannot infer: where includes are looked for, and the two ways to
/// run a script.  Kept in the help because those are the questions a new user
/// actually has.
const AFTER_HELP: &str = "\
INCLUDES:
    Include \"path.btm\" resolves relative to the including file first, then the
    -I directories in order, then $BARTELANG_PATH (colon-separated).

EXAMPLES:
    bartelang run examples/sys_fetch.btm Delft
    bartelang parse examples/tour.btm
    bartelang examples/tour.btm          shorthand for `run`";

/// The CLI, as a clap command.  Built by hand rather than with `derive` so the
/// shorthand below stays explicit and no proc-macro dependency is needed.
fn cli() -> Command {
    Command::new("bartelang")
        .version(env!("CARGO_PKG_VERSION"))
        .about("A 1998 Visual Basic 6 dialect running on a Unix stream engine")
        .subcommand_required(true)
        .after_help(AFTER_HELP)
        .subcommand(
            Command::new("run")
                .about("Parse and execute a script")
                .arg(include_dirs())
                .arg(Arg::new("script").value_name("file.btm").required(true))
                .arg(
                    Arg::new("args")
                        .value_name("args")
                        .num_args(0..)
                        .allow_hyphen_values(true)
                        .help("Arguments for the script, populating ARGS"),
                )
                .trailing_var_arg(true),
        )
        .subcommand(
            Command::new("parse")
                .about("Dump the parsed AST")
                .arg(include_dirs())
                .arg(Arg::new("script").value_name("file.btm").required(true)),
        )
        .subcommand(Command::new("version").about("Print the version"))
}

fn include_dirs() -> Arg {
    Arg::new("include_dirs")
        .short('I')
        .long("include-dir")
        .value_name("dir")
        .action(ArgAction::Append)
        .help("Add a directory to the include search path (repeatable)")
}

fn main() -> ExitCode {
    restore_default_sigpipe();

    let args = normalize_shorthand(std::env::args().skip(1).collect());

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

/// `bartelang script.btm` is shorthand for `bartelang run script.btm` - the
/// natural way to type a one-off script.  Anything that names a command or a
/// flag is left for clap to interpret.
fn normalize_shorthand(mut args: Vec<String>) -> Vec<String> {
    const COMMANDS: [&str; 8] = [
        "run",
        "parse",
        "version",
        "help",
        "--help",
        "-h",
        "--version",
        "-V",
    ];
    let looks_like_script = match args.first() {
        Some(first) => {
            !COMMANDS.contains(&first.as_str())
                && (first.ends_with(".btm") || std::path::Path::new(first).is_file())
        }
        None => false,
    };
    if looks_like_script {
        args.insert(0, "run".to_string());
    }
    args
}

fn dispatch(args: Vec<String>) -> ExitCode {
    if args.is_empty() {
        // Bare `bartelang`: show the help, but keep treating it as a usage
        // problem (exit 2), as this CLI always has.
        let mut command = cli();
        let _ = command.print_help();
        return ExitCode::from(2);
    }

    let argv = std::iter::once("bartelang".to_string()).chain(args);
    let matches = match cli().try_get_matches_from(argv) {
        Ok(matches) => matches,
        Err(error) => {
            // clap decides where the text goes and what code it implies: help
            // and `--version` print to stdout and exit 0, a usage problem
            // prints to stderr and exits 2.
            let _ = error.print();
            return ExitCode::from(error.exit_code() as u8);
        }
    };

    match matches.subcommand() {
        Some(("run", sub)) => run_script(sub),
        Some(("parse", sub)) => parse_script(sub),
        Some(("version", _)) => {
            emit_line(&format!("bartelang {}", env!("CARGO_PKG_VERSION")));
            ExitCode::SUCCESS
        }
        // `subcommand_required` makes this unreachable; treat it as usage.
        _ => ExitCode::from(2),
    }
}

fn include_dirs_from(sub: &ArgMatches) -> Vec<PathBuf> {
    sub.get_many::<String>("include_dirs")
        .map(|values| values.map(PathBuf::from).collect())
        .unwrap_or_default()
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

fn run_script(sub: &ArgMatches) -> ExitCode {
    let include_dirs = include_dirs_from(sub);
    let Some(path) = sub.get_one::<String>("script") else {
        // clap enforces `required`, so this is unreachable; treat it as usage.
        return ExitCode::from(2);
    };
    let script_args: Vec<String> = sub
        .get_many::<String>("args")
        .map(|values| values.cloned().collect())
        .unwrap_or_default();

    let loaded = match loader_with(include_dirs).load_file(path) {
        Ok(loaded) => loaded,
        Err(error) => return report_load_error(&error),
    };

    let mut interp = Interp::new(script_args);
    match interp.run_loaded(&loaded) {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("bartelang: {}", error.render(&loaded.sources));
            ExitCode::from(1)
        }
    }
}

fn parse_script(sub: &ArgMatches) -> ExitCode {
    let include_dirs = include_dirs_from(sub);
    let Some(path) = sub.get_one::<String>("script") else {
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
