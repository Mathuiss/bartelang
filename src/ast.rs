//! Abstract syntax tree for Bartelang.
//!
//! The node set is the one given in the implementation specification, plus a
//! small number of additions marked `extension` that the spec's own grammar
//! implies but does not spell out (arrays, reading from a file channel,
//! `Close` with no handle).

use crate::lexer::Token;
use crate::value::Variant;

/// One expression of the language.  Everything evaluates to a [`Variant`].
#[derive(Clone, Debug)]
pub enum Expr {
    /// A constant: number, string, boolean, `Empty` or `Null`.
    Literal(Variant),
    /// A bare name, resolved by the interpreter.
    Variable(String),
    /// `"User $name logged in"` - a string broken into literal and variable parts.
    InterpolatedString(Vec<Expr>),
    /// A binary operator, including `And`, `Or` and `Mod` (which arrive as
    /// keyword tokens).
    BinaryOp {
        left: Box<Expr>,
        operator: Token,
        right: Box<Expr>,
    },
    /// A prefix `-`, `+` or `Not`.
    UnaryOp { operator: Token, right: Box<Expr> },
    /// The content of a `` `...` `` command substitution, or of the command on
    /// the right of a pipe.
    ShellCommand(CommandText),
    /// `expr | \`cmd\`` - pipe a Bartelang value into a shell's stdin.
    Pipeline {
        input: Box<Expr>,
        command: CommandText,
    },
    /// `Name(args)` - a built-in, a user procedure, or a subscript when the
    /// name turns out to be an array.
    FunctionCall { name: String, args: Vec<Expr> },
    /// `http.Open "GET", url, False` / `http.ResponseText`
    MethodCall {
        object: String,
        method: String,
        args: Vec<Expr>,
    },
    /// `buffer(1)` - strictly 1-based.
    ArrayAccess { name: String, index: Box<Expr> },
    /// `receiver.Member`, where the receiver is not a plain variable name, so
    /// that `points(1).X` and `MakePoint().X` resolve.  Chained member access
    /// nests through here.
    MemberAccess {
        object: Box<Expr>,
        member: String,
        args: Vec<Expr>,
    },
}

/// The text of a command handed to `sh -c`.
///
/// Plain backticks are [`CommandText::Literal`]: the text reaches the shell
/// exactly as typed, `$` and all.  Writing a `$` immediately before the
/// backtick opts that command into string interpolation, so `$name`, `${name}`
/// and `\$` mean what they mean inside a double-quoted string - which also
/// means a shell variable needs `\$` to get through untouched.
#[derive(Clone, Debug)]
pub enum CommandText {
    /// `` `cmd` `` - handed to the shell verbatim.
    Literal(String),
    /// `` $`cmd $var` `` - the parts are evaluated and concatenated first.
    Interpolated(Vec<Expr>),
}

/// How a file channel was opened.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum FileMode {
    Input,
    Output,
    Append,
}

/// Which procedure an `Exit` leaves.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ProcedureKind {
    Sub,
    Function,
}

impl ProcedureKind {
    /// The spelling used to declare this kind: `Sub` or `Function`.
    pub fn name(self) -> &'static str {
        match self {
            ProcedureKind::Sub => "Sub",
            ProcedureKind::Function => "Function",
        }
    }
}

/// One pattern of a `Select Case` arm.
#[derive(Clone, Debug)]
pub enum CasePattern {
    /// `Case 5`
    Value(Expr),
    /// `Case 1 To 5`
    Range(Expr, Expr),
    /// `Case Is > 5`
    Comparison { operator: Token, value: Expr },
}

/// A `Case ...` arm: one or more patterns sharing one body.
#[derive(Clone, Debug)]
pub struct SelectArm {
    pub patterns: Vec<CasePattern>,
    pub body: Vec<Stmt>,
}

/// The condition of a `Do` loop, written before or after the body.
#[derive(Clone, Debug)]
pub struct LoopCondition {
    /// `Until cond` rather than `While cond`.
    pub until: bool,
    pub condition: Expr,
}

/// How a printed value is followed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Separator {
    /// `;` - the next value follows immediately.
    None,
    /// `,` - advance to the next print zone.
    Zone,
}

/// One value in a `Print` list, with whatever followed it.
#[derive(Clone, Debug)]
pub struct PrintItem {
    pub value: Expr,
    pub after: Separator,
}

/// One field of a `Type` declaration.
#[derive(Clone, Debug)]
pub struct TypeField {
    pub name: String,
    /// The fixed length of an array field, when it has one.
    pub size: Option<Expr>,
}

/// The statement behind any source-location wrapper.
pub fn peel(stmt: &Stmt) -> &Stmt {
    match stmt {
        Stmt::Located { inner, .. } => peel(inner),
        other => other,
    }
}

/// One statement of the language.
#[derive(Clone, Debug)]
pub enum Stmt {
    /// A statement, tagged with the source *unit* (file) and line it began on.
    /// The parser wraps every statement, and the interpreter attaches the
    /// innermost location to any error that escapes - which is how runtime
    /// errors report a file and a line at all.  Unit 0 is the entry file.
    Located {
        unit: usize,
        line: usize,
        inner: Box<Stmt>,
    },
    /// extension: `Include "path.btm"` - the declarations of another file
    /// become part of this program, hoisted exactly as if they had been typed
    /// here.
    ///
    /// The parser only accepts it at the top level of a file, and the path must
    /// be a string literal, because it is resolved once at load time by the
    /// loader, before anything runs.  A `Stmt::Include` must never reach the
    /// interpreter: [`crate::loader::Loader`] expands every one of them.
    Include {
        /// The path exactly as written in the source.
        path: String,
    },
    /// `Dim name [As Type]`
    Dim {
        name: String,
        /// The `As <Type>` name.  It only matters when it names a record type
        /// declared with `Type ... End Type`.
        type_name: Option<String>,
    },
    /// extension: `Dim buffer(10)`, optionally `As <RecordType>`
    DimArray {
        name: String,
        size: Expr,
        /// When this names a record type, every slot starts as a fresh record.
        type_name: Option<String>,
    },
    Let {
        target: String,
        value: Expr,
    },
    /// extension: `Let buffer(1) = "x"` - write to an array slot.
    LetIndexed {
        name: String,
        index: Expr,
        value: Expr,
    },
    Set {
        target: String,
        value: Expr,
    },
    If {
        condition: Expr,
        then_branch: Vec<Stmt>,
        else_branch: Option<Vec<Stmt>>,
    },
    While {
        condition: Expr,
        body: Vec<Stmt>,
    },
    For {
        variable: String,
        start: Expr,
        end: Expr,
        step: Option<Expr>,
        body: Vec<Stmt>,
    },
    /// `Do ... Loop`, with an optional condition before and/or after the body.
    DoLoop {
        pre: Option<LoopCondition>,
        post: Option<LoopCondition>,
        body: Vec<Stmt>,
    },
    /// `Select Case` - the subject is evaluated once and the first matching arm
    /// runs.  There is no fall-through.
    SelectCase {
        subject: Expr,
        arms: Vec<SelectArm>,
        otherwise: Option<Vec<Stmt>>,
    },
    /// `With obj` - a leading `.` inside the body refers to `obj`.
    With {
        object: String,
        /// Synthetic name holding the reference captured when the block was
        /// entered, so reassigning `object` inside cannot change what `.x`
        /// means - which is how VB6 behaved.
        temp: String,
        body: Vec<Stmt>,
    },
    /// `obj.Member = value`
    SetMember {
        object: String,
        member: String,
        value: Expr,
    },
    /// `Const name = value`
    Const {
        name: String,
        value: Expr,
    },
    /// `Type Name` ... `End Type`
    TypeDeclaration {
        name: String,
        fields: Vec<TypeField>,
    },
    /// `For Each element In collection`
    ForEach {
        variable: String,
        collection: Expr,
        body: Vec<Stmt>,
    },
    /// `ReDim [Preserve] name(size)` - resizes an existing array in place, so
    /// every holder of it sees the change.
    ReDim {
        name: String,
        size: Expr,
        preserve: bool,
    },
    /// `Exit Sub` / `Exit Function`
    ExitProcedure {
        kind: ProcedureKind,
    },
    /// `Exit For`
    ExitFor,
    /// `Exit While`
    ExitWhile,
    /// `Exit Do`
    ExitDo,
    TryCatch {
        try_block: Vec<Stmt>,
        catch_var: String,
        catch_block: Vec<Stmt>,
    },
    SubDeclaration {
        name: String,
        params: Vec<String>,
        body: Vec<Stmt>,
    },
    FuncDeclaration {
        name: String,
        params: Vec<String>,
        body: Vec<Stmt>,
    },
    ExprStatement(Expr),

    // Built-in I/O
    DebugPrint {
        items: Vec<PrintItem>,
        /// False when the statement ended with `;`, which suppresses the line
        /// ending.
        newline: bool,
    },
    FileOpen {
        filename: Expr,
        mode: FileMode,
        channel: Expr,
    },
    FilePrint {
        channel: Expr,
        items: Vec<PrintItem>,
        newline: bool,
    },
    /// `Write #1, a, b` - quoted, comma-separated values, one record per line.
    FileWrite {
        channel: Expr,
        values: Vec<Expr>,
    },
    /// `Input #1, a, b` - parses what `Write #1` produced.
    FileInput {
        channel: Expr,
        targets: Vec<String>,
    },
    FileClose(Expr),
    /// extension: `Close` closing every open channel.
    FileCloseAll,
    /// `Line Input #n, target` - read one line from a channel.  The channel is
    /// an expression, as it was in VB6, so `FreeFile()` can supply it.
    FileRead {
        channel: Expr,
        target: String,
    },
}

/// A parsed program: the statements to run, in source order.
#[derive(Clone, Debug)]
pub struct Program {
    pub statements: Vec<Stmt>,
}
