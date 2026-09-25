//! Error types shared by the lexer, parser, loader and interpreter.
//!
//! Bartelang keeps the Visual Basic 6 notion of a *numbered* error, so that the
//! `Err` object can expose `Err.Number` and `Err.Description` inside a
//! `Catch` block.  Numbers in the 1..=1000 range follow VBA's classic codes;
//! anything from 1000 up is Bartelang-specific.

use std::fmt;

/// A runtime error, carrying a VB-style error number.
#[derive(Debug, Clone, PartialEq)]
pub struct BrtError {
    pub number: i32,
    pub message: String,
    /// `Err.Source`.  Set by `Err.Raise`, left empty by interpreter errors.
    pub source: Option<String>,
    /// The source line the statement came from, attached as the error unwinds
    /// out of the statement that raised it.  `None` for errors raised outside
    /// any statement, such as `Err.Raise` at the prompt.
    pub line: Option<usize>,
    /// The source *unit* (file) that line belongs to, attached alongside it.
    /// The renderer resolves it to a name through the program's
    /// [`SourceMap`](crate::loader::SourceMap); `None` for a single source
    /// parsed straight from a string.
    pub unit: Option<usize>,
}

impl BrtError {
    /// A numbered error with no source and no location; the interpreter fills
    /// the location in as the error unwinds.
    pub fn new(number: i32, message: impl Into<String>) -> Self {
        BrtError {
            number,
            message: message.into(),
            source: None,
            line: None,
            unit: None,
        }
    }

    /// Attaches an `Err.Source` to this error.
    pub fn with_source(mut self, source: Option<String>) -> Self {
        self.source = source;
        self
    }

    /// The one-line form, naming the file when the source map knows it.
    pub fn headline(&self, file: Option<&str>) -> String {
        match (file, self.line) {
            (Some(file), Some(line)) => format!(
                "runtime error {} in {file} at line {line}: {}",
                self.number, self.message
            ),
            (Some(file), None) => {
                format!("runtime error {} in {file}: {}", self.number, self.message)
            }
            (None, Some(line)) => format!(
                "runtime error {} at line {line}: {}",
                self.number, self.message
            ),
            (None, None) => format!("runtime error {}: {}", self.number, self.message),
        }
    }

    /// The display form used by the CLI, including the quoted source line.
    pub fn render(&self, sources: &crate::loader::SourceMap) -> String {
        let file = self.unit.and_then(|unit| sources.name(unit));
        let mut out = self.headline(file);
        if let Some(line) = self.line {
            // A unit-less error still belongs to unit 0 when the program came
            // from a single string, so the quote keeps working there.
            let unit = self.unit.unwrap_or(0);
            if let Some(text) = sources.line(unit, line) {
                // No caret: a line is what the interpreter knows, not a column.
                out.push_str(&format!("\n  {line} | {text}"));
            }
        }
        out
    }

    /// 5 - Invalid procedure call or argument.
    pub fn invalid_call(message: impl Into<String>) -> Self {
        Self::new(5, message)
    }

    /// 9 - Subscript out of range.
    pub fn subscript(message: impl Into<String>) -> Self {
        Self::new(9, message)
    }

    /// 11 - Division by zero.
    pub fn division_by_zero() -> Self {
        Self::new(11, "Division by zero")
    }

    /// 13 - Type mismatch.
    pub fn type_mismatch(message: impl Into<String>) -> Self {
        Self::new(13, message)
    }

    /// 35 - Sub or Function not defined.
    pub fn not_defined(name: &str) -> Self {
        Self::new(35, format!("Sub or Function not defined: {name}"))
    }

    /// 52 - Bad file name or number.
    pub fn bad_file_channel(channel: u8) -> Self {
        Self::new(52, format!("Bad file name or number: #{channel} is not open"))
    }

    /// 53 - File not found.
    pub fn path_not_found(path: &str) -> Self {
        Self::new(53, format!("File not found: {path}"))
    }

    /// 424 - Object required.
    pub fn object_required(message: impl Into<String>) -> Self {
        Self::new(424, message)
    }

    /// 429 - The requested object type cannot be created.
    /// 429 - The requested object type cannot be created.  The listing is
    /// generated from `OBJECT_TYPES`, so it can never go stale.
    pub fn cant_create_object(name: &str, known: &[(&str, &[&str])]) -> Self {
        let listing = known
            .iter()
            .map(|(canonical, aliases)| {
                if aliases.is_empty() {
                    (*canonical).to_string()
                } else {
                    format!("{canonical} ({})", aliases.join(", "))
                }
            })
            .collect::<Vec<_>>()
            .join(", ");
        Self::new(
            429,
            format!("Cannot create object \"{name}\" (known types: {listing})"),
        )
    }

    /// 438 - Object doesn't support this property or method.
    pub fn no_such_member(object: &str, member: &str) -> Self {
        Self::new(
            438,
            format!("Object '{object}' doesn't support this property or method: '{member}'"),
        )
    }

    /// 450 - Wrong number of arguments.
    pub fn wrong_arg_count(name: &str, got: usize, wanted: &str) -> Self {
        Self::new(
            450,
            format!("Wrong number of arguments for {name}: got {got}, expected {wanted}"),
        )
    }

    /// 500 - Bartelang-specific: reference to an undeclared variable.
    ///
    /// VB6 without `Option Explicit` silently materialises a local `Empty`;
    /// Bartelang chooses to be strict so typos surface immediately.
    pub fn not_defined_variable(name: &str) -> Self {
        Self::new(500, format!("Variable not defined: {name}"))
    }

    /// 1001 - Bartelang-specific: the shell could not be spawned.
    pub fn shell(message: impl Into<String>) -> Self {
        Self::new(1001, message)
    }

    /// 1002 - Bartelang-specific: HTTP transport failure.
    pub fn http(message: impl Into<String>) -> Self {
        Self::new(1002, message)
    }

    /// 1003 - Bartelang-specific: runaway recursion.
    pub fn stack_overflow(limit: usize) -> Self {
        Self::new(
            1003,
            format!("Call stack overflow: exceeded {limit} nested procedure calls"),
        )
    }

    /// 6 - Overflow.  VB6's own number for a result that does not fit.
    pub fn overflow(op: &str) -> Self {
        Self::new(6, format!("Overflow evaluating '{op}'"))
    }

    /// 501 - Bartelang-specific: assignment to a name declared `Const`.
    pub fn const_assignment(name: &str) -> Self {
        Self::new(501, format!("Cannot assign to constant '{name}'"))
    }

    /// 94 - Invalid use of Null.  VB6 propagated Null instead; Bartelang would
    /// rather say so and point at `IsNull`.
    pub fn null_use(where_: &str) -> Self {
        Self::new(
            94,
            format!("Invalid use of Null in {where_} (use IsNull to test for it)"),
        )
    }

    /// 5 - `Exit For`/`Exit While`/`Exit Do` with no matching enclosing loop.
    pub fn exit_outside_loop(word: &str, expected: &str) -> Self {
        Self::new(5, format!("Exit {word} used outside a {expected} loop"))
    }
}
impl fmt::Display for BrtError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "error {}: {}", self.number, self.message)
    }
}

impl std::error::Error for BrtError {}

/// A lexing or parsing failure, pointing at a position in the source text.
#[derive(Debug, Clone, PartialEq)]
pub struct SyntaxError {
    pub message: String,
    pub line: usize,
    pub col: usize,
    /// Which source unit (file) of a multi-file program this error is in.
    /// `None` for a single source parsed directly from a string.
    pub unit: Option<usize>,
}

impl SyntaxError {
    /// A parse or lex failure at a 1-based line and column.
    pub fn new(message: impl Into<String>, line: usize, col: usize) -> Self {
        SyntaxError {
            message: message.into(),
            line,
            col,
            unit: None,
        }
    }

    /// Records which source unit of a loaded program this error belongs to.
    pub fn in_unit(mut self, unit: usize) -> Self {
        self.unit = Some(unit);
        self
    }

    /// The one-line form, naming the file when the source map knows it.
    pub(crate) fn headline(&self, file: Option<&str>) -> String {
        match file {
            Some(file) => format!(
                "syntax error in {file} at line {}, column {}: {}",
                self.line, self.col, self.message
            ),
            None => format!(
                "syntax error at line {}, column {}: {}",
                self.line, self.col, self.message
            ),
        }
    }
}

impl fmt::Display for SyntaxError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.headline(None))
    }
}

impl std::error::Error for SyntaxError {}

/// Renders a syntax error with the offending source line and a caret, e.g.
///
/// ```text
/// syntax error in lib/math.btm at line 4, column 13: expected ')'
///   4 | Let x = (1 + 2
///     |             ^
/// ```
pub fn render_syntax_error(err: &SyntaxError, sources: &crate::loader::SourceMap) -> String {
    let file = err.unit.and_then(|unit| sources.name(unit));
    let mut out = err.headline(file);
    // An error from a single string has no unit, and its lines live in unit 0.
    let unit = err.unit.unwrap_or(0);
    if let Some(text) = sources.line(unit, err.line) {
        let gutter = err.line.to_string();
        let pad = " ".repeat(gutter.len());
        out.push('\n');
        out.push_str(&format!("  {gutter} | {text}\n"));
        let caret_col = err.col.saturating_sub(1);
        out.push_str(&format!("  {pad} | {}^", " ".repeat(caret_col)));
    }
    out
}
