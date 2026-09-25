//! Recursive-descent parser: tokens in, AST out.
//!
//! Operator precedence follows VB6, loosest first:
//!
//! ```text
//! |            cross-boundary pipe (Bartelang addition)
//! Or
//! And
//! Not
//! = <> < > <= >=
//! &
//! + -
//! Mod
//! \            integer division
//! * /
//! unary + -
//! ^            exponentiation (right associative)
//! ```

use std::collections::HashSet;

use crate::ast::{
    CasePattern, Expr, FileMode, LoopCondition, PrintItem, ProcedureKind, Program, SelectArm,
    Separator, Stmt, TypeField,
};
use crate::error::SyntaxError;
use crate::lexer::{tokenize, Lexed, Token};
use crate::value::Variant;

/// Function names that always win over a same-named array, so that
/// `Dim args(3)` cannot shadow `ARGS`.
const BUILTIN_NAMES: &[&str] = &[
    "args", "env", "inputbox", "createobject", "len", "left", "right", "mid", "instr", "instrrev",
    "ucase", "lcase", "trim", "ltrim", "rtrim", "replace", "chr", "asc", "val", "str", "cstr",
    "cint", "clng", "cdbl", "csng", "cbool", "abs", "int", "fix", "sgn", "sqr", "now", "date",
    "time", "hex", "oct", "space", "string", "isnumeric", "isempty", "eof",
    "array", "split", "join", "lbound", "ubound",
    "format", "round", "rnd", "randomize", "timer", "dateadd", "datediff", "datepart",
    "typename", "isobject", "isarray", "isdate", "isnull",
    "freefile", "lof", "loc", "seek",
];

/// VB-style named constants available without `Dim`.
const NAMED_CONSTANTS: &[(&str, &str)] = &[
    ("vbcrlf", "\r\n"),
    ("vblf", "\n"),
    ("vbcr", "\r"),
    ("vbtab", "\t"),
    ("vbnullstring", ""),
];

/// A recursive-descent parser over a token stream.  The operator precedence it
/// implements is documented at the top of this module.
pub struct Parser {
    tokens: Vec<Token>,
    lines: Vec<usize>,
    cols: Vec<usize>,
    pos: usize,
    /// Names declared with `Dim name(...)`, so `name(1)` parses as indexing
    /// rather than a call.  Calls still win at runtime, so ordering is safe.
    declared_arrays: HashSet<String>,
    /// Bodies of the enclosing `With` blocks, innermost last.  A leading `.`
    /// resolves against the innermost entry, which makes `With` lexical.
    with_stack: Vec<String>,
    /// Counter for the synthetic names that hold captured `With` objects.
    with_counter: usize,
    /// Which source unit (file) this parser is reading.  Statements are tagged
    /// with it so the loader's source map can name the file on an error.
    unit: usize,
    /// How many block bodies enclose the current statement.  `Include` is only
    /// legal at zero, i.e. at the top level of a file.
    block_depth: usize,
}

impl Parser {
    /// A parser ready to consume an already-lexed program.
    pub fn new(lexed: Lexed) -> Self {
        Parser {
            tokens: lexed.tokens,
            lines: lexed.lines,
            cols: lexed.cols,
            pos: 0,
            declared_arrays: HashSet::new(),
            with_stack: Vec::new(),
            with_counter: 0,
            unit: 0,
            block_depth: 0,
        }
    }

    /// The same parser, reading the source unit `unit` of a multi-file program.
    pub fn with_unit(mut self, unit: usize) -> Self {
        self.unit = unit;
        self
    }

    /// Convenience: tokenise and parse in one step.
    pub fn parse_source(src: &str) -> Result<(Program, Vec<String>), SyntaxError> {
        let lexed = tokenize(src)?;
        let source_lines = lexed.source_lines.clone();
        let mut parser = Parser::new(lexed);
        let program = parser.parse_program()?;
        Ok((program, source_lines))
    }

    // ---------------------------------------------------------------- helpers

    fn peek(&self) -> &Token {
        self.tokens.get(self.pos).unwrap_or(&Token::Eof)
    }

    fn peek_at(&self, offset: usize) -> &Token {
        self.tokens.get(self.pos + offset).unwrap_or(&Token::Eof)
    }

    fn advance(&mut self) -> Token {
        let token = self.peek().clone();
        if self.pos < self.tokens.len() {
            self.pos += 1;
        }
        token
    }

    fn at_eof(&self) -> bool {
        matches!(self.peek(), Token::Eof)
    }

    fn at_statement_end(&self) -> bool {
        self.peek().ends_statement()
    }

    fn check(&self, token: &Token) -> bool {
        self.peek() == token
    }

    fn eat(&mut self, token: &Token) -> bool {
        if self.check(token) {
            self.advance();
            true
        } else {
            false
        }
    }

    fn check_ident(&self, name: &str) -> bool {
        matches!(self.peek(), Token::Ident(n) if n == name)
    }

    fn eat_ident(&mut self, name: &str) -> bool {
        if self.check_ident(name) {
            self.advance();
            true
        } else {
            false
        }
    }

    /// Current 1-based line/column, for diagnostics.
    fn here(&self) -> (usize, usize) {
        (self.line_of(self.pos), self.col_of(self.pos))
    }

    fn line_of(&self, index: usize) -> usize {
        self.lines.get(index).copied().unwrap_or(1)
    }

    fn col_of(&self, index: usize) -> usize {
        self.cols.get(index).copied().unwrap_or(1)
    }

    fn error_here(&self, message: impl Into<String>) -> SyntaxError {
        let (line, col) = self.here();
        SyntaxError::new(message, line, col).in_unit(self.unit)
    }

    /// A diagnostic pointing at an earlier token than the current position.
    fn error_at(&self, index: usize, message: impl Into<String>) -> SyntaxError {
        SyntaxError::new(message, self.line_of(index), self.col_of(index)).in_unit(self.unit)
    }

    fn expect(&mut self, token: &Token, what: &str) -> Result<(), SyntaxError> {
        if self.check(token) {
            self.advance();
            Ok(())
        } else {
            Err(self.error_here(format!(
                "expected {what}, found {}",
                self.peek().describe()
            )))
        }
    }

    fn expect_ident_word(&mut self, word: &str) -> Result<(), SyntaxError> {
        if self.eat_ident(word) {
            Ok(())
        } else {
            Err(self.error_here(format!(
                "expected '{word}', found {}",
                self.peek().describe()
            )))
        }
    }

    fn expect_name(&mut self, what: &str) -> Result<String, SyntaxError> {
        match self.peek().clone() {
            Token::Ident(name) => {
                self.advance();
                Ok(name)
            }
            other => Err(self.error_here(format!("expected {what}, found {}", other.describe()))),
        }
    }

    fn skip_separators(&mut self) {
        while matches!(self.peek(), Token::Newline | Token::Colon) {
            self.advance();
        }
    }

    fn expect_statement_end(&mut self) -> Result<(), SyntaxError> {
        if self.at_statement_end() {
            Ok(())
        } else {
            Err(self.error_here(format!(
                "expected end of statement, found {}",
                self.peek().describe()
            )))
        }
    }

    // -------------------------------------------------------------- top level

    /// Parses the whole token stream into a program.
    pub fn parse_program(&mut self) -> Result<Program, SyntaxError> {
        let mut statements = Vec::new();
        loop {
            self.skip_separators();
            if self.at_eof() {
                break;
            }
            statements.push(self.parse_located_statement()?);
            self.expect_statement_end()?;
        }
        Ok(Program { statements })
    }

    /// Parses statements until one of `terminators` starts a line, leaving the
    /// terminator in place for the caller.
    fn parse_block_until(&mut self, terminators: &[&str]) -> Result<Vec<Stmt>, SyntaxError> {
        // Everything inside a block is one level down, which is what makes
        // `Include` a top-level-only statement.
        self.block_depth += 1;
        let result = self.parse_block_body(terminators);
        self.block_depth -= 1;
        result
    }

    fn parse_block_body(&mut self, terminators: &[&str]) -> Result<Vec<Stmt>, SyntaxError> {
        let mut statements = Vec::new();
        loop {
            self.skip_separators();
            if self.at_eof() {
                return Err(self.error_here(format!(
                    "unexpected end of file, expected {}",
                    terminators
                        .iter()
                        .map(|t| format!("'{t}'"))
                        .collect::<Vec<_>>()
                        .join(" or ")
                )));
            }
            if let Token::Ident(name) = self.peek() {
                if terminators.contains(&name.as_str()) {
                    break;
                }
            }
            statements.push(self.parse_located_statement()?);
            self.expect_statement_end()?;
        }
        Ok(statements)
    }

    /// Parses one statement, tagging it with the source unit and the line it
    /// started on.  The interpreter turns that tag into the file and line on a
    /// runtime error.
    fn parse_located_statement(&mut self) -> Result<Stmt, SyntaxError> {
        let line = self.line_of(self.pos);
        let inner = self.parse_statement()?;
        Ok(Stmt::Located {
            unit: self.unit,
            line,
            inner: Box::new(inner),
        })
    }

    /// Consumes an optional VB6 `As <Type>` clause and reports the type name.
    fn take_as_type(&mut self) -> Result<Option<String>, SyntaxError> {
        if self.eat_ident("as") {
            return Ok(Some(self.expect_name(
                "a type name (String, Integer, Long, Double, Boolean, Variant, Object, or a Type you declared)",
            )?));
        }
        Ok(None)
    }

    /// Consumes an optional `As <Type>` clause, discarding the type name.
    fn eat_as_type(&mut self) -> Result<(), SyntaxError> {
        self.take_as_type()?;
        Ok(())
    }

    // ------------------------------------------------------------- statements

    fn parse_statement(&mut self) -> Result<Stmt, SyntaxError> {
        let start = self.pos;
        match self.peek().clone() {
            Token::Ident(name) => match name.as_str() {
                "dim" => {
                    self.advance();
                    self.parse_dim()
                }
                "let" => {
                    self.advance();
                    self.parse_assignment(false)
                }
                "set" => {
                    self.advance();
                    self.parse_assignment(true)
                }
                "if" => {
                    self.advance();
                    self.parse_if()
                }
                "while" => {
                    self.advance();
                    self.parse_while()
                }
                "for" => {
                    self.advance();
                    if self.eat_ident("each") {
                        self.parse_for_each()
                    } else {
                        self.parse_for()
                    }
                }
                "do" => {
                    self.advance();
                    self.parse_do_loop()
                }
                "select" => {
                    self.advance();
                    self.parse_select_case()
                }
                "with" => {
                    self.advance();
                    self.parse_with()
                }
                "const" => {
                    self.advance();
                    self.parse_const()
                }
                "include" => {
                    // A real statement, not a comment metacommand: the loader
                    // reads the file once per canonical path, before anything
                    // runs.  Like `open` and `print`, the keyword wins outright.
                    self.advance();
                    self.parse_include()
                }
                "type" => {
                    self.advance();
                    self.parse_type()
                }
                "redim" => {
                    self.advance();
                    self.parse_redim()
                }
                "exit" => {
                    self.advance();
                    self.parse_exit()
                }
                "try" => {
                    self.advance();
                    self.parse_try()
                }
                "sub" => {
                    self.advance();
                    self.parse_procedure(false)
                }
                "function" => {
                    self.advance();
                    self.parse_procedure(true)
                }
                "print" => {
                    self.advance();
                    self.parse_print()
                }
                "open" => {
                    self.advance();
                    self.parse_open()
                }
                "close" => {
                    self.advance();
                    self.parse_close()
                }
                "line" => {
                    // `Line Input` is a statement; a bare `line` is just a
                    // variable.
                    if matches!(self.peek_at(1), Token::Ident(next) if next.as_str() == "input") {
                        self.advance();
                        self.parse_line_input()
                    } else {
                        self.parse_assign_or_call(start)
                    }
                }
                "input" => {
                    if matches!(self.peek_at(1), Token::Hash) {
                        self.advance();
                        self.parse_input()
                    } else {
                        self.parse_assign_or_call(start)
                    }
                }
                "write" => {
                    if matches!(self.peek_at(1), Token::Hash) {
                        self.advance();
                        self.parse_write()
                    } else {
                        self.parse_assign_or_call(start)
                    }
                }
                "call" => {
                    self.advance();
                    let value = self.parse_expr()?;
                    Ok(Stmt::ExprStatement(value))
                }
                _ => self.parse_assign_or_call(start),
            },
            Token::Dot => self.parse_with_member_statement(),
            _ => {
                let value = self.parse_expr()?;
                Ok(Stmt::ExprStatement(value))
            }
        }
    }

    /// `Include "path.btm"` - declarations from another file join this program.
    ///
    /// The path must be a literal (the loader resolves it before anything runs)
    /// and the statement must sit at the top level of a file, so that hoisting
    /// sees every declaration it brings in.
    fn parse_include(&mut self) -> Result<Stmt, SyntaxError> {
        if self.block_depth > 0 {
            return Err(self.error_here(
                "Include must be at the top level of a file, not inside a block",
            ));
        }
        match self.peek().clone() {
            Token::Str(path) => {
                self.advance();
                Ok(Stmt::Include { path })
            }
            other => Err(self.error_here(format!(
                "expected a string literal path after Include, found {}",
                other.describe()
            ))),
        }
    }

    /// `Dim x` or `Dim x(10)`, with an optional `As <Type>` tail.
    fn parse_dim(&mut self) -> Result<Stmt, SyntaxError> {
        let name = self.expect_name("a variable name")?;
        if self.check(&Token::LParen) {
            self.advance();
            // `Dim buffer()` declares an empty array, for a later `ReDim`.
            let size = if self.check(&Token::RParen) {
                Expr::Literal(Variant::Int(0))
            } else {
                self.parse_expr()?
            };
            self.expect(&Token::RParen, "')'")?;
            let type_name = self.take_as_type()?;
            self.declared_arrays.insert(name.clone());
            return Ok(Stmt::DimArray {
                name,
                size,
                type_name,
            });
        }
        let type_name = self.take_as_type()?;
        Ok(Stmt::Dim { name, type_name })
    }

    /// `Let x = expr`, `Let x = expr` for arrays, and the `Set` variant.
    fn parse_assignment(&mut self, is_set: bool) -> Result<Stmt, SyntaxError> {
        let name = self.expect_name("a variable name")?;
        if self.check(&Token::LParen) {
            self.advance();
            let index = self.parse_expr()?;
            self.expect(&Token::RParen, "')'")?;
            self.expect(&Token::Eq, "'='")?;
            let value = self.parse_expr()?;
            return Ok(Stmt::LetIndexed { name, index, value });
        }
        self.expect(&Token::Eq, "'='")?;
        let value = self.parse_expr()?;
        Ok(if is_set {
            Stmt::Set {
                target: name,
                value,
            }
        } else {
            Stmt::Let {
                target: name,
                value,
            }
        })
    }

    /// The ambiguous cases: `x = 1` (implicit Let), `obj.Method args`,
    /// `Debug.Print expr`, `arr(1) = v`, or a bare call statement.
    fn parse_assign_or_call(&mut self, start: usize) -> Result<Stmt, SyntaxError> {
        let name = self.expect_name("a statement")?;

        if self.check(&Token::Dot) {
            if name == "debug" {
                if let Token::Ident(next) = self.peek_at(1) {
                    if next == "print" {
                        self.advance();
                        self.advance();
                        let (items, newline) = self.parse_print_items()?;
                        return Ok(Stmt::DebugPrint { items, newline });
                    }
                }
            }
            self.advance(); // '.'
            let method = self.expect_name("a method or property name")?;
            // `obj.Member = value` assigns to a member instead of calling it.
            if self.check(&Token::Eq) {
                self.advance();
                let value = self.parse_expr()?;
                return Ok(Stmt::SetMember {
                    object: name,
                    member: method,
                    value,
                });
            }
            let args = if self.check(&Token::LParen) {
                self.parse_parenthesised_args()?
            } else if self.at_statement_end() {
                Vec::new()
            } else {
                self.parse_bare_args()?
            };
            return Ok(Stmt::ExprStatement(Expr::MethodCall {
                object: name,
                method,
                args,
            }));
        }

        if self.check(&Token::LParen) {
            // Either an array assignment, or an ordinary call/expression.
            self.advance();
            let index = self.parse_expr()?;
            if self.eat(&Token::RParen) && self.check(&Token::Eq) {
                self.advance();
                let value = self.parse_expr()?;
                return Ok(Stmt::LetIndexed { name, index, value });
            }
            self.pos = start;
            let expr = self.parse_expr()?;
            return Ok(Stmt::ExprStatement(expr));
        }

        if self.check(&Token::Eq) {
            self.advance();
            let value = self.parse_expr()?;
            return Ok(Stmt::Let {
                target: name,
                value,
            });
        }

        // Anything else left on the line means a bare-argument call:
        // `Banner "procedures"` is `Banner("procedures")`, VB6 style.
        if !self.at_statement_end() {
            let args = self.parse_bare_args()?;
            return Ok(Stmt::ExprStatement(Expr::FunctionCall { name, args }));
        }

        // A bare identifier: a `Sub` call with no arguments, or an expression.
        self.pos = start;
        let expr = self.parse_expr()?;
        Ok(Stmt::ExprStatement(expr))
    }

    /// `#n`, where `n` may be any expression - including `FreeFile()`.
    fn parse_channel(&mut self) -> Result<Expr, SyntaxError> {
        self.expect(&Token::Hash, "'#' to introduce a file channel")?;
        self.parse_expr()
    }

    fn parse_print(&mut self) -> Result<Stmt, SyntaxError> {
        if !matches!(self.peek(), Token::Hash) {
            return Err(self.error_here(
                "Print needs a file channel, e.g. `Print #1, value` (use Debug.Print for console output)",
            ));
        }
        let channel = self.parse_channel()?;
        // VB6 accepted either separator after the channel.
        if !self.eat(&Token::Comma) && !self.eat(&Token::Semicolon) {
            return Err(self.error_here("expected ',' between the file channel and the values"));
        }
        let (items, newline) = self.parse_print_items()?;
        Ok(Stmt::FilePrint {
            channel,
            items,
            newline,
        })
    }

    /// A comma- or semicolon-separated print list.  The returned flag is false
    /// when the list ended with `;`, which suppresses the line ending.
    fn parse_print_items(&mut self) -> Result<(Vec<PrintItem>, bool), SyntaxError> {
        let mut items = Vec::new();
        let mut newline = true;
        if self.at_statement_end() {
            return Ok((items, newline));
        }
        loop {
            let value = self.parse_expr()?;
            let after = if self.eat(&Token::Semicolon) {
                newline = !self.at_statement_end();
                Separator::None
            } else if self.eat(&Token::Comma) {
                newline = true;
                Separator::Zone
            } else {
                newline = true;
                items.push(PrintItem {
                    value,
                    after: Separator::None,
                });
                break;
            };
            items.push(PrintItem { value, after });
            if self.at_statement_end() {
                break;
            }
        }
        Ok((items, newline))
    }

    /// `Write #1, a, b`
    fn parse_write(&mut self) -> Result<Stmt, SyntaxError> {
        let channel = self.parse_channel()?;
        if !self.eat(&Token::Comma) && !self.eat(&Token::Semicolon) {
            return Err(self.error_here("expected ',' after the file channel"));
        }
        let mut values = Vec::new();
        if !self.at_statement_end() {
            loop {
                values.push(self.parse_expr()?);
                if !self.eat(&Token::Comma) {
                    break;
                }
            }
        }
        Ok(Stmt::FileWrite { channel, values })
    }

    /// `Input #1, a, b`
    fn parse_input(&mut self) -> Result<Stmt, SyntaxError> {
        let channel = self.parse_channel()?;
        if !self.eat(&Token::Comma) && !self.eat(&Token::Semicolon) {
            return Err(self.error_here("expected ',' after the file channel"));
        }
        let mut targets = Vec::new();
        loop {
            targets.push(self.expect_name("a variable to read into")?);
            if !self.eat(&Token::Comma) {
                break;
            }
        }
        Ok(Stmt::FileInput { channel, targets })
    }

    fn parse_open(&mut self) -> Result<Stmt, SyntaxError> {
        let filename = self.parse_expr()?;
        self.expect_ident_word("for")?;
        let mode_name = self.expect_name("a file mode (Input, Output or Append)")?;
        let mode = match mode_name.as_str() {
            "input" => FileMode::Input,
            "output" => FileMode::Output,
            "append" => FileMode::Append,
            other => {
                return Err(self.error_here(format!(
                    "unsupported file mode '{other}' (expected Input, Output or Append)"
                )))
            }
        };
        self.expect_ident_word("as")?;
        let channel = self.parse_channel()?;
        Ok(Stmt::FileOpen {
            filename,
            mode,
            channel,
        })
    }

    fn parse_close(&mut self) -> Result<Stmt, SyntaxError> {
        if matches!(self.peek(), Token::Hash) {
            Ok(Stmt::FileClose(self.parse_channel()?))
        } else {
            Ok(Stmt::FileCloseAll)
        }
    }

    fn parse_line_input(&mut self) -> Result<Stmt, SyntaxError> {
        if !self.eat_ident("input") {
            return Err(self.error_here(format!(
                "expected 'Input' after 'Line', found {}",
                self.peek().describe()
            )));
        }
        let channel = self.parse_channel()?;
        self.expect(&Token::Comma, "','")?;
        let target = self.expect_name("a variable to read into")?;
        Ok(Stmt::FileRead { channel, target })
    }

    fn parse_if(&mut self) -> Result<Stmt, SyntaxError> {
        let condition = self.parse_expr()?;
        self.expect_ident_word("then")?;
        if self.at_statement_end() {
            self.parse_block_if(condition)
        } else {
            self.parse_inline_if(condition)
        }
    }

    fn parse_block_if(&mut self, condition: Expr) -> Result<Stmt, SyntaxError> {
        let then_branch = self.parse_block_until(&["elseif", "else", "end"])?;
        let else_branch = if self.eat_ident("elseif") {
            let nested_condition = self.parse_expr()?;
            self.expect_ident_word("then")?;
            // A chained ElseIf compiles down to a nested If in the else branch,
            // which swallows its own `End If`.
            if self.at_statement_end() {
                Some(vec![self.parse_block_if(nested_condition)?])
            } else {
                Some(vec![self.parse_inline_if(nested_condition)?])
            }
        } else if self.eat_ident("else") {
            Some(self.parse_block_until(&["end"])?)
        } else {
            None
        };
        // Only consume the terminator when this frame still owns it.
        if self.check_ident("end") {
            self.advance();
            self.expect_ident_word("if")?;
        }
        Ok(Stmt::If {
            condition,
            then_branch,
            else_branch,
        })
    }

    /// Single-line form: `If a Then b = 1 Else c = 2`.
    fn parse_inline_if(&mut self, condition: Expr) -> Result<Stmt, SyntaxError> {
        let mut then_branch = Vec::new();
        while !self.at_statement_end() && !self.check_ident("else") && !self.check_ident("elseif") {
            then_branch.push(self.parse_statement()?);
            self.eat(&Token::Colon);
        }
        if then_branch.is_empty() {
            return Err(self.error_here("expected a statement after 'Then'"));
        }

        let else_branch = if self.eat_ident("elseif") {
            let nested_condition = self.parse_expr()?;
            self.expect_ident_word("then")?;
            Some(vec![self.parse_inline_if(nested_condition)?])
        } else if self.eat_ident("else") {
            let mut branch = Vec::new();
            while !self.at_statement_end() {
                branch.push(self.parse_statement()?);
                self.eat(&Token::Colon);
            }
            if branch.is_empty() {
                return Err(self.error_here("expected a statement after 'Else'"));
            }
            Some(branch)
        } else {
            None
        };

        Ok(Stmt::If {
            condition,
            then_branch,
            else_branch,
        })
    }

    fn parse_while(&mut self) -> Result<Stmt, SyntaxError> {
        let condition = self.parse_expr()?;
        let body = self.parse_block_until(&["wend"])?;
        self.expect_ident_word("wend")?;
        Ok(Stmt::While { condition, body })
    }

    fn parse_for(&mut self) -> Result<Stmt, SyntaxError> {
        let variable = self.expect_name("a loop variable")?;
        self.expect(&Token::Eq, "'='")?;
        let start = self.parse_expr()?;
        self.expect_ident_word("to")?;
        let end = self.parse_expr()?;
        let step = if self.eat_ident("step") {
            Some(self.parse_expr()?)
        } else {
            None
        };
        let body = self.parse_block_until(&["next"])?;
        self.expect_ident_word("next")?;
        // `Next i` may name the loop variable again.
        if matches!(self.peek(), Token::Ident(_)) {
            self.advance();
        }
        Ok(Stmt::For {
            variable,
            start,
            end,
            step,
            body,
        })
    }

    /// `For Each element In collection` ... `Next [element]`
    fn parse_for_each(&mut self) -> Result<Stmt, SyntaxError> {
        let variable = self.expect_name("a loop variable after 'For Each'")?;
        self.expect_ident_word("in")?;
        let collection = self.parse_expr()?;
        let body = self.parse_block_until(&["next"])?;
        self.expect_ident_word("next")?;
        if matches!(self.peek(), Token::Ident(_)) {
            self.advance();
        }
        Ok(Stmt::ForEach {
            variable,
            collection,
            body,
        })
    }

    /// `ReDim [Preserve] name(size)`
    fn parse_redim(&mut self) -> Result<Stmt, SyntaxError> {
        let preserve = self.eat_ident("preserve");
        let name = self.expect_name("an array name after 'ReDim'")?;
        self.expect(&Token::LParen, "'('")?;
        let size = if self.check(&Token::RParen) {
            Expr::Literal(Variant::Int(0))
        } else {
            self.parse_expr()?
        };
        self.expect(&Token::RParen, "')'")?;
        self.declared_arrays.insert(name.clone());
        Ok(Stmt::ReDim {
            name,
            size,
            preserve,
        })
    }

    fn parse_do_loop(&mut self) -> Result<Stmt, SyntaxError> {
        let pre = self.parse_loop_condition()?;
        let body = self.parse_block_until(&["loop"])?;
        self.expect_ident_word("loop")?;
        let post = self.parse_loop_condition()?;
        Ok(Stmt::DoLoop { pre, post, body })
    }

    /// `While cond` / `Until cond`, or nothing at all.
    fn parse_loop_condition(&mut self) -> Result<Option<LoopCondition>, SyntaxError> {
        if self.eat_ident("while") {
            Ok(Some(LoopCondition {
                until: false,
                condition: self.parse_expr()?,
            }))
        } else if self.eat_ident("until") {
            Ok(Some(LoopCondition {
                until: true,
                condition: self.parse_expr()?,
            }))
        } else {
            Ok(None)
        }
    }

    /// `Select Case subject` ... `End Select`
    fn parse_select_case(&mut self) -> Result<Stmt, SyntaxError> {
        self.expect_ident_word("case")?;
        let subject = self.parse_expr()?;
        let mut arms = Vec::new();
        let mut otherwise = None;
        loop {
            self.skip_separators();
            if self.at_eof() {
                return Err(
                    self.error_here("unexpected end of file: expected 'Case' or 'End Select'")
                );
            }
            if self.check_ident("end") {
                break;
            }
            if !self.eat_ident("case") {
                return Err(self.error_here(format!(
                    "expected 'Case' or 'End Select', found {}",
                    self.peek().describe()
                )));
            }
            if self.eat_ident("else") {
                otherwise = Some(self.parse_block_until(&["case", "end"])?);
            } else {
                let patterns = self.parse_case_patterns()?;
                let body = self.parse_block_until(&["case", "end"])?;
                arms.push(SelectArm { patterns, body });
            }
        }
        self.expect_ident_word("end")?;
        self.expect_ident_word("select")?;
        Ok(Stmt::SelectCase {
            subject,
            arms,
            otherwise,
        })
    }

    fn parse_case_patterns(&mut self) -> Result<Vec<CasePattern>, SyntaxError> {
        let mut patterns = Vec::new();
        loop {
            patterns.push(self.parse_case_pattern()?);
            if !self.eat(&Token::Comma) {
                break;
            }
        }
        Ok(patterns)
    }

    fn parse_case_pattern(&mut self) -> Result<CasePattern, SyntaxError> {
        if self.eat_ident("is") {
            let operator = match self.peek() {
                Token::Eq => Token::Eq,
                Token::Ne => Token::Ne,
                Token::Lt => Token::Lt,
                Token::Gt => Token::Gt,
                Token::Le => Token::Le,
                Token::Ge => Token::Ge,
                other => {
                    return Err(self.error_here(format!(
                        "expected a comparison after 'Case Is', found {}",
                        other.describe()
                    )))
                }
            };
            self.advance();
            let value = self.parse_expr()?;
            return Ok(CasePattern::Comparison { operator, value });
        }
        let first = self.parse_expr()?;
        if self.eat_ident("to") {
            let last = self.parse_expr()?;
            return Ok(CasePattern::Range(first, last));
        }
        Ok(CasePattern::Value(first))
    }

    /// `With obj` ... `End With`
    fn parse_with(&mut self) -> Result<Stmt, SyntaxError> {
        let object = self.expect_name("a variable name after 'With'")?;
        let temp = format!("__with_{}", self.with_counter);
        self.with_counter += 1;
        self.with_stack.push(temp.clone());
        let body = self.parse_block_until(&["end"])?;
        self.expect_ident_word("end")?;
        self.expect_ident_word("with")?;
        self.with_stack.pop();
        Ok(Stmt::With {
            object,
            temp,
            body,
        })
    }

    /// `.Member` inside a `With`, resolved to the captured object.
    fn parse_with_member(&mut self) -> Result<(String, String), SyntaxError> {
        let Some(object) = self.with_stack.last().cloned() else {
            return Err(self.error_here(
                "'.' can only start a member reference inside a With...End With block",
            ));
        };
        self.expect(&Token::Dot, "'.'")?;
        let member = self.expect_name("a member name after '.'")?;
        Ok((object, member))
    }

    /// A statement beginning with `.`: `/.Open "GET", url, False`
    fn parse_with_member_statement(&mut self) -> Result<Stmt, SyntaxError> {
        let (object, method) = self.parse_with_member()?;
        // `.Member = value` inside a `With` block.
        if self.check(&Token::Eq) {
            self.advance();
            let value = self.parse_expr()?;
            return Ok(Stmt::SetMember {
                object,
                member: method,
                value,
            });
        }
        let args = if self.check(&Token::LParen) {
            self.parse_parenthesised_args()?
        } else if self.at_statement_end() {
            Vec::new()
        } else {
            self.parse_bare_args()?
        };
        Ok(Stmt::ExprStatement(Expr::MethodCall {
            object,
            method,
            args,
        }))
    }

    /// `Const Name [As Type] = value`
    fn parse_const(&mut self) -> Result<Stmt, SyntaxError> {
        let name = self.expect_name("a constant name")?;
        self.eat_as_type()?;
        self.expect(&Token::Eq, "'='")?;
        let value = self.parse_expr()?;
        Ok(Stmt::Const { name, value })
    }

    /// `Type Name` ... `End Type`
    fn parse_type(&mut self) -> Result<Stmt, SyntaxError> {
        let name = self.expect_name("a type name after 'Type'")?;
        let mut fields = Vec::new();
        loop {
            self.skip_separators();
            if self.at_eof() {
                return Err(self.error_here("unexpected end of file: expected 'End Type'"));
            }
            if self.check_ident("end") {
                break;
            }
            let field = self.expect_name("a field name")?;
            let size = if self.check(&Token::LParen) {
                self.advance();
                let size = self.parse_expr()?;
                self.expect(&Token::RParen, "')'")?;
                Some(size)
            } else {
                None
            };
            // VB6 required the `As` clause; the type name itself is not enforced.
            self.expect_ident_word("as")?;
            self.expect_name("a type name")?;
            fields.push(TypeField { name: field, size });
            self.expect_statement_end()?;
        }
        self.expect_ident_word("end")?;
        self.expect_ident_word("type")?;
        Ok(Stmt::TypeDeclaration { name, fields })
    }

    /// `Exit Sub|Function|For|While|Do`
    fn parse_exit(&mut self) -> Result<Stmt, SyntaxError> {
        let position = self.pos;
        let word = self.expect_name("Sub, Function, For, While or Do after 'Exit'")?;
        match word.as_str() {
            "sub" => Ok(Stmt::ExitProcedure {
                kind: ProcedureKind::Sub,
            }),
            "function" => Ok(Stmt::ExitProcedure {
                kind: ProcedureKind::Function,
            }),
            "for" => Ok(Stmt::ExitFor),
            "while" => Ok(Stmt::ExitWhile),
            "do" => Ok(Stmt::ExitDo),
            other => Err(self.error_at(
                position,
                format!(
                    "unknown Exit target '{other}' (expected Sub, Function, For, While or Do)"
                ),
            )),
        }
    }

    fn parse_try(&mut self) -> Result<Stmt, SyntaxError> {
        let try_block = self.parse_block_until(&["catch"])?;
        self.expect_ident_word("catch")?;
        let catch_var = self.expect_name("an error variable name")?;
        let catch_block = self.parse_block_until(&["end"])?;
        self.expect_ident_word("end")?;
        self.expect_ident_word("try")?;
        Ok(Stmt::TryCatch {
            try_block,
            catch_var,
            catch_block,
        })
    }

    fn parse_procedure(&mut self, is_function: bool) -> Result<Stmt, SyntaxError> {
        let name = self.expect_name("a procedure name")?;
        let params = if self.check(&Token::LParen) {
            self.parse_params()?
        } else {
            Vec::new()
        };
        let body = self.parse_block_until(&["end"])?;
        self.expect_ident_word("end")?;
        self.expect_ident_word(if is_function { "function" } else { "sub" })?;
        Ok(if is_function {
            Stmt::FuncDeclaration { name, params, body }
        } else {
            Stmt::SubDeclaration { name, params, body }
        })
    }

    fn parse_params(&mut self) -> Result<Vec<String>, SyntaxError> {
        self.expect(&Token::LParen, "'('")?;
        let mut params = Vec::new();
        if !self.check(&Token::RParen) {
            loop {
                // `ByVal`/`ByRef` are accepted and ignored: everything is ByVal.
                let _ = self.eat_ident("byval") || self.eat_ident("byref");
                let name = self.expect_name("a parameter name")?;
                self.eat_as_type()?;
                params.push(name);
                if !self.eat(&Token::Comma) {
                    break;
                }
            }
        }
        self.expect(&Token::RParen, "')'")?;
        Ok(params)
    }

    // ------------------------------------------------------------ expressions

    /// Parses one expression.
    pub fn parse_expr(&mut self) -> Result<Expr, SyntaxError> {
        self.parse_pipeline()
    }

    fn parse_pipeline(&mut self) -> Result<Expr, SyntaxError> {
        let mut left = self.parse_or()?;
        while self.check(&Token::Pipe) {
            self.advance();
            let command = match self.advance() {
                Token::Backtick(cmd) => cmd,
                Token::Str(text) => text,
                other => {
                    return Err(self.error_here(format!(
                        "expected a `command` after '|', found {}",
                        other.describe()
                    )))
                }
            };
            left = Expr::Pipeline {
                input: Box::new(left),
                command,
            };
        }
        Ok(left)
    }

    fn parse_or(&mut self) -> Result<Expr, SyntaxError> {
        let mut left = self.parse_and()?;
        while self.check_ident("or") {
            self.advance();
            let right = self.parse_and()?;
            left = binary(left, Token::Ident("or".to_string()), right);
        }
        Ok(left)
    }

    fn parse_and(&mut self) -> Result<Expr, SyntaxError> {
        let mut left = self.parse_not()?;
        while self.check_ident("and") {
            self.advance();
            let right = self.parse_not()?;
            left = binary(left, Token::Ident("and".to_string()), right);
        }
        Ok(left)
    }

    fn parse_not(&mut self) -> Result<Expr, SyntaxError> {
        if self.eat_ident("not") {
            let right = self.parse_not()?;
            return Ok(Expr::UnaryOp {
                operator: Token::Ident("not".to_string()),
                right: Box::new(right),
            });
        }
        self.parse_comparison()
    }

    fn parse_comparison(&mut self) -> Result<Expr, SyntaxError> {
        let mut left = self.parse_concat()?;
        loop {
            let operator = match self.peek() {
                Token::Eq => Token::Eq,
                Token::Ne => Token::Ne,
                Token::Lt => Token::Lt,
                Token::Gt => Token::Gt,
                Token::Le => Token::Le,
                Token::Ge => Token::Ge,
                _ => break,
            };
            self.advance();
            let right = self.parse_concat()?;
            left = binary(left, operator, right);
        }
        Ok(left)
    }

    fn parse_concat(&mut self) -> Result<Expr, SyntaxError> {
        let mut left = self.parse_additive()?;
        while self.check(&Token::Amp) {
            self.advance();
            let right = self.parse_additive()?;
            left = binary(left, Token::Amp, right);
        }
        Ok(left)
    }

    fn parse_additive(&mut self) -> Result<Expr, SyntaxError> {
        let mut left = self.parse_modulo()?;
        loop {
            let operator = match self.peek() {
                Token::Plus => Token::Plus,
                Token::Minus => Token::Minus,
                _ => break,
            };
            self.advance();
            let right = self.parse_modulo()?;
            left = binary(left, operator, right);
        }
        Ok(left)
    }

    fn parse_modulo(&mut self) -> Result<Expr, SyntaxError> {
        let mut left = self.parse_int_division()?;
        while self.check_ident("mod") {
            self.advance();
            let right = self.parse_int_division()?;
            left = binary(left, Token::Ident("mod".to_string()), right);
        }
        Ok(left)
    }

    fn parse_int_division(&mut self) -> Result<Expr, SyntaxError> {
        let mut left = self.parse_multiplicative()?;
        while self.check(&Token::Backslash) {
            self.advance();
            let right = self.parse_multiplicative()?;
            left = binary(left, Token::Backslash, right);
        }
        Ok(left)
    }

    fn parse_multiplicative(&mut self) -> Result<Expr, SyntaxError> {
        let mut left = self.parse_unary()?;
        loop {
            let operator = match self.peek() {
                Token::Star => Token::Star,
                Token::Slash => Token::Slash,
                _ => break,
            };
            self.advance();
            let right = self.parse_unary()?;
            left = binary(left, operator, right);
        }
        Ok(left)
    }

    fn parse_unary(&mut self) -> Result<Expr, SyntaxError> {
        match self.peek() {
            Token::Minus => {
                self.advance();
                let right = self.parse_unary()?;
                Ok(Expr::UnaryOp {
                    operator: Token::Minus,
                    right: Box::new(right),
                })
            }
            Token::Plus => {
                self.advance();
                self.parse_unary()
            }
            _ => self.parse_power(),
        }
    }

    fn parse_power(&mut self) -> Result<Expr, SyntaxError> {
        let base = self.parse_postfix()?;
        if self.check(&Token::Caret) {
            self.advance();
            // Right associative, and the exponent may be negative.
            let exponent = self.parse_unary()?;
            return Ok(binary(base, Token::Caret, exponent));
        }
        Ok(base)
    }

    /// Function calls `f(x)`, array indexing `a(1)`, member access `o.Member`.
    fn parse_postfix(&mut self) -> Result<Expr, SyntaxError> {
        let mut base = self.parse_primary()?;
        loop {
            if self.check(&Token::Dot) {
                self.advance();
                let member = self.expect_name("a member name after '.'")?;
                let args = if self.check(&Token::LParen) {
                    self.parse_parenthesised_args()?
                } else {
                    Vec::new()
                };
                base = match base {
                    Expr::Variable(name) => Expr::MethodCall {
                        object: name,
                        method: member,
                        args,
                    },
                    // Anything else keeps its receiver as an expression, which
                    // is what lets `points(1).X` and `f().X` resolve.
                    other => Expr::MemberAccess {
                        object: Box::new(other),
                        member,
                        args,
                    },
                };
                continue;
            }

            if self.check(&Token::LParen) {
                let args = self.parse_parenthesised_args()?;
                base = match base {
                    Expr::Variable(name) if self.declared_arrays.contains(&name) => {
                        if args.len() != 1 {
                            return Err(self.error_here(
                                "array indexing takes exactly one subscript",
                            ));
                        }
                        let mut args = args;
                        Expr::ArrayAccess {
                            name,
                            index: Box::new(args.remove(0)),
                        }
                    }
                    Expr::Variable(name) => Expr::FunctionCall { name, args },
                    _ => {
                        return Err(
                            self.error_here("only a name can be called; a receiver uses '.'")
                        )
                    }
                };
                continue;
            }

            break;
        }
        Ok(base)
    }

    fn parse_primary(&mut self) -> Result<Expr, SyntaxError> {
        match self.peek().clone() {
            Token::Int(v) => {
                self.advance();
                Ok(Expr::Literal(Variant::Int(v)))
            }
            Token::Float(v) => {
                self.advance();
                Ok(Expr::Literal(Variant::Float(v)))
            }
            Token::Str(raw) => {
                self.advance();
                Ok(split_interpolated(&raw))
            }
            Token::Backtick(cmd) => {
                self.advance();
                Ok(Expr::ShellCommand(cmd))
            }
            Token::Hash => {
                // `#1` as a value, so that `EOF(#1)` works, and `#f` so that
                // `EOF(#FreeFile())` does too.
                self.advance();
                self.parse_expr()
            }
            Token::LParen => {
                self.advance();
                let inner = self.parse_expr()?;
                self.expect(&Token::RParen, "')'")?;
                Ok(inner)
            }
            Token::Dot => {
                let (object, member) = self.parse_with_member()?;
                let args = if self.check(&Token::LParen) {
                    self.parse_parenthesised_args()?
                } else {
                    Vec::new()
                };
                Ok(Expr::MethodCall {
                    object,
                    method: member,
                    args,
                })
            }
            Token::Ident(name) => {
                self.advance();
                match name.as_str() {
                    "true" => Ok(Expr::Literal(Variant::Bool(true))),
                    "false" => Ok(Expr::Literal(Variant::Bool(false))),
                    // `Nothing` and `Empty` are both "no value"; `Null` is a
                    // distinct one, which is what IsNull detects.
                    "nothing" | "empty" => Ok(Expr::Literal(Variant::Empty)),
                    "null" => Ok(Expr::Literal(Variant::Null)),
                    _ => {
                        if let Some((_, value)) =
                            NAMED_CONSTANTS.iter().find(|(key, _)| *key == name)
                        {
                            return Ok(Expr::Literal(Variant::string(*value)));
                        }
                        Ok(Expr::Variable(name))
                    }
                }
            }
            other => Err(self.error_here(format!(
                "expected an expression, found {}",
                other.describe()
            ))),
        }
    }

    fn parse_parenthesised_args(&mut self) -> Result<Vec<Expr>, SyntaxError> {
        self.expect(&Token::LParen, "'('")?;
        let mut args = Vec::new();
        if !self.check(&Token::RParen) {
            loop {
                args.push(self.parse_expr()?);
                if !self.eat(&Token::Comma) {
                    break;
                }
            }
        }
        self.expect(&Token::RParen, "')'")?;
        Ok(args)
    }

    /// VB6 lets a call statement drop its parentheses: `http.Open "GET", url`.
    fn parse_bare_args(&mut self) -> Result<Vec<Expr>, SyntaxError> {
        let mut args = Vec::new();
        loop {
            args.push(self.parse_expr()?);
            if !self.eat(&Token::Comma) {
                break;
            }
        }
        Ok(args)
    }
}

fn binary(left: Expr, operator: Token, right: Expr) -> Expr {
    Expr::BinaryOp {
        left: Box::new(left),
        operator,
        right: Box::new(right),
    }
}

/// Whether `name` is a standard-library function.  The parser asks so that
/// `Len(...)` reads as a call rather than an array subscript.
pub fn is_builtin(name: &str) -> bool {
    BUILTIN_NAMES.contains(&name)
}

/// Splits a string literal into literal and `$variable` parts.
///
/// `"on $sysName ok"` becomes `[Literal("on "), Variable("sysname"), Literal(" ok")]`.
/// `\$` escapes a literal dollar sign, and `${name}` is accepted too.
pub fn split_interpolated(raw: &str) -> Expr {
    let chars: Vec<char> = raw.chars().collect();
    let mut parts: Vec<Expr> = Vec::new();
    let mut current = String::new();
    let mut i = 0usize;

    let flush = |current: &mut String, parts: &mut Vec<Expr>| {
        if !current.is_empty() {
            parts.push(Expr::Literal(Variant::String(std::mem::take(current))));
        }
    };

    while i < chars.len() {
        let c = chars[i];
        if c == '\\' && i + 1 < chars.len() && chars[i + 1] == '$' {
            current.push('$');
            i += 2;
            continue;
        }
        if c == '$' {
            if i + 1 < chars.len() && chars[i + 1] == '{' {
                if let Some(offset) = chars[i + 2..].iter().position(|ch| *ch == '}') {
                    let name: String = chars[i + 2..i + 2 + offset].iter().collect();
                    if !name.is_empty() {
                        flush(&mut current, &mut parts);
                        parts.push(Expr::Variable(name.to_lowercase()));
                    }
                    i = i + 2 + offset + 1;
                    continue;
                }
            } else if i + 1 < chars.len() && (chars[i + 1].is_alphabetic() || chars[i + 1] == '_') {
                let mut j = i + 1;
                while j < chars.len() && (chars[j].is_alphanumeric() || chars[j] == '_') {
                    j += 1;
                }
                let name: String = chars[i + 1..j].iter().collect();
                flush(&mut current, &mut parts);
                parts.push(Expr::Variable(name.to_lowercase()));
                i = j;
                continue;
            }
        }
        current.push(c);
        i += 1;
    }
    if !current.is_empty() {
        parts.push(Expr::Literal(Variant::String(current)));
    }

    match parts.len() {
        0 => Expr::Literal(Variant::String(String::new())),
        1 => parts.pop().unwrap(),
        _ => Expr::InterpolatedString(parts),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(src: &str) -> Program {
        Parser::parse_source(src).unwrap().0
    }

    /// The statement at `index`, with its source-location wrapper peeled off.
    fn at(program: &Program, index: usize) -> &Stmt {
        peel(&program.statements[index])
    }

    use crate::ast::peel;

    fn parse_err(src: &str) -> SyntaxError {
        Parser::parse_source(src).unwrap_err()
    }

    #[test]
    fn dim_and_let() {
        let program = parse("DIM x\nLet x = 5\n");
        assert_eq!(program.statements.len(), 2);
        assert!(matches!(
            at(&program, 0),
            Stmt::Dim { name, type_name } if name == "x" && type_name.is_none()
        ));
        assert!(matches!(at(&program, 1), Stmt::Let { .. }));
    }

    #[test]
    fn a_dim_can_name_a_record_type() {
        let program = parse("Dim p As Point\nDim n As Integer\n");
        assert!(matches!(
            at(&program, 0),
            Stmt::Dim { type_name: Some(name), .. } if name == "point"
        ));
        assert!(matches!(
            at(&program, 1),
            Stmt::Dim { type_name: Some(name), .. } if name == "integer"
        ));
    }

    #[test]
    fn type_declarations_parse_fields_including_arrays() {
        let program = parse(
            "Type Point\n  X As Double\n  Y As Double\n  Tags(3) As String\nEnd Type\n",
        );
        let Stmt::TypeDeclaration { name, fields } = at(&program, 0) else {
            panic!("expected a TypeDeclaration");
        };
        assert_eq!(name, "point");
        assert_eq!(fields.len(), 3);
        assert_eq!(fields[0].name, "x");
        assert!(fields[0].size.is_none());
        assert!(fields[2].size.is_some());
    }

    #[test]
    fn implicit_assignment_without_let() {
        let program = parse("x = 5\n");
        assert!(matches!(at(&program, 0), Stmt::Let { .. }));
    }

    #[test]
    fn operator_precedence_matches_vb6() {
        // & binds looser than +, so the addition happens first.
        let program = parse("Let s = 1 + 2 & 3\n");
        let Stmt::Let { value, .. } = at(&program, 0) else {
            panic!("expected a Let");
        };
        let Expr::BinaryOp { operator, left, .. } = value else {
            panic!("expected a binary op");
        };
        assert_eq!(*operator, Token::Amp);
        assert!(matches!(
            **left,
            Expr::BinaryOp {
                operator: Token::Plus,
                ..
            }
        ));
    }

    #[test]
    fn if_elseif_else_chain() {
        let program = parse(
            "If a = 1 Then\n  Let x = 1\nElseIf a = 2 Then\n  Let x = 2\nElse\n  Let x = 3\nEnd If\n",
        );
        let Stmt::If {
            then_branch,
            else_branch,
            ..
        } = at(&program, 0)
        else {
            panic!("expected an If");
        };
        assert_eq!(then_branch.len(), 1);
        let nested = else_branch.as_ref().unwrap();
        assert!(matches!(nested[0], Stmt::If { .. }));
    }

    #[test]
    fn inline_if() {
        let program = parse("If a Then Let x = 1 Else Let x = 2\n");
        let Stmt::If { else_branch, .. } = at(&program, 0) else {
            panic!("expected an If");
        };
        assert!(else_branch.is_some());
    }

    #[test]
    fn for_and_while_blocks() {
        let program = parse("For i = 1 To 10 Step 2\n  Debug.Print i\nNext i\n");
        let Stmt::For { variable, step, .. } = at(&program, 0) else {
            panic!("expected a For");
        };
        assert_eq!(variable, "i");
        assert!(step.is_some());

        let program = parse("While i < 3\n  Let i = i + 1\nWend\n");
        assert!(matches!(at(&program, 0), Stmt::While { .. }));
    }

    #[test]
    fn try_catch_blocks() {
        let program = parse("Try\n  x = 1\nCatch Err\n  Debug.Print Err.Message\nEnd Try\n");
        let Stmt::TryCatch { catch_var, .. } = at(&program, 0) else {
            panic!("expected a TryCatch");
        };
        assert_eq!(catch_var, "err");
    }

    #[test]
    fn sub_and_function_declarations() {
        let program = parse("Function Add(a, b)\n  Add = a + b\nEnd Function\n");
        let Stmt::FuncDeclaration { name, params, .. } = at(&program, 0) else {
            panic!("expected a Function");
        };
        assert_eq!(name, "add");
        assert_eq!(params, &vec!["a".to_string(), "b".to_string()]);
    }

    #[test]
    fn file_channel_statements() {
        let program = parse("Open \"f.txt\" For Append As #1\nPrint #1, \"x\"\nClose #1\n");
        assert!(matches!(
            at(&program, 0),
            Stmt::FileOpen {
                mode: FileMode::Append,
                ..
            }
        ));
        assert!(matches!(at(&program, 1), Stmt::FilePrint { .. }));
        assert!(matches!(at(&program, 2), Stmt::FileClose(_)));
    }

    #[test]
    fn a_file_number_may_be_an_expression() {
        // Which is the whole point of `FreeFile()`.
        let program = parse("Dim f\nLet f = FreeFile()\nOpen \"x\" For Output As #f\nClose #f\n");
        let Stmt::FileOpen { channel, .. } = at(&program, 2) else {
            panic!("expected a FileOpen");
        };
        assert!(matches!(channel, Expr::Variable(name) if name == "f"));

        let program = parse("Open \"x\" For Output As #FreeFile()\n");
        let Stmt::FileOpen { channel, .. } = at(&program, 0) else {
            panic!("expected a FileOpen");
        };
        assert!(matches!(channel, Expr::FunctionCall { name, .. } if name == "freefile"));
    }

    #[test]
    fn pipeline_binds_loosest() {
        let program = parse("Let w = http.ResponseText | `tr a-z A-Z`\n");
        let Stmt::Let { value, .. } = at(&program, 0) else {
            panic!("expected a Let");
        };
        let Expr::Pipeline { input, command } = value else {
            panic!("expected a pipeline");
        };
        assert_eq!(command, "tr a-z A-Z");
        assert!(matches!(**input, Expr::MethodCall { .. }));
    }

    #[test]
    fn interpolated_strings_split_on_dollars() {
        let program = parse("Let s = \"hi $name and ${other}!\"\n");
        let Stmt::Let { value, .. } = at(&program, 0) else {
            panic!("expected a Let");
        };
        let Expr::InterpolatedString(parts) = value else {
            panic!("expected interpolation, got {value:?}");
        };
        assert_eq!(parts.len(), 5);
        assert!(matches!(&parts[0], Expr::Literal(Variant::String(s)) if s == "hi "));
        assert!(matches!(&parts[1], Expr::Variable(n) if n == "name"));
        assert!(matches!(&parts[2], Expr::Literal(Variant::String(s)) if s == " and "));
        assert!(matches!(&parts[3], Expr::Variable(n) if n == "other"));
        assert!(matches!(&parts[4], Expr::Literal(Variant::String(s)) if s == "!"));
    }

    #[test]
    fn escaped_dollar_is_literal() {
        let program = parse("Let s = \"cost: \\$5\"\n");
        let Stmt::Let { value, .. } = at(&program, 0) else {
            panic!("expected a Let");
        };
        assert!(matches!(value, Expr::Literal(Variant::String(s)) if s == "cost: $5"));
    }

    #[test]
    fn arrays_are_declared_and_indexed_1_based() {
        let program = parse("Dim buf(3)\nLet buf(1) = \"a\"\nLet s = buf(1)\n");
        assert!(matches!(at(&program, 0), Stmt::DimArray { .. }));
        let Stmt::LetIndexed { index, .. } = at(&program, 1) else {
            panic!("expected an indexed assignment");
        };
        assert!(matches!(index, Expr::Literal(Variant::Int(1))));
        let Stmt::Let { value, .. } = at(&program, 2) else {
            panic!("expected a Let");
        };
        assert!(matches!(value, Expr::ArrayAccess { .. }));
    }

    #[test]
    fn missing_then_is_reported_with_position() {
        let err = parse_err("If a\n");
        assert!(err.message.contains("expected 'then'"), "{err}");
        assert_eq!(err.line, 1);
    }

    #[test]
    fn unterminated_block_is_reported() {
        let err = parse_err("While a < 3\n  Let a = a + 1\n");
        assert!(err.message.contains("unexpected end of file"), "{err}");
        assert!(err.message.contains("wend"), "{err}");
    }

    #[test]
    fn blank_lines_and_comments_are_ignored() {
        let program = parse("' comment\n\n   \nDim x\n");
        assert_eq!(program.statements.len(), 1);
    }

    #[test]
    fn colon_separates_statements() {
        let program = parse("Dim x: Let x = 1: Debug.Print x\n");
        assert_eq!(program.statements.len(), 3);
    }

    #[test]
    fn grouped_and_or_not() {
        let program = parse("If Not (a = 1 And b = 2) Or c Then\n  Let x = 1\nEnd If\n");
        assert!(matches!(at(&program, 0), Stmt::If { .. }));
    }

    #[test]
    fn exit_statements_parse() {
        let program = parse("Exit For\nExit While\nExit Do\nExit Sub\nExit Function\n");
        assert!(matches!(at(&program, 0), Stmt::ExitFor));
        assert!(matches!(at(&program, 1), Stmt::ExitWhile));
        assert!(matches!(at(&program, 2), Stmt::ExitDo));
        assert!(matches!(
            at(&program, 3),
            Stmt::ExitProcedure {
                kind: ProcedureKind::Sub
            }
        ));
        assert!(matches!(
            at(&program, 4),
            Stmt::ExitProcedure {
                kind: ProcedureKind::Function
            }
        ));
    }

    #[test]
    fn an_unknown_exit_target_is_reported() {
        let error = parse_err("Exit sideways\n");
        assert!(error.message.contains("unknown Exit target"), "{error}");
    }

    #[test]
    fn do_loop_condition_placements_parse() {
        let program = parse("Do While a\n  Let a = 1\nLoop Until b\n");
        let Stmt::DoLoop { pre, post, body } = at(&program, 0) else {
            panic!("expected a DoLoop");
        };
        assert!(pre.as_ref().is_some_and(|condition| !condition.until));
        assert!(post.as_ref().is_some_and(|condition| condition.until));
        assert_eq!(body.len(), 1);

        let program = parse("Do\n  Let a = 1\nLoop\n");
        let Stmt::DoLoop { pre, post, .. } = at(&program, 0) else {
            panic!("expected a DoLoop");
        };
        assert!(pre.is_none() && post.is_none());
    }

    #[test]
    fn select_case_patterns_parse() {
        let program = parse(
            "Select Case n\nCase 1, 2\n  Let x = 1\nCase 5 To 9\n  Let x = 2\nCase Is > 20\n  Let x = 3\nCase Else\n  Let x = 4\nEnd Select\n",
        );
        let Stmt::SelectCase {
            arms, otherwise, ..
        } = at(&program, 0)
        else {
            panic!("expected a SelectCase");
        };
        assert_eq!(arms.len(), 3);
        assert_eq!(arms[0].patterns.len(), 2);
        assert!(matches!(arms[1].patterns[0], CasePattern::Range(..)));
        assert!(matches!(
            arms[2].patterns[0],
            CasePattern::Comparison {
                operator: Token::Gt,
                ..
            }
        ));
        assert!(otherwise.is_some());
    }

    #[test]
    fn with_resolves_leading_dots_and_tags_them_to_its_object() {
        let program = parse(
            "With http\n  .Open \"GET\", url, False\n  Debug.Print .ResponseText\nEnd With\n",
        );
        let Stmt::With { object, body, .. } = at(&program, 0) else {
            panic!("expected a With");
        };
        assert_eq!(object, "http");
        assert_eq!(body.len(), 2);

        let Stmt::ExprStatement(Expr::MethodCall { object, method, .. }) = peel(&body[0]) else {
            panic!("expected a method call");
        };
        assert_eq!(method, "open");
        // Resolved to the synthetic capture, not to the name `http`.
        assert_ne!(object, "http");
    }

    #[test]
    fn a_dot_outside_a_with_block_is_reported() {
        let error = parse_err(".Open\n");
        assert!(error.message.contains("With...End With"), "{error}");
    }

    #[test]
    fn nested_with_blocks_resolve_to_the_innermost() {
        let program = parse("With a\n  With b\n    .Stop\n  End With\nEnd With\n");
        let Stmt::With { body, .. } = at(&program, 0) else {
            panic!("expected a With");
        };
        let Stmt::With { body: inner, .. } = peel(&body[0]) else {
            panic!("expected a nested With");
        };
        let Stmt::ExprStatement(Expr::MethodCall { object, .. }) = peel(&inner[0]) else {
            panic!("expected a method call");
        };
        // The outer block captured `__with_0`, the inner `__with_1`.
        assert!(object.ends_with('1'), "resolved to {object}");
    }

    #[test]
    fn const_declaration_parses() {
        let program = parse("Const Limit As Integer = 10\n");
        assert!(matches!(at(&program, 0), Stmt::Const { .. }));
    }

    #[test]
    fn iif_is_an_ordinary_call_at_parse_time() {
        let program = parse("Let r = IIf(a, b, c)\n");
        let Stmt::Let { value, .. } = at(&program, 0) else {
            panic!("expected a Let");
        };
        assert!(matches!(value, Expr::FunctionCall { name, .. } if name == "iif"));
    }
}
