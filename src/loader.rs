//! Multi-file programs: `Include`, path resolution and the source map.
//!
//! A Bartelang program used to be one file.  [`Loader`] changes that: it reads
//! the entry file, and wherever it finds `Include "path.btm"` at the top level,
//! it reads that file too and splices its statements into the program before
//! anything runs.  Declarations hoist exactly as they do in a single file, so
//! a procedure defined in an included file may be called from the entry file
//! and vice versa.
//!
//! Three rules keep it predictable:
//!
//! * **A file is read once per canonical path.**  Two files that both include
//!   the same library get one copy of it, and that copy's top-level code runs
//!   once.  A *cycle* (`a.btm` includes `b.btm` includes `a.btm`) is an error
//!   that prints the chain, not an infinite loop.
//! * **Includes resolve relative to the including file first**, then the
//!   loader's include directories, then (in the CLI) `$BARTELANG_PATH`.  The
//!   current directory is deliberately *not* searched: a script behaves the
//!   same from any shell.
//! * **Every source unit is kept**, so an error inside `lib/math.btm` names
//!   that file and quotes its line through the [`SourceMap`].

use std::collections::HashMap;
use std::fmt;
use std::fs;
use std::path::{Path, PathBuf};
use std::rc::Rc;

use crate::ast::{peel, Stmt};
use crate::error::{render_syntax_error, SyntaxError};
use crate::lexer::tokenize;
use crate::parser::Parser;
use crate::Loaded;

/// One source file that went into a program.  Kept behind [`SourceMap`], which
/// exposes only the name and the lines a diagnostic needs.
#[derive(Debug, Clone)]
pub(crate) struct SourceFile {
    name: Option<String>,
    lines: Vec<String>,
}

impl SourceFile {
    /// Splits text into lines the way the lexer does, so line numbers agree.
    fn from_text(name: Option<String>, source: &str) -> Self {
        let lines = source
            .split('\n')
            .map(|line| line.strip_suffix('\r').unwrap_or(line).to_string())
            .collect();
        SourceFile { name, lines }
    }
}

/// Every file a program was loaded from, indexed by the unit ids the parser
/// tags statements with.  Unit 0 is the entry file.
#[derive(Debug, Clone, Default)]
pub struct SourceMap {
    files: Rc<Vec<SourceFile>>,
}

impl SourceMap {
    pub(crate) fn from_files(files: Vec<SourceFile>) -> Self {
        SourceMap {
            files: Rc::new(files),
        }
    }

    /// A map for a single source that never touched the filesystem.
    pub(crate) fn from_source(source: &str) -> Self {
        Self::from_files(vec![SourceFile::from_text(None, source)])
    }

    /// How many source units the program was loaded from.
    pub fn len(&self) -> usize {
        self.files.len()
    }

    /// True when no source was recorded at all.
    pub fn is_empty(&self) -> bool {
        self.files.is_empty()
    }

    /// The display name of a unit, when it has one.
    pub fn name(&self, unit: usize) -> Option<&str> {
        self.files.get(unit).and_then(|file| file.name.as_deref())
    }

    /// The text of a 1-based line of a unit, for diagnostics.
    pub fn line(&self, unit: usize, line: usize) -> Option<&str> {
        if line == 0 {
            return None;
        }
        self.files
            .get(unit)
            .and_then(|file| file.lines.get(line - 1))
            .map(String::as_str)
    }
}

/// Why a program could not be loaded.
#[derive(Debug)]
pub enum LoadErrorKind {
    /// The entry file itself could not be read.  The CLI treats this as a
    /// command-line problem (exit 2), as it always has.
    Unreadable { path: String, message: String },
    /// A source file did not parse.  The error knows its unit, so it renders
    /// with the right file name.
    Syntax(SyntaxError),
    /// A numbered load-time problem: a missing include (53), an include cycle
    /// (1004), or a declaration defined more than once (1005).  `unit`/`line`
    /// point at the `Include` or the redeclaration that caused it.
    Include {
        number: i32,
        message: String,
        unit: usize,
        line: usize,
    },
}

/// A failure while loading a program, with the source map as it stood when the
/// failure happened so the error can still quote the offending line.
#[derive(Debug)]
pub struct LoadError {
    kind: LoadErrorKind,
    sources: SourceMap,
}

impl LoadError {
    fn new(kind: LoadErrorKind, sources: SourceMap) -> Self {
        LoadError { kind, sources }
    }

    fn unreadable(path: &Path, error: std::io::Error) -> Self {
        LoadError {
            kind: LoadErrorKind::Unreadable {
                path: path.to_string_lossy().into_owned(),
                message: error.to_string(),
            },
            sources: SourceMap::default(),
        }
    }

    /// What went wrong, for callers that need to branch on it.
    pub fn kind(&self) -> &LoadErrorKind {
        &self.kind
    }

    /// The VB-style error number, when this is a numbered include problem.
    pub fn number(&self) -> Option<i32> {
        match &self.kind {
            LoadErrorKind::Include { number, .. } => Some(*number),
            _ => None,
        }
    }

    /// The display form used by the CLI, quoting the offending line when the
    /// source map can provide it.
    pub fn render(&self) -> String {
        match &self.kind {
            LoadErrorKind::Syntax(error) => render_syntax_error(error, &self.sources),
            _ => {
                let mut out = self.to_string();
                if let LoadErrorKind::Include { unit, line, .. } = &self.kind {
                    if let Some(text) = self.sources.line(*unit, *line) {
                        out.push_str(&format!("\n  {line} | {text}"));
                    }
                }
                out
            }
        }
    }
}

impl fmt::Display for LoadError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match &self.kind {
            LoadErrorKind::Unreadable { path, message } => {
                write!(f, "cannot read {path}: {message}")
            }
            LoadErrorKind::Syntax(error) => {
                let file = error.unit.and_then(|unit| self.sources.name(unit));
                write!(f, "{}", error.headline(file))
            }
            LoadErrorKind::Include {
                number,
                message,
                unit,
                line,
            } => match self.sources.name(*unit) {
                Some(file) => {
                    write!(f, "load error {number} in {file} at line {line}: {message}")
                }
                None => write!(f, "load error {number} at line {line}: {message}"),
            },
        }
    }
}

impl std::error::Error for LoadError {}

/// Loads programs that may span more than one file.
///
/// ```no_run
/// use bartelang::Loader;
///
/// let loaded = Loader::new()
///     .include_dir("/usr/share/bartelang")
///     .load_file("report.btm")?;
/// # Ok::<(), bartelang::LoadError>(())
/// ```
#[derive(Debug, Default)]
pub struct Loader {
    include_dirs: Vec<PathBuf>,
}

impl Loader {
    /// A loader with no include directories: only file-relative includes work.
    pub fn new() -> Self {
        Loader::default()
    }

    /// Appends a directory to the include search path.  Directories are tried
    /// in the order they were added.
    pub fn include_dir(mut self, dir: impl Into<PathBuf>) -> Self {
        self.include_dirs.push(dir.into());
        self
    }

    /// Reads `path` as the entry file and loads everything it includes.
    pub fn load_file(&self, path: impl AsRef<Path>) -> Result<Loaded, LoadError> {
        let path = path.as_ref();
        let source =
            fs::read_to_string(path).map_err(|error| LoadError::unreadable(path, error))?;
        self.load_source(&path.to_string_lossy(), &source)
    }

    /// Loads already-read source text.  `name` is what diagnostics call the
    /// file, and its parent directory is where that file's includes resolve
    /// from first.  An absolute name that sits under the working directory is
    /// shortened to the relative form in messages.
    pub fn load_source(&self, name: &str, source: &str) -> Result<Loaded, LoadError> {
        let entry = absolute(Path::new(name));
        let base = entry
            .parent()
            .map(Path::to_path_buf)
            .unwrap_or_else(|| PathBuf::from("."));
        let mut state = State {
            loader: self,
            files: Vec::new(),
            seen: HashMap::new(),
            stack: Vec::new(),
            base,
        };
        let display = entry_display(name);
        let unit = state.begin(display.clone(), source);
        let canonical = canonical(&entry);
        state.seen.insert(canonical.clone(), unit);
        state.stack.push((canonical, display));

        let statements = state.parse(unit, source)?;
        let statements = state.expand(statements, &entry)?;
        state.stack.pop();

        state.check_duplicates(&statements)?;
        let sources = SourceMap::from_files(state.files);
        Ok(Loaded {
            program: crate::ast::Program { statements },
            sources,
        })
    }
}

/// Everything one load needs to remember: the files read so far, the canonical
/// paths already included, and the chain currently being loaded.
struct State<'a> {
    loader: &'a Loader,
    files: Vec<SourceFile>,
    /// Canonical path -> unit id, so a file is only read once.
    seen: HashMap<PathBuf, usize>,
    /// Canonical paths being loaded right now, innermost last, each with the
    /// display name that goes into a cycle message.
    stack: Vec<(PathBuf, String)>,
    /// The entry file's directory: display names for included files are
    /// computed relative to it.
    base: PathBuf,
}

impl State<'_> {
    /// Registers a new source unit and returns its id.
    fn begin(&mut self, name: String, source: &str) -> usize {
        let unit = self.files.len();
        self.files.push(SourceFile::from_text(Some(name), source));
        unit
    }

    fn parse(&self, unit: usize, source: &str) -> Result<Vec<Stmt>, LoadError> {
        let lexed = tokenize(source).map_err(|error| {
            LoadError::new(
                LoadErrorKind::Syntax(error.in_unit(unit)),
                SourceMap::from_files(self.files.clone()),
            )
        })?;
        let mut parser = Parser::new(lexed).with_unit(unit);
        let program = parser.parse_program().map_err(|error| {
            LoadError::new(
                LoadErrorKind::Syntax(error.in_unit(unit)),
                SourceMap::from_files(self.files.clone()),
            )
        })?;
        Ok(program.statements)
    }

    /// Replaces every top-level `Include` with the statements of the file it
    /// names, depth first, so a file is only ever read once.
    fn expand(
        &mut self,
        statements: Vec<Stmt>,
        current_file: &Path,
    ) -> Result<Vec<Stmt>, LoadError> {
        let mut expanded = Vec::with_capacity(statements.len());
        for statement in statements {
            if let Stmt::Include { path } = peel(&statement) {
                let (at_unit, at_line) = located_at(&statement);
                let resolved = self.resolve(path, current_file, at_unit, at_line)?;
                let canonical_path = canonical(&resolved.absolute);
                if let Some(index) = self
                    .stack
                    .iter()
                    .position(|(path, _)| path == &canonical_path)
                {
                    let mut chain: Vec<&str> = self.stack[index..]
                        .iter()
                        .map(|(_, name)| name.as_str())
                        .collect();
                    chain.push(&resolved.display);
                    return Err(self.include_error(
                        1004,
                        format!("Circular include: {}", chain.join(" -> ")),
                        at_unit,
                        at_line,
                    ));
                }
                if self.seen.contains_key(&canonical_path) {
                    // Already part of this program: include once, run once.
                    continue;
                }
                let source = fs::read_to_string(&resolved.absolute).map_err(|error| {
                    self.include_error(
                        53,
                        format!("Could not read {}: {error}", resolved.display),
                        at_unit,
                        at_line,
                    )
                })?;
                let included = self.begin(resolved.display.clone(), &source);
                self.seen.insert(canonical_path.clone(), included);
                self.stack.push((canonical_path, resolved.display));
                let parsed = self.parse(included, &source)?;
                let nested = self.expand(parsed, &resolved.absolute)?;
                self.stack.pop();
                expanded.extend(nested);
            } else {
                expanded.push(statement);
            }
        }
        Ok(expanded)
    }

    /// Applies the resolution order: absolute path, then the including file's
    /// directory, then the loader's include directories.
    fn resolve(
        &self,
        path: &str,
        current_file: &Path,
        unit: usize,
        line: usize,
    ) -> Result<Resolved, LoadError> {
        let mut candidates: Vec<PathBuf> = Vec::new();
        if Path::new(path).is_absolute() {
            candidates.push(PathBuf::from(path));
        } else {
            if let Some(dir) = current_file.parent() {
                candidates.push(dir.join(path));
            }
            for dir in &self.loader.include_dirs {
                candidates.push(dir.join(path));
            }
        }
        let found = candidates.iter().find(|candidate| candidate.is_file());
        let Some(found) = found else {
            let searched = candidates
                .iter()
                .map(|candidate| candidate.display().to_string())
                .collect::<Vec<_>>()
                .join(", ");
            let mut message = format!("File not found: {path} (searched: {searched})");
            if Path::new(path).is_file() {
                message.push_str(&format!(
                    "; note: {path} exists in the current directory, but includes resolve \
                     relative to the including file - add -I . to search here"
                ));
            }
            return Err(self.include_error(53, message, unit, line));
        };
        let absolute = absolute(found);
        let display = self.display_name(&absolute);
        Ok(Resolved { absolute, display })
    }

    /// Names an included file the way the user would say it: relative to the
    /// entry file's directory when it sits underneath it (so messages read the
    /// way the `Include` did), else relative to the working directory, else
    /// absolute.
    fn display_name(&self, absolute: &Path) -> String {
        if let Ok(relative) = absolute.strip_prefix(&self.base) {
            return relative.display().to_string();
        }
        if let Ok(cwd) = std::env::current_dir() {
            if let Ok(relative) = absolute.strip_prefix(&cwd) {
                return relative.display().to_string();
            }
        }
        absolute.display().to_string()
    }

    fn include_error(&self, number: i32, message: String, unit: usize, line: usize) -> LoadError {
        LoadError::new(
            LoadErrorKind::Include {
                number,
                message,
                unit,
                line,
            },
            SourceMap::from_files(self.files.clone()),
        )
    }

    /// Reports a procedure or record type declared twice.  VB6 kept the last
    /// definition silently; with more than one file in play, silence is how a
    /// library quietly stops working, so this is a load error naming both
    /// places.  Declarations buried in a block are checked too, since a
    /// `Sub` inside an `If` still registers itself when that block runs.
    fn check_duplicates(&self, statements: &[Stmt]) -> Result<(), LoadError> {
        let mut procedures: HashMap<&str, (usize, usize)> = HashMap::new();
        let mut types: HashMap<&str, (usize, usize)> = HashMap::new();
        let mut all = Vec::new();
        collect_statements(statements, &mut all);
        for statement in all {
            let (unit, line) = located_at(statement);
            let (kind, name) = match peel(statement) {
                Stmt::SubDeclaration { name, .. } | Stmt::FuncDeclaration { name, .. } => {
                    ("Procedure", name)
                }
                Stmt::TypeDeclaration { name, .. } => ("Record type", name),
                _ => continue,
            };
            let table = if kind == "Procedure" {
                &mut procedures
            } else {
                &mut types
            };
            if let Some((first_unit, first_line)) = table.get(name.as_str()) {
                let first = self.describe(*first_unit, *first_line);
                let message =
                    format!("{kind} '{name}' is defined more than once (first at {first})");
                return Err(self.include_error(1005, message, unit, line));
            }
            table.insert(name.as_str(), (unit, line));
        }
        Ok(())
    }

    fn describe(&self, unit: usize, line: usize) -> String {
        match self.files.get(unit).and_then(|file| file.name.as_deref()) {
            Some(name) => format!("{name} line {line}"),
            None => format!("line {line}"),
        }
    }
}

/// A resolved include target: where it is on disk, and what to call it.
struct Resolved {
    absolute: PathBuf,
    display: String,
}

/// The `(unit, line)` of a statement, when the parser wrapped it.
fn located_at(statement: &Stmt) -> (usize, usize) {
    match statement {
        Stmt::Located { unit, line, .. } => (*unit, *line),
        _ => (0, 0),
    }
}

/// Every statement reachable from `statements`, block bodies included, so that
/// declaration checks see the whole program rather than only its top level.
fn collect_statements<'a>(statements: &'a [Stmt], out: &mut Vec<&'a Stmt>) {
    for statement in statements {
        out.push(statement);
        match peel(statement) {
            Stmt::If {
                then_branch,
                else_branch,
                ..
            } => {
                collect_statements(then_branch, out);
                if let Some(else_branch) = else_branch {
                    collect_statements(else_branch, out);
                }
            }
            Stmt::While { body, .. }
            | Stmt::For { body, .. }
            | Stmt::DoLoop { body, .. }
            | Stmt::ForEach { body, .. }
            | Stmt::With { body, .. }
            | Stmt::SubDeclaration { body, .. }
            | Stmt::FuncDeclaration { body, .. } => collect_statements(body, out),
            Stmt::SelectCase {
                arms, otherwise, ..
            } => {
                for arm in arms {
                    collect_statements(&arm.body, out);
                }
                if let Some(otherwise) = otherwise {
                    collect_statements(otherwise, out);
                }
            }
            Stmt::TryCatch {
                try_block,
                catch_block,
                ..
            } => {
                collect_statements(try_block, out);
                collect_statements(catch_block, out);
            }
            _ => {}
        }
    }
}

/// Makes a path absolute against the current directory, without touching the
/// filesystem (so symlinks stay as the user wrote them).
fn absolute(path: &Path) -> PathBuf {
    if path.is_absolute() {
        path.to_path_buf()
    } else {
        std::env::current_dir()
            .map(|cwd| cwd.join(path))
            .unwrap_or_else(|_| path.to_path_buf())
    }
}

/// The name the entry file is shown by: exactly what the user typed, unless
/// they typed an absolute path that sits under the working directory, in which
/// case the shorter relative form is friendlier and just as findable.
fn entry_display(name: &str) -> String {
    let path = Path::new(name);
    if path.is_absolute() {
        if let Ok(cwd) = std::env::current_dir() {
            if let Ok(relative) = path.strip_prefix(&cwd) {
                return relative.display().to_string();
            }
        }
    }
    name.to_string()
}

/// Identity for include-once and cycle detection.  Falls back to the absolute
/// path when the filesystem will not canonicalise it (it usually will: the
/// file was just read).
fn canonical(path: &Path) -> PathBuf {
    fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    static COUNTER: AtomicUsize = AtomicUsize::new(0);

    fn temp_dir(tag: &str) -> PathBuf {
        let id = COUNTER.fetch_add(1, Ordering::SeqCst);
        let dir = std::env::temp_dir().join(format!(
            "bartelang-loader-{}-{id}-{tag}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).expect("create the temporary directory");
        dir
    }

    fn write(dir: &Path, name: &str, contents: &str) -> PathBuf {
        let path = dir.join(name);
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).expect("create the parent directory");
        }
        fs::write(&path, contents).expect("write the file");
        path
    }

    fn names(loaded: &Loaded) -> Vec<String> {
        loaded
            .program
            .statements
            .iter()
            .filter_map(|statement| match peel(statement) {
                Stmt::FuncDeclaration { name, .. } | Stmt::SubDeclaration { name, .. } => {
                    Some(name.clone())
                }
                _ => None,
            })
            .collect()
    }

    #[test]
    fn an_include_is_expanded_and_named_relative_to_the_entry() {
        let dir = temp_dir("expand");
        write(
            &dir,
            "lib.btm",
            "Function Ten()\n    Ten = 10\nEnd Function\n",
        );
        let script = write(&dir, "script.btm", "Include \"lib.btm\"\n");
        let loaded = Loader::new().load_file(&script).expect("load");
        assert_eq!(loaded.sources.len(), 2);
        assert_eq!(
            loaded.sources.name(0),
            Some(script.to_string_lossy().as_ref())
        );
        assert_eq!(loaded.sources.name(1), Some("lib.btm"));
        assert_eq!(names(&loaded), vec!["ten"]);
    }

    #[test]
    fn the_same_file_is_read_once_however_it_is_spelled() {
        let dir = temp_dir("once");
        write(&dir, "lib.btm", "Debug.Print \"init\"\n");
        fs::create_dir_all(dir.join("sub")).expect("create sub");
        write(
            &dir,
            "script.btm",
            "Include \"lib.btm\"\nInclude \"sub/../lib.btm\"\n",
        );
        let loaded = Loader::new()
            .load_file(dir.join("script.btm"))
            .expect("load");
        // Entry plus one library, not two copies of it.
        assert_eq!(loaded.sources.len(), 2);
    }

    #[test]
    fn the_including_files_directory_beats_the_include_path() {
        let dir = temp_dir("order");
        write(
            &dir,
            "lib.btm",
            "Function Real()\n    Real = 1\nEnd Function\n",
        );
        write(
            &dir,
            "elsewhere/lib.btm",
            "Function Decoy()\n    Decoy = 1\nEnd Function\n",
        );
        let script = write(&dir, "script.btm", "Include \"lib.btm\"\n");
        let loaded = Loader::new()
            .include_dir(dir.join("elsewhere"))
            .load_file(&script)
            .expect("load");
        assert_eq!(names(&loaded), vec!["real"]);
    }

    #[test]
    fn include_directories_are_tried_in_order() {
        let dir = temp_dir("dirs");
        write(
            &dir,
            "first/lib.btm",
            "Function First()\n    First = 1\nEnd Function\n",
        );
        write(
            &dir,
            "second/lib.btm",
            "Function Second()\n    Second = 1\nEnd Function\n",
        );
        let script = write(&dir, "script.btm", "Include \"lib.btm\"\n");
        let loaded = Loader::new()
            .include_dir(dir.join("first"))
            .include_dir(dir.join("second"))
            .load_file(&script)
            .expect("load");
        assert_eq!(names(&loaded), vec!["first"]);
    }

    #[test]
    fn a_missing_include_is_error_53_and_lists_the_search() {
        let dir = temp_dir("missing");
        write(
            &dir,
            "script.btm",
            "Include \"lib/util.btm\"\n",
        );
        let error = Loader::new()
            .include_dir(dir.join("libs"))
            .load_file(dir.join("script.btm"))
            .expect_err("the include is missing");
        assert_eq!(error.number(), Some(53));
        let message = error.to_string();
        assert!(message.contains("File not found: lib/util.btm"), "{message}");
        assert!(message.contains("searched:"), "{message}");
        assert!(message.contains("libs"), "{message}");
    }

    #[test]
    fn a_cycle_is_error_1004_with_the_chain() {
        let dir = temp_dir("cycle");
        write(&dir, "a.btm", "Include \"b.btm\"\n");
        write(&dir, "b.btm", "Include \"a.btm\"\n");
        let error = Loader::new()
            .load_file(dir.join("a.btm"))
            .expect_err("the include is circular");
        assert_eq!(error.number(), Some(1004));
        assert!(
            error.to_string().contains("a.btm -> b.btm -> a.btm"),
            "{error}"
        );
    }

    #[test]
    fn a_duplicate_declaration_is_error_1005_and_names_both_places() {
        let dir = temp_dir("duplicate");
        write(
            &dir,
            "lib.btm",
            "Sub Report()\n    Debug.Print \"lib\"\nEnd Sub\n",
        );
        write(
            &dir,
            "script.btm",
            "Include \"lib.btm\"\nSub Report()\n    Debug.Print \"script\"\nEnd Sub\n",
        );
        let error = Loader::new()
            .load_file(dir.join("script.btm"))
            .expect_err("the procedure is defined twice");
        assert_eq!(error.number(), Some(1005));
        let message = error.to_string();
        assert!(message.contains("'report' is defined more than once"), "{message}");
        assert!(message.contains("lib.btm line 1"), "{message}");
        // The error points at the second definition, in the entry file.
        assert!(message.contains("script.btm at line 2"), "{message}");
    }

    #[test]
    fn a_duplicate_hidden_in_a_block_is_still_error_1005() {
        let dir = temp_dir("duplicate-nested");
        write(
            &dir,
            "script.btm",
            "Sub A()\nEnd Sub\nIf 1 Then\n    Sub A()\n    End Sub\nEnd If\n",
        );
        let error = Loader::new()
            .load_file(dir.join("script.btm"))
            .expect_err("the procedure is defined twice");
        assert_eq!(error.number(), Some(1005));
        assert!(
            error.to_string().contains("'a' is defined more than once"),
            "{error}"
        );
    }

    #[test]
    fn a_source_map_quotes_lines_by_unit() {
        let dir = temp_dir("lines");
        write(&dir, "lib.btm", "' one\n' two\n");
        let script = write(&dir, "script.btm", "Include \"lib.btm\"\n");
        let loaded = Loader::new().load_file(&script).expect("load");
        assert_eq!(loaded.sources.line(0, 1), Some("Include \"lib.btm\""));
        assert_eq!(loaded.sources.line(1, 2), Some("' two"));
        assert_eq!(loaded.sources.line(9, 1), None);
    }
}