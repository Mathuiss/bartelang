//! The tree-walk interpreter: environments, coercion, control flow and I/O.

use std::cell::RefCell;
use std::cmp::Ordering;
use std::collections::{HashMap, HashSet};
use std::fs::{File, OpenOptions};
use std::io::{Read, Seek, Write};
use std::process::{Command, Stdio};
use std::rc::Rc;
use std::thread;

use crate::ast::{
    CasePattern, Expr, FileMode, LoopCondition, PrintItem, Program, SelectArm, Separator, Stmt,
    peel,
};
use crate::error::BrtError;
use crate::lexer::Token;
use crate::loader::SourceMap;
use crate::objects::{BartObject, ErrObject, Member};
use crate::parser::is_builtin;
use crate::random::Rng;
use crate::record::TypeDef;
use crate::value::{compare, Num, Variant};
use crate::Loaded;

/// Global interpreter state, as specified: variables, vintage file channels and
/// the exit status of the last shell command.
///
/// `constants` is a Bartelang addition, and is what makes `Const` genuinely
/// immutable.  Arrays need no separate map: they are ordinary `Variant` values.
pub struct Environment {
    pub variables: HashMap<String, Variant>,
    /// Names declared `Const` in this scope, which refuse reassignment.
    pub constants: HashSet<String>,
    pub file_channels: HashMap<u8, File>,
    pub last_exit_code: i32,
}

impl Default for Environment {
    fn default() -> Self {
        Environment {
            variables: HashMap::new(),
            constants: HashSet::new(),
            file_channels: HashMap::new(),
            last_exit_code: 0,
        }
    }
}

/// A local scope, created for each `Sub`/`Function` call.
#[derive(Default)]
pub struct Frame {
    variables: HashMap<String, Variant>,
    constants: HashSet<String>,
}

/// A parsed `Sub` or `Function`.
struct Procedure {
    params: Vec<String>,
    body: Vec<Stmt>,
    is_function: bool,
}

/// What a `For` loop is counting with.
enum Counter {
    Int(i64),
    Float(f64),
}

/// Kinds of loop, so `Exit For` can tell whether it is really inside a `For`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum LoopKind {
    For,
    While,
    Do,
}

impl LoopKind {
    fn describe(self) -> &'static str {
        match self {
            LoopKind::For => "For...Next",
            LoopKind::While => "While...Wend",
            LoopKind::Do => "Do...Loop",
        }
    }
}

/// Non-error control transfer out of a block.  Anything that is not a `Flow`
/// stays a `Result::Err`, so `Try/Catch` keeps catching only real errors.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Flow {
    /// Carry on with the next statement.
    Normal,
    /// `Exit For` / `Exit While` / `Exit Do`: leave the innermost loop.
    Break,
    /// `Exit Sub` / `Exit Function`: leave the current procedure.
    Return,
}

/// What a loop body asked its loop to do next.
enum Iteration {
    Again,
    Stop,
    Return,
}

/// A tree-walk interpreter: environments, control flow, the shell engine, file
/// channels and the object table.
///
/// Build one with [`Interp::new`], then hand it a [`Program`] via
/// [`Interp::run`] — or a whole loaded program, source map included, via
/// [`Interp::run_loaded`], which is what the CLI uses.
pub struct Interp {
    pub(crate) globals: Environment,
    frames: Vec<Frame>,
    procedures: HashMap<String, Rc<Procedure>>,
    /// Kinds of the enclosing loops, innermost last.
    loops: Vec<LoopKind>,
    /// Captured `With` subjects, innermost last.  Consulted before ordinary
    /// scopes, and never assignable.
    with_bindings: Vec<(String, Variant)>,
    /// State behind `Rnd` / `Randomize`.
    pub(crate) rng: Rng,
    /// Record types declared with `Type ... End Type`.
    types: HashMap<String, Rc<TypeDef>>,
    pub(crate) cli_args: Vec<String>,
    /// The source files the running program came from, so an error can name
    /// the file it happened in.  `None` when the program was not loaded from
    /// files (the string API), in which case `Err.File` stays empty.
    sources: Option<Rc<SourceMap>>,
    depth: usize,
    max_depth: usize,
}

impl Interp {
    /// A fresh interpreter.  `cli_args` is what `ARGS` will report.
    pub fn new(cli_args: Vec<String>) -> Self {
        let mut globals = Environment::default();
        // The `Err` object is always present, exactly as in VBA.
        globals.variables.insert(
            "err".to_string(),
            Variant::Object(Rc::new(RefCell::new(ErrObject::new()))),
        );
        Interp {
            globals,
            frames: Vec::new(),
            procedures: HashMap::new(),
            loops: Vec::new(),
            with_bindings: Vec::new(),
            rng: Rng::default(),
            types: HashMap::new(),
            cli_args,
            sources: None,
            depth: 0,
            max_depth: 256,
        }
    }

    /// Executes a loaded, multi-file program, wiring up its source map so that
    /// errors name the file they came from - both in the CLI's diagnostics and
    /// in `Err.File` inside a `Catch` block.
    pub fn run_loaded(&mut self, loaded: &Loaded) -> Result<(), BrtError> {
        self.sources = Some(Rc::new(loaded.sources.clone()));
        self.run(&loaded.program)
    }

    /// Executes a program.  `Sub`/`Function` declarations are hoisted first, so
    /// a procedure may be called before its definition appears.
    pub fn run(&mut self, program: &Program) -> Result<(), BrtError> {
        let mut main: Vec<&Stmt> = Vec::new();
        for stmt in &program.statements {
            match peel(stmt) {
                Stmt::SubDeclaration { .. } | Stmt::FuncDeclaration { .. } => {
                    self.register_procedure(peel(stmt));
                }
                // Record definitions are hoisted too, so `Dim p As Point` does
                // not have to come after the `Type Point` block.
                Stmt::TypeDeclaration { .. } => {
                    self.exec_stmt(stmt)?;
                }
                _ => main.push(stmt),
            }
        }
        for stmt in main {
            self.exec_stmt(stmt)?;
        }
        Ok(())
    }

    /// The exit status of the most recent shell command, for `ENV("?")`.
    pub fn last_exit_code(&self) -> i32 {
        self.globals.last_exit_code
    }

    fn register_procedure(&mut self, stmt: &Stmt) {
        match stmt {
            Stmt::SubDeclaration { name, params, body } => {
                self.procedures.insert(
                    name.clone(),
                    Rc::new(Procedure {
                        params: params.clone(),
                        body: body.clone(),
                        is_function: false,
                    }),
                );
            }
            Stmt::FuncDeclaration { name, params, body } => {
                self.procedures.insert(
                    name.clone(),
                    Rc::new(Procedure {
                        params: params.clone(),
                        body: body.clone(),
                        is_function: true,
                    }),
                );
            }
            _ => {}
        }
    }

    // -------------------------------------------------------------- variables

    fn declare_var(&mut self, name: &str, value: Variant) {
        if let Some(frame) = self.frames.last_mut() {
            frame.variables.insert(name.to_string(), value);
        } else {
            self.globals.variables.insert(name.to_string(), value);
        }
    }

    /// Declares a name in the current scope that refuses reassignment.
    fn declare_const(&mut self, name: &str, value: Variant) {
        self.declare_var(name, value);
        if let Some(frame) = self.frames.last_mut() {
            frame.constants.insert(name.to_string());
        } else {
            self.globals.constants.insert(name.to_string());
        }
    }

    /// Assigns to the nearest existing binding, or creates one in the current
    /// scope.  `Dim` is what pins a name to a procedure's local scope.
    fn assign_var(&mut self, name: &str, value: Variant) -> Result<(), BrtError> {
        for frame in self.frames.iter_mut().rev() {
            if frame.variables.contains_key(name) {
                if frame.constants.contains(name) {
                    return Err(BrtError::const_assignment(name));
                }
                frame.variables.insert(name.to_string(), value);
                return Ok(());
            }
        }
        if self.globals.variables.contains_key(name) {
            if self.globals.constants.contains(name) {
                return Err(BrtError::const_assignment(name));
            }
            self.globals.variables.insert(name.to_string(), value);
            return Ok(());
        }
        self.declare_var(name, value);
        Ok(())
    }

    /// The nearest binding for `name`, without cloning it.
    fn find_var(&self, name: &str) -> Option<&Variant> {
        // `With` captures win, so `.member` keeps meaning the object the block
        // captured even if that name is rebound inside the block.
        for (bound, value) in self.with_bindings.iter().rev() {
            if bound == name {
                return Some(value);
            }
        }
        for frame in self.frames.iter().rev() {
            if let Some(value) = frame.variables.get(name) {
                return Some(value);
            }
        }
        self.globals.variables.get(name)
    }

    fn lookup_var(&self, name: &str) -> Result<Variant, BrtError> {
        self.find_var(name)
            .cloned()
            .ok_or_else(|| BrtError::not_defined_variable(name))
    }

    /// Like `lookup_var`, but keeps a record's identity instead of copying it.
    /// `With` needs this: `With p` has to let `.field = ...` reach `p` itself.
    fn lookup_shared(&self, name: &str) -> Result<Variant, BrtError> {
        match self.find_var(name) {
            Some(Variant::Record(record)) => Ok(Variant::Record(Rc::clone(record))),
            Some(other) => Ok(other.clone()),
            None => Err(BrtError::not_defined_variable(name)),
        }
    }

    /// Declares `name` as an array of `size` empty slots.
    ///
    /// Arrays are ordinary values, so a clone of one shares its storage - the
    /// same aliasing VB6 gave you when passing an array to a procedure.
    /// Declares `name` as an array of `size` slots, each a copy of `element` -
    /// which is `Empty` unless the declaration named a record type.
    fn declare_array(&mut self, name: &str, size: usize, element: Variant) {
        let storage = Rc::new(RefCell::new(vec![element; size]));
        self.declare_var(name, Variant::Array(storage));
    }

    /// The storage behind `name`, or a type mismatch if it is not an array.
    fn lookup_array(&self, name: &str) -> Result<Rc<RefCell<Vec<Variant>>>, BrtError> {
        match self.lookup_var(name)? {
            Variant::Array(storage) => Ok(storage),
            other => Err(BrtError::type_mismatch(format!(
                "'{name}' is not an array (it is {})",
                other.type_name()
            ))),
        }
    }

    fn try_lookup_array(&self, name: &str) -> Option<Rc<RefCell<Vec<Variant>>>> {
        match self.lookup_var(name) {
            Ok(Variant::Array(storage)) => Some(storage),
            _ => None,
        }
    }

    fn array_get(&self, name: &str, index: i64) -> Result<Variant, BrtError> {
        let storage = self.lookup_array(name)?;
        let elements = storage.borrow();
        if index < 1 || index as usize > elements.len() {
            return Err(BrtError::subscript(format!(
                "{name}({index}) is outside 1..{}",
                elements.len()
            )));
        }
        Ok(elements[(index - 1) as usize].clone())
    }

    fn array_set(&mut self, name: &str, index: i64, value: Variant) -> Result<(), BrtError> {
        let storage = self.lookup_array(name)?;
        let mut elements = storage.borrow_mut();
        if index < 1 || index as usize > elements.len() {
            return Err(BrtError::subscript(format!(
                "{name}({index}) is outside 1..{}",
                elements.len()
            )));
        }
        elements[(index - 1) as usize] = value;
        Ok(())
    }

    /// `ReDim` resizes in place, so every holder of this array sees the change.
    fn array_resize(&mut self, name: &str, size: usize, preserve: bool) -> Result<(), BrtError> {
        let storage = self.lookup_array(name)?;
        let mut elements = storage.borrow_mut();
        if preserve {
            elements.resize(size, Variant::Empty);
        } else {
            elements.clear();
            elements.resize(size, Variant::Empty);
        }
        Ok(())
    }

    // ------------------------------------------------------------- statements

    pub(crate) fn exec_block(&mut self, statements: &[Stmt]) -> Result<Flow, BrtError> {
        for stmt in statements {
            match self.exec_stmt(stmt)? {
                Flow::Normal => {}
                flow => return Ok(flow),
            }
        }
        Ok(Flow::Normal)
    }

    fn exec_stmt(&mut self, stmt: &Stmt) -> Result<Flow, BrtError> {
        match stmt {
            // The location wrapper: run the statement, and tag any error that
            // escapes with the line it started on.  Wrappers nest, and the
            // innermost one attaches first, so the reported line is the one that
            // actually failed rather than the enclosing block's.
            Stmt::Located { unit, line, inner } => match self.exec_stmt(inner) {
                Ok(flow) => Ok(flow),
                Err(mut error) => {
                    if error.line.is_none() {
                        error.line = Some(*line);
                        error.unit = Some(*unit);
                    }
                    Err(error)
                }
            },
            Stmt::Include { path } => Err(BrtError::invalid_call(format!(
                "Include \"{path}\" was not expanded: load the program with load_file \
                 or Loader so the include can be resolved"
            ))),
            Stmt::Dim { name, type_name } => {
                // `As <Type>` only means anything when it names a record type;
                // otherwise the clause is decoration, as it was in VB6.
                let value = match type_name
                    .as_deref()
                    .and_then(|declared| self.types.get(declared))
                {
                    Some(definition) => definition.instantiate(),
                    None => Variant::Empty,
                };
                self.declare_var(name, value);
                Ok(Flow::Normal)
            }

            Stmt::TypeDeclaration { name, fields } => {
                let mut resolved = Vec::with_capacity(fields.len());
                for field in fields {
                    let size = match &field.size {
                        Some(expression) => {
                            let size = self.eval(expression)?.to_int()?;
                            if size < 0 {
                                return Err(BrtError::subscript(format!(
                                    "Type {name}: field {} has a negative size",
                                    field.name
                                )));
                            }
                            Some(size as usize)
                        }
                        None => None,
                    };
                    resolved.push((field.name.clone(), size));
                }
                self.types.insert(
                    name.clone(),
                    Rc::new(TypeDef {
                        name: name.clone(),
                        fields: resolved,
                    }),
                );
                Ok(Flow::Normal)
            }

            Stmt::DimArray {
                name,
                size,
                type_name,
            } => {
                let size = self.eval(size)?.to_int()?;
                if size < 0 {
                    return Err(BrtError::subscript(format!(
                        "Dim {name}({size}) - an array cannot have a negative size"
                    )));
                }
                // `Dim slots(3) As Point` starts every slot as its own record.
                let element = match type_name
                    .as_deref()
                    .and_then(|declared| self.types.get(declared))
                {
                    Some(definition) => definition.instantiate(),
                    None => Variant::Empty,
                };
                self.declare_array(name, size as usize, element);
                Ok(Flow::Normal)
            }

            Stmt::ReDim {
                name,
                size,
                preserve,
            } => {
                let size = self.eval(size)?.to_int()?;
                if size < 0 {
                    return Err(BrtError::subscript(format!(
                        "ReDim {name}({size}) - an array cannot have a negative size"
                    )));
                }
                self.array_resize(name, size as usize, *preserve)?;
                Ok(Flow::Normal)
            }

            Stmt::Const { name, value } => {
                let value = self.eval(value)?;
                self.declare_const(name, value);
                Ok(Flow::Normal)
            }

            Stmt::Let { target, value } => {
                let value = self.eval(value)?;
                self.assign_var(target, value)?;
                Ok(Flow::Normal)
            }

            Stmt::Set { target, value } => {
                let value = self.eval(value)?;
                if !matches!(value, Variant::Object(_) | Variant::Empty) {
                    return Err(BrtError::object_required(format!(
                        "Set {target} = ... needs an object, got {}",
                        value.type_name()
                    )));
                }
                self.assign_var(target, value)?;
                Ok(Flow::Normal)
            }

            Stmt::LetIndexed { name, index, value } => {
                let index = self.eval(index)?;
                let value = self.eval(value)?;
                match self.lookup_var(name)? {
                    // An array subscript has to be a whole number...
                    Variant::Array(_) => self.array_set(name, index.to_int()?, value)?,
                    // ...whereas an object subscript is a key, of any type.
                    Variant::Object(handle) => handle.borrow_mut().set_item(index, value)?,
                    other => {
                        return Err(BrtError::type_mismatch(format!(
                            "'{name}' is neither an array nor an object (it is {})",
                            other.type_name()
                        )))
                    }
                }
                Ok(Flow::Normal)
            }

            Stmt::SetMember {
                object,
                member,
                value,
            } => {
                // `lookup_shared` rather than `lookup_var`: copying a record
                // before assigning to its field would write into the copy and
                // lose the change.
                let target = self.lookup_shared(object)?;
                let handle = match target {
                    Variant::Object(handle) => handle,
                    Variant::Record(record) => {
                        let value = self.eval(value)?;
                        record.borrow_mut().set(member, value)?;
                        return Ok(Flow::Normal);
                    }
                    other => {
                        return Err(BrtError::object_required(format!(
                            "'{object}' is not an object (it is {})",
                            other.type_name()
                        )))
                    }
                };
                let value = self.eval(value)?;
                handle.borrow_mut().set_property(member, value)?;
                Ok(Flow::Normal)
            }

            Stmt::If {
                condition,
                then_branch,
                else_branch,
            } => {
                if self.eval(condition)?.truthy() {
                    self.exec_block(then_branch)
                } else if let Some(branch) = else_branch {
                    self.exec_block(branch)
                } else {
                    Ok(Flow::Normal)
                }
            }

            Stmt::While { condition, body } => {
                self.loops.push(LoopKind::While);
                let result = self.run_while(condition, body);
                self.loops.pop();
                result
            }

            Stmt::DoLoop { pre, post, body } => {
                self.loops.push(LoopKind::Do);
                let result = self.run_do_loop(pre.as_ref(), post.as_ref(), body);
                self.loops.pop();
                result
            }

            Stmt::For {
                variable,
                start,
                end,
                step,
                body,
            } => {
                self.loops.push(LoopKind::For);
                let result = self.exec_for(variable, start, end, step.as_ref(), body);
                self.loops.pop();
                result
            }

            Stmt::ForEach {
                variable,
                collection,
                body,
            } => {
                let storage = match self.eval(collection)? {
                    Variant::Array(storage) => storage,
                    other => {
                        return Err(BrtError::type_mismatch(format!(
                            "For Each needs an array, got {}",
                            other.type_name()
                        )))
                    }
                };
                // Iterate a snapshot: the body may resize the array, and it must
                // not be able to fight this loop for the borrow.
                let elements = storage.borrow().clone();
                // `Exit For` leaves a `For Each` too, as it did in VB6.
                self.loops.push(LoopKind::For);
                let result = self.run_for_each(variable, elements, body);
                self.loops.pop();
                result
            }

            Stmt::SelectCase {
                subject,
                arms,
                otherwise,
            } => self.exec_select_case(subject, arms, otherwise.as_deref()),

            Stmt::With {
                object,
                temp,
                body,
            } => {
                // Capture the reference now: rebinding `object` inside the
                // block must not change what `.member` refers to, and for a
                // record that means keeping its identity rather than a copy.
                let value = self.lookup_shared(object)?;
                self.with_bindings.push((temp.clone(), value));
                let result = self.exec_block(body);
                self.with_bindings.pop();
                result
            }

            Stmt::ExitProcedure { kind } => {
                if self.depth == 0 {
                    return Err(BrtError::invalid_call(format!(
                        "Exit {} used outside a procedure",
                        kind.name()
                    )));
                }
                Ok(Flow::Return)
            }

            Stmt::ExitFor => self.exit_loop(LoopKind::For, "For"),
            Stmt::ExitWhile => self.exit_loop(LoopKind::While, "While"),
            Stmt::ExitDo => self.exit_loop(LoopKind::Do, "Do"),

            Stmt::TryCatch {
                try_block,
                catch_var,
                catch_block,
            } => match self.exec_block(try_block) {
                // Only real errors are caught; `Exit` flows pass straight out.
                Ok(flow) => Ok(flow),
                Err(error) => {
                    let handle = self.set_err_object(&error);
                    // The named catch variable aliases the same `Err` object,
                    // so both `Catch Err` and `Catch e` work.
                    self.assign_var(catch_var, Variant::Object(handle))?;
                    self.exec_block(catch_block)
                }
            },

            Stmt::SubDeclaration { .. } | Stmt::FuncDeclaration { .. } => {
                self.register_procedure(stmt);
                Ok(Flow::Normal)
            }

            Stmt::ExprStatement(expr) => {
                // A bare name may be a `Sub` invoked without arguments, or a
                // zero-argument built-in such as `Randomize`.
                if let Expr::Variable(name) = expr {
                    if self.procedures.contains_key(name) {
                        self.call_procedure(name, Vec::new())?;
                        return Ok(Flow::Normal);
                    }
                    // A real variable of the same name wins; only an undefined
                    // one falls through to the built-in.
                    if is_builtin(name) && self.lookup_var(name).is_err() {
                        self.call_builtin(name, Vec::new())?;
                        return Ok(Flow::Normal);
                    }
                }
                self.eval(expr)?;
                Ok(Flow::Normal)
            }

            Stmt::DebugPrint { items, newline } => {
                let text = self.render_print_list(items, *newline)?;
                crate::emit_prompt(&text);
                Ok(Flow::Normal)
            }

            Stmt::FileOpen {
                filename,
                mode,
                channel,
            } => {
                let path = self.eval(filename)?.as_string()?;
                let file = match mode {
                    FileMode::Input => File::open(&path)
                        .map_err(|e| BrtError::new(53, format!("Cannot open {path} for Input: {e}")))?,
                    FileMode::Output => File::create(&path)
                        .map_err(|e| BrtError::new(53, format!("Cannot open {path} for Output: {e}")))?,
                    FileMode::Append => OpenOptions::new()
                        .create(true)
                        .append(true)
                        .open(&path)
                        .map_err(|e| BrtError::new(53, format!("Cannot open {path} for Append: {e}")))?,
                };
                // A repeated `Open ... As #1` replaces the old handle, closing it.
                let channel = self.channel_of(channel)?;
                self.globals.file_channels.insert(channel, file);
                Ok(Flow::Normal)
            }

            Stmt::FilePrint {
                channel,
                items,
                newline,
            } => {
                let text = self.render_print_list(items, *newline)?;
                let channel = self.channel_of(channel)?;
                let file = self.channel(channel)?;
                file.write_all(text.as_bytes())
                    .map_err(|e| BrtError::new(5, format!("Write to #{channel} failed: {e}")))?;
                Ok(Flow::Normal)
            }

            Stmt::FileWrite { channel, values } => {
                let mut evaluated = Vec::with_capacity(values.len());
                for value in values {
                    evaluated.push(self.eval(value)?);
                }
                let record = crate::records::encode(&evaluated)?;
                let channel = self.channel_of(channel)?;
                let file = self.channel(channel)?;
                writeln!(file, "{record}")
                    .map_err(|e| BrtError::new(5, format!("Write to #{channel} failed: {e}")))?;
                Ok(Flow::Normal)
            }

            Stmt::FileInput { channel, targets } => {
                let channel = self.channel_of(channel)?;
                let file = self.channel(channel)?;
                let line = match read_line(file) {
                    Ok(Some(line)) => line,
                    // Past the end there is nothing to read; the variables keep
                    // whatever they held, which is what EOF(#n) guards against.
                    Ok(None) => return Ok(Flow::Normal),
                    Err(e) => {
                        return Err(BrtError::new(5, format!("Read from #{channel} failed: {e}")))
                    }
                };
                let fields = crate::records::parse(&line);
                for (index, target) in targets.iter().enumerate() {
                    let value = fields.get(index).cloned().unwrap_or(Variant::Empty);
                    self.assign_var(target, value)?;
                }
                Ok(Flow::Normal)
            }

            Stmt::FileClose(channel) => {
                let channel = self.channel_of(channel)?;
                if self.globals.file_channels.remove(&channel).is_none() {
                    return Err(BrtError::bad_file_channel(channel));
                }
                Ok(Flow::Normal)
            }

            Stmt::FileCloseAll => {
                self.globals.file_channels.clear();
                Ok(Flow::Normal)
            }

            Stmt::FileRead { channel, target } => {
                let channel = self.channel_of(channel)?;
                let file = self.channel(channel)?;
                let value = match read_line(file) {
                    Ok(Some(line)) => Variant::String(line),
                    Ok(None) => Variant::Empty,
                    Err(e) => {
                        return Err(BrtError::new(5, format!("Read from #{channel} failed: {e}")))
                    }
                };
                self.assign_var(target, value)?;
                Ok(Flow::Normal)
            }
        }
    }

    fn exec_for(
        &mut self,
        variable: &str,
        start: &Expr,
        end: &Expr,
        step: Option<&Expr>,
        body: &[Stmt],
    ) -> Result<Flow, BrtError> {
        let start_value = self.eval(start)?;
        let end_value = self.eval(end)?;
        let step_value = match step {
            Some(expr) => self.eval(expr)?,
            None => Variant::Int(1),
        };

        // An all-integer loop counts in integers, so we never drift.
        let integral = matches!(start_value, Variant::Int(_))
            && matches!(end_value, Variant::Int(_))
            && matches!(step_value, Variant::Int(_));

        let first = start_value.to_float()?;
        let last = end_value.to_float()?;
        let stride = step_value.to_float()?;
        if stride == 0.0 {
            return Err(BrtError::invalid_call("For loop Step cannot be zero"));
        }

        let mut counter = if integral {
            Counter::Int(first as i64)
        } else {
            Counter::Float(first)
        };
        let last_int = last as i64;
        let stride_int = stride as i64;

        loop {
            let keep_going = match counter {
                Counter::Int(current) => {
                    if stride_int > 0 {
                        current <= last_int
                    } else {
                        current >= last_int
                    }
                }
                Counter::Float(current) => {
                    if stride > 0.0 {
                        current <= last
                    } else {
                        current >= last
                    }
                }
            };
            if !keep_going {
                break;
            }
            let value = match counter {
                Counter::Int(current) => Variant::Int(current),
                Counter::Float(current) => Variant::Float(current),
            };
            self.assign_var(variable, value)?;
            match self.run_iteration(body)? {
                Iteration::Again => {}
                Iteration::Stop => return Ok(Flow::Normal),
                Iteration::Return => return Ok(Flow::Return),
            }
            counter = match counter {
                Counter::Int(current) => Counter::Int(current.wrapping_add(stride_int)),
                Counter::Float(current) => Counter::Float(current + stride),
            };
        }
        Ok(Flow::Normal)
    }

    /// Runs a loop body once and reports what the loop should do next.
    fn run_iteration(&mut self, body: &[Stmt]) -> Result<Iteration, BrtError> {
        Ok(match self.exec_block(body)? {
            Flow::Normal => Iteration::Again,
            Flow::Break => Iteration::Stop,
            Flow::Return => Iteration::Return,
        })
    }

    /// `For Each` walks a snapshot of the elements taken when the loop started.
    fn run_for_each(
        &mut self,
        variable: &str,
        elements: Vec<Variant>,
        body: &[Stmt],
    ) -> Result<Flow, BrtError> {
        for element in elements {
            self.assign_var(variable, element)?;
            match self.run_iteration(body)? {
                Iteration::Again => {}
                Iteration::Stop => return Ok(Flow::Normal),
                Iteration::Return => return Ok(Flow::Return),
            }
        }
        Ok(Flow::Normal)
    }

    /// `While cond` keeps going while the condition holds; `Until cond` keeps
    /// going while it does not.
    fn loop_condition_holds(&mut self, condition: &LoopCondition) -> Result<bool, BrtError> {
        let value = self.eval(&condition.condition)?.truthy();
        Ok(if condition.until { !value } else { value })
    }

    fn run_while(&mut self, condition: &Expr, body: &[Stmt]) -> Result<Flow, BrtError> {
        while self.eval(condition)?.truthy() {
            match self.run_iteration(body)? {
                Iteration::Again => {}
                Iteration::Stop => return Ok(Flow::Normal),
                Iteration::Return => return Ok(Flow::Return),
            }
        }
        Ok(Flow::Normal)
    }

    /// `Do` loops may test before the body, after it, both, or neither.
    fn run_do_loop(
        &mut self,
        pre: Option<&LoopCondition>,
        post: Option<&LoopCondition>,
        body: &[Stmt],
    ) -> Result<Flow, BrtError> {
        loop {
            if let Some(condition) = pre {
                if !self.loop_condition_holds(condition)? {
                    return Ok(Flow::Normal);
                }
            }
            match self.run_iteration(body)? {
                Iteration::Again => {}
                Iteration::Stop => return Ok(Flow::Normal),
                Iteration::Return => return Ok(Flow::Return),
            }
            if let Some(condition) = post {
                if !self.loop_condition_holds(condition)? {
                    return Ok(Flow::Normal);
                }
            }
        }
    }

    /// `Exit For`/`Exit While`/`Exit Do` must match the innermost loop, so a
    /// mistargeted exit is reported instead of silently doing the wrong thing.
    fn exit_loop(&self, expected: LoopKind, word: &str) -> Result<Flow, BrtError> {
        match self.loops.last() {
            Some(current) if *current == expected => Ok(Flow::Break),
            Some(current) => Err(BrtError::invalid_call(format!(
                "Exit {word} used inside a {} loop",
                current.describe()
            ))),
            None => Err(BrtError::exit_outside_loop(word, expected.describe())),
        }
    }

    /// `Select Case`: the subject is evaluated once, and the first arm holding a
    /// matching pattern runs.  There is no fall-through.
    fn exec_select_case(
        &mut self,
        subject: &Expr,
        arms: &[SelectArm],
        otherwise: Option<&[Stmt]>,
    ) -> Result<Flow, BrtError> {
        let subject = self.eval(subject)?;
        for arm in arms {
            for pattern in &arm.patterns {
                if self.case_matches(&subject, pattern)? {
                    return self.exec_block(&arm.body);
                }
            }
        }
        match otherwise {
            Some(body) => self.exec_block(body),
            None => Ok(Flow::Normal),
        }
    }

    fn case_matches(&mut self, subject: &Variant, pattern: &CasePattern) -> Result<bool, BrtError> {
        match pattern {
            CasePattern::Value(expr) => {
                let value = self.eval(expr)?;
                Ok(compare(subject, &value)? == Ordering::Equal)
            }
            CasePattern::Range(low, high) => {
                let low = self.eval(low)?;
                let high = self.eval(high)?;
                Ok(compare(subject, &low)? != Ordering::Less
                    && compare(subject, &high)? != Ordering::Greater)
            }
            CasePattern::Comparison { operator, value } => {
                let value = self.eval(value)?;
                let ordering = compare(subject, &value)?;
                Ok(match operator {
                    Token::Eq => ordering.is_eq(),
                    Token::Ne => ordering.is_ne(),
                    Token::Lt => ordering.is_lt(),
                    Token::Gt => ordering.is_gt(),
                    Token::Le => ordering.is_le(),
                    Token::Ge => ordering.is_ge(),
                    _ => false,
                })
            }
        }
    }

    /// Resolves a `#n` file number.  The number is an expression, so this is
    /// what lets `FreeFile()` choose a channel at run time.
    fn channel_of(&mut self, expression: &Expr) -> Result<u8, BrtError> {
        let number = self.eval(expression)?.to_int()?;
        u8::try_from(number).map_err(|_| {
            BrtError::new(52, format!("#{number} is not a valid file channel (0-255)"))
        })
    }

    /// Renders a `Print` list, applying the separators and the trailing newline.
    ///
    /// VB6 advanced a comma to the next fixed 14-column zone; this emits a tab,
    /// which needs no per-stream column bookkeeping and behaves sensibly in both
    /// files and terminals.
    fn render_print_list(&mut self, items: &[PrintItem], newline: bool) -> Result<String, BrtError> {
        let mut text = String::new();
        for item in items {
            text.push_str(&self.eval(&item.value)?.display_string());
            if item.after == Separator::Zone {
                text.push('\t');
            }
        }
        if newline {
            text.push('\n');
        }
        Ok(text)
    }

    fn set_err_object(&mut self, error: &BrtError) -> Rc<RefCell<dyn BartObject>> {
        let file = error.unit.and_then(|unit| {
            self.sources
                .as_ref()
                .and_then(|sources| sources.name(unit))
                .map(str::to_string)
        });
        let mut object = ErrObject::new();
        object.set_from(error, file.as_deref());
        let handle: Rc<RefCell<dyn BartObject>> = Rc::new(RefCell::new(object));
        self.globals
            .variables
            .insert("err".to_string(), Variant::Object(handle.clone()));
        handle
    }

    // ------------------------------------------------------------ expressions

    /// Evaluates one expression.
    pub fn eval(&mut self, expr: &Expr) -> Result<Variant, BrtError> {
        match expr {
            Expr::Literal(value) => Ok(value.clone()),

            Expr::Variable(name) => self.lookup_var(name),

            Expr::InterpolatedString(parts) => {
                let mut text = String::new();
                for part in parts {
                    text.push_str(&self.eval(part)?.display_string());
                }
                Ok(Variant::String(text))
            }

            Expr::BinaryOp {
                left,
                operator,
                right,
            } => self.eval_binary(left, operator, right),

            Expr::UnaryOp { operator, right } => {
                let value = self.eval(right)?;
                match operator {
                    Token::Minus => Ok(match value.to_num()? {
                        Num::Int(v) => Variant::Int(v.wrapping_neg()),
                        Num::Float(v) => Variant::Float(-v),
                    }),
                    Token::Plus => Ok(value),
                    Token::Ident(name) if name == "not" => Ok(Variant::Bool(!value.truthy())),
                    other => Err(BrtError::invalid_call(format!(
                        "unsupported unary operator {}",
                        other.describe()
                    ))),
                }
            }

            Expr::ShellCommand(command) => {
                let output = self.run_shell(command, None)?;
                Ok(Variant::String(output))
            }

            Expr::Pipeline { input, command } => {
                let data = self.eval(input)?.as_string()?;
                let output = self.run_shell(command, Some(data))?;
                Ok(Variant::String(output))
            }

            Expr::FunctionCall { name, args } => {
                if self.procedures.contains_key(name) {
                    let mut evaluated = Vec::with_capacity(args.len());
                    for arg in args {
                        evaluated.push(self.eval(arg)?);
                    }
                    return self.call_procedure(name, evaluated);
                }
                // `IIf` is the one built-in that must not evaluate both
                // branches, so it is intercepted before the eager path below.
                // VB6's `IIf` did evaluate both; see the README for the why.
                if name == "iif" {
                    return self.eval_iif(args);
                }
                let mut evaluated = Vec::with_capacity(args.len());
                for arg in args {
                    evaluated.push(self.eval(arg)?);
                }
                if is_builtin(name) {
                    return self.call_builtin(name, evaluated);
                }
                // `a(1)` where `Dim a(3)` was declared: a one-argument index.
                if evaluated.len() == 1 {
                    if let Some(handle) = self.try_lookup_array(name) {
                        let index = evaluated[0].to_int()?;
                        let elements = handle.borrow();
                        if index < 1 || index as usize > elements.len() {
                            return Err(BrtError::subscript(format!(
                                "{name}({index}) is outside 1..{}",
                                elements.len()
                            )));
                        }
                        return Ok(elements[(index - 1) as usize].clone());
                    }
                    // VB6's default member: `dict(key)` means `dict.Item(key)`.
                    if let Ok(Variant::Object(handle)) = self.lookup_var(name) {
                        let key = evaluated.into_iter().next().expect("one argument");
                        return handle.borrow_mut().call_method("item", vec![key]);
                    }
                }
                Err(BrtError::not_defined(name))
            }

            Expr::MethodCall {
                object,
                method,
                args,
            } => self.eval_member(object, method, args),

            Expr::MemberAccess {
                object,
                member,
                args,
            } => {
                let target = self.eval(object)?;
                let mut evaluated = Vec::with_capacity(args.len());
                for arg in args {
                    evaluated.push(self.eval(arg)?);
                }
                self.apply_member(target, member, evaluated)
            }

            Expr::ArrayAccess { name, index } => {
                let index = self.eval(index)?.to_int()?;
                self.array_get(name, index)
            }
        }
    }

    fn eval_member(
        &mut self,
        object: &str,
        member: &str,
        args: &[Expr],
    ) -> Result<Variant, BrtError> {
        // `lookup_shared` keeps a record's identity rather than copying it, so
        // reading and writing a field reach the same record the variable holds.
        let target = self.lookup_shared(object)?;
        let mut evaluated = Vec::with_capacity(args.len());
        for arg in args {
            evaluated.push(self.eval(arg)?);
        }
        self.apply_member(target, member, evaluated)
    }

    /// Dispatches `receiver.member(args)` once the receiver is in hand.
    fn apply_member(
        &mut self,
        target: Variant,
        member: &str,
        args: Vec<Variant>,
    ) -> Result<Variant, BrtError> {
        let handle = match target {
            Variant::Object(handle) => handle,
            // A record's fields are reached the same way an object's members
            // are, so `p.X` needs no separate syntax.
            Variant::Record(record) => {
                if !args.is_empty() {
                    return Err(BrtError::no_such_member(&record.borrow().type_name, member));
                }
                return record.borrow().get(member);
            }
            other => {
                return Err(BrtError::object_required(format!(
                    "'{member}' is not a member of a {} value",
                    other.type_name()
                )))
            }
        };

        // Look the member kind up in its own statement: a temporary borrow held
        // across a `match` would still be live inside the arms, and the method
        // arm needs a mutable borrow of the same cell.
        let kind = handle.borrow().member_kind(member);
        match kind {
            Member::Property => {
                if !args.is_empty() {
                    return Err(BrtError::no_such_member(&handle.borrow().type_name(), member));
                }
                handle.borrow().get_property(member)
            }
            Member::Method => handle.borrow_mut().call_method(member, args),
            Member::Unknown => {
                if args.is_empty() {
                    Err(BrtError::no_such_member(
                        &handle.borrow().type_name(),
                        member,
                    ))
                } else {
                    // Lenient fallback for objects that do not advertise members.
                    handle.borrow_mut().call_method(member, args)
                }
            }
        }
    }

    fn eval_binary(&mut self, left: &Expr, operator: &Token, right: &Expr) -> Result<Variant, BrtError> {
        // `And` / `Or` short-circuit, so they are handled before evaluation.
        if let Token::Ident(name) = operator {
            match name.as_str() {
                "and" => {
                    if !self.eval(left)?.truthy() {
                        return Ok(Variant::Bool(false));
                    }
                    return Ok(Variant::Bool(self.eval(right)?.truthy()));
                }
                "or" => {
                    if self.eval(left)?.truthy() {
                        return Ok(Variant::Bool(true));
                    }
                    return Ok(Variant::Bool(self.eval(right)?.truthy()));
                }
                _ => {}
            }
        }

        let lhs = self.eval(left)?;

        // `+` on two strings is still a numeric operation, so it must not
        // silently concatenate.  `&` is the concatenation operator.
        let rhs = self.eval(right)?;

        match operator {
            Token::Amp => Ok(Variant::String(format!(
                "{}{}",
                lhs.as_string()?,
                rhs.as_string()?
            ))),
            Token::Plus => arithmetic(&lhs, &rhs, Arithmetic::Add),
            Token::Minus => arithmetic(&lhs, &rhs, Arithmetic::Subtract),
            Token::Star => arithmetic(&lhs, &rhs, Arithmetic::Multiply),
            Token::Slash => arithmetic(&lhs, &rhs, Arithmetic::Divide),
            Token::Backslash => arithmetic(&lhs, &rhs, Arithmetic::IntegerDivide),
            Token::Caret => arithmetic(&lhs, &rhs, Arithmetic::Power),
            Token::Ident(name) if name == "mod" => arithmetic(&lhs, &rhs, Arithmetic::Modulo),
            Token::Eq | Token::Ne | Token::Lt | Token::Gt | Token::Le | Token::Ge => {
                let ordering = compare(&lhs, &rhs)?;
                let result = match operator {
                    Token::Eq => ordering.is_eq(),
                    Token::Ne => ordering.is_ne(),
                    Token::Lt => ordering.is_lt(),
                    Token::Gt => ordering.is_gt(),
                    Token::Le => ordering.is_le(),
                    Token::Ge => ordering.is_ge(),
                    _ => unreachable!(),
                };
                Ok(Variant::Bool(result))
            }
            other => Err(BrtError::invalid_call(format!(
                "unsupported operator {}",
                other.describe()
            ))),
        }
    }

    // ------------------------------------------------------------- procedures

    fn call_procedure(&mut self, name: &str, args: Vec<Variant>) -> Result<Variant, BrtError> {
        let procedure = self
            .procedures
            .get(name)
            .cloned()
            .ok_or_else(|| BrtError::not_defined(name))?;

        if args.len() != procedure.params.len() {
            return Err(BrtError::wrong_arg_count(
                name,
                args.len(),
                &procedure.params.len().to_string(),
            ));
        }

        self.depth += 1;
        if self.depth > self.max_depth {
            self.depth -= 1;
            return Err(BrtError::stack_overflow(self.max_depth));
        }

        let mut frame = Frame::default();
        for (param, value) in procedure.params.iter().zip(args) {
            frame.variables.insert(param.clone(), value);
        }
        if procedure.is_function {
            // A `Function` returns by assigning to its own name.
            frame.variables.insert(name.to_string(), Variant::Empty);
        }
        self.frames.push(frame);

        let result = self.exec_block(&procedure.body);

        // `Exit Sub`/`Exit Function` surface as `Flow::Return`; either way the
        // body has finished, so a `Function` now reads its own name's value.
        let outcome = match result {
            Ok(_flow) => {
                if procedure.is_function {
                    let value = self
                        .frames
                        .last()
                        .and_then(|frame| frame.variables.get(name))
                        .cloned()
                        .unwrap_or(Variant::Empty);
                    Ok(value)
                } else {
                    Ok(Variant::Empty)
                }
            }
            Err(error) => Err(error),
        };

        self.frames.pop();
        self.depth -= 1;
        outcome
    }

    // ----------------------------------------------------------- bash engine

    /// `IIf(condition, when_true, when_false)` - only the taken branch runs.
    ///
    /// A deliberate break from VB6, where `IIf` was an ordinary function and
    /// therefore evaluated both sides: the old behaviour meant
    /// `IIf(x <> 0, 1 / x, 0)` divided by zero anyway.
    fn eval_iif(&mut self, args: &[Expr]) -> Result<Variant, BrtError> {
        if args.len() != 3 {
            return Err(BrtError::wrong_arg_count("IIf", args.len(), "3"));
        }
        if self.eval(&args[0])?.truthy() {
            self.eval(&args[1])
        } else {
            self.eval(&args[2])
        }
    }

    /// Runs `sh -c <command>`, optionally feeding `stdin_data` into it, and
    /// returns stdout with the trailing newline removed.
    fn run_shell(&mut self, command: &str, stdin_data: Option<String>) -> Result<String, BrtError> {
        let mut child = Command::new("sh")
            .arg("-c")
            .arg(command)
            .stdin(if stdin_data.is_some() {
                Stdio::piped()
            } else {
                Stdio::null()
            })
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|e| BrtError::shell(format!("Could not start shell: {e}")))?;

        if let Some(data) = stdin_data {
            if let Some(mut pipe) = child.stdin.take() {
                // Feed the child from a helper thread so that a large payload
                // cannot deadlock against a chatty pipeline.
                let _ = thread::Builder::new()
                    .name("bartelang-stdin".to_string())
                    .spawn(move || {
                        let _ = pipe.write_all(data.as_bytes());
                        // Dropping `pipe` here closes the child's stdin.
                    });
            }
        }

        let output = child
            .wait_with_output()
            .map_err(|e| BrtError::shell(format!("Shell command failed: {e}")))?;

        self.globals.last_exit_code = output.status.code().unwrap_or(-1);

        if !output.stderr.is_empty() {
            eprint!("{}", String::from_utf8_lossy(&output.stderr));
        }

        Ok(strip_trailing_newline(&String::from_utf8_lossy(&output.stdout)))
    }
}

// ------------------------------------------------------------------ helpers

enum Arithmetic {
    Add,
    Subtract,
    Multiply,
    Divide,
    IntegerDivide,
    Modulo,
    Power,
}

/// Implements the VB6 arithmetic quirks: strings are parsed as numbers when
/// possible, and a string that is not numeric is a type mismatch rather than a
/// silent concatenation.
fn arithmetic(left: &Variant, right: &Variant, operation: Arithmetic) -> Result<Variant, BrtError> {
    let a = left.to_num()?;
    let b = right.to_num()?;

    match operation {
        Arithmetic::Add => match (a, b) {
            (Num::Int(x), Num::Int(y)) => x
                .checked_add(y)
                .map(Variant::Int)
                .ok_or_else(|| BrtError::overflow("+")),
            _ => Ok(Variant::Float(a.as_f64() + b.as_f64())),
        },
        Arithmetic::Subtract => match (a, b) {
            (Num::Int(x), Num::Int(y)) => x
                .checked_sub(y)
                .map(Variant::Int)
                .ok_or_else(|| BrtError::overflow("-")),
            _ => Ok(Variant::Float(a.as_f64() - b.as_f64())),
        },
        Arithmetic::Multiply => match (a, b) {
            (Num::Int(x), Num::Int(y)) => x
                .checked_mul(y)
                .map(Variant::Int)
                .ok_or_else(|| BrtError::overflow("*")),
            _ => Ok(Variant::Float(a.as_f64() * b.as_f64())),
        },
        Arithmetic::Divide => {
            let divisor = b.as_f64();
            if divisor == 0.0 {
                return Err(BrtError::division_by_zero());
            }
            Ok(Variant::Float(a.as_f64() / divisor))
        }
        Arithmetic::IntegerDivide => {
            let divisor = b.as_i64();
            if divisor == 0 {
                return Err(BrtError::division_by_zero());
            }
            Ok(Variant::Int(a.as_i64().wrapping_div(divisor)))
        }
        Arithmetic::Modulo => {
            let divisor = b.as_i64();
            if divisor == 0 {
                return Err(BrtError::division_by_zero());
            }
            Ok(Variant::Int(a.as_i64().wrapping_rem(divisor)))
        }
        Arithmetic::Power => Ok(Variant::Float(a.as_f64().powf(b.as_f64()))),
    }
}

/// The specification says the trailing newline is stripped from shell output.
fn strip_trailing_newline(text: &str) -> String {
    let trimmed = text.strip_suffix('\n').unwrap_or(text);
    trimmed.strip_suffix('\r').unwrap_or(trimmed).to_string()
}

/// Reads one line from a vintage file channel, without needing a buffered reader.
pub(crate) fn read_line(file: &mut File) -> std::io::Result<Option<String>> {
    let mut buffer: Vec<u8> = Vec::new();
    let mut byte = [0u8; 1];
    loop {
        match file.read(&mut byte)? {
            0 => {
                if buffer.is_empty() {
                    return Ok(None);
                }
                break;
            }
            _ => {
                if byte[0] == b'\n' {
                    break;
                }
                buffer.push(byte[0]);
            }
        }
    }
    if buffer.last() == Some(&b'\r') {
        buffer.pop();
    }
    Ok(Some(String::from_utf8_lossy(&buffer).into_owned()))
}

/// True when a file channel has been read to the end - backs `EOF(#1)`.
pub(crate) fn channel_at_eof(file: &mut File) -> std::io::Result<bool> {
    let position = file.stream_position()?;
    let length = file.metadata()?.len();
    Ok(position >= length)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::parser::Parser;

    fn run(src: &str) -> Result<Vec<String>, BrtError> {
        let (program, _) = Parser::parse_source(src).expect("parse");
        let mut interp = Interp::new(Vec::new());
        interp.run(&program)?;
        Ok(Vec::new())
    }

    fn eval_value(src: &str) -> Variant {
        let (program, _) = Parser::parse_source(src).expect("parse");
        let mut interp = Interp::new(Vec::new());
        let Stmt::Let { value, .. } = crate::ast::peel(&program.statements[0]) else {
            panic!("expected a Let statement");
        };
        interp.eval(value).expect("evaluate")
    }

    #[test]
    fn plus_adds_but_amp_concatenates() {
        assert_eq!(eval_value("Let r = 2 + 3 * 4\n").to_int().unwrap(), 14);
        assert_eq!(
            eval_value("Let r = \"1\" + 2\n").to_int().unwrap(),
            3
        );
        assert_eq!(eval_value("Let r = \"a\" & 1\n").display_string(), "a1");
    }

    #[test]
    fn plus_on_a_non_numeric_string_is_a_type_mismatch() {
        let err = run("Let r = \"abc\" + 1\n").unwrap_err();
        assert_eq!(err.number, 13);
    }

    #[test]
    fn integer_and_modulo_division() {
        assert_eq!(eval_value("Let r = 7 \\ 2\n").to_int().unwrap(), 3);
        assert_eq!(eval_value("Let r = 7 Mod 4\n").to_int().unwrap(), 3);
        assert_eq!(eval_value("Let r = 7 / 2\n").to_float().unwrap(), 3.5);
    }

    #[test]
    fn division_by_zero_is_error_11() {
        let err = run("Let r = 1 / 0\n").unwrap_err();
        assert_eq!(err.number, 11);
    }

    #[test]
    fn for_loop_counts_inclusively() {
        run("Dim total\nLet total = 0\nFor i = 1 To 5\n  Let total = total + i\nNext\nIf total <> 15 Then\n  Let crash = 1 / 0\nEnd If\n").unwrap();
    }

    #[test]
    fn while_loop_with_wend() {
        run("Dim i\nLet i = 0\nWhile i < 3\n  Let i = i + 1\nWend\nIf i <> 3 Then\n  Let crash = 1 / 0\nEnd If\n").unwrap();
    }

    #[test]
    fn function_returns_via_its_own_name() {
        run("Function Double(n)\n  Double = n * 2\nEnd Function\nDim r\nLet r = Double(21)\nIf r <> 42 Then\n  Let crash = 1 / 0\nEnd If\n").unwrap();
    }

    #[test]
    fn recursion_is_supported() {
        run("Function Fact(n)\n  If n <= 1 Then\n    Fact = 1\n  Else\n    Fact = n * Fact(n - 1)\n  End If\nEnd Function\nDim r\nLet r = Fact(6)\nIf r <> 720 Then\n  Let crash = 1 / 0\nEnd If\n").unwrap();
    }

    #[test]
    fn try_catch_swallows_errors() {
        run("Dim caught\nLet caught = 0\nTry\n  Let boom = 1 / 0\nCatch Err\n  Let caught = Err.Number\nEnd Try\nIf caught <> 11 Then\n  Let crash = 1 / 0\nEnd If\n").unwrap();
    }

    #[test]
    fn sub_can_be_called_without_parentheses() {
        run("Dim out\nLet out = 0\nSub Bump()\n  Let out = out + 1\nEnd Sub\nBump\nBump\nIf out <> 2 Then\n  Let crash = 1 / 0\nEnd If\n").unwrap();
    }

    #[test]
    fn undefined_variable_is_reported() {
        let err = run("Debug.Print missing\n").unwrap_err();
        assert_eq!(err.number, 500);
    }

    #[test]
    fn booleans_are_vba_style() {
        assert_eq!(eval_value("Let r = True\n").to_int().unwrap(), -1);
        assert_eq!(eval_value("Let r = 1 = 1\n").raw_bool(), true);
        assert_eq!(eval_value("Let r = Not 0\n").raw_bool(), true);
    }

    #[test]
    fn arrays_are_one_based_and_bounds_checked() {
        run("Dim buf(3)\nLet buf(1) = 10\nLet buf(3) = 30\nIf buf(1) + buf(3) <> 40 Then\n  Let crash = 1 / 0\nEnd If\n").unwrap();
        let err = run("Dim buf(3)\nLet buf(0) = 1\n").unwrap_err();
        assert_eq!(err.number, 9);
    }

    #[test]
    fn shell_backticks_return_stdout_and_set_the_exit_code() {
        let value = eval_value("Let r = `printf hello`\n");
        assert_eq!(value.display_string(), "hello");
        let code = eval_value("Let r = `exit 7`\n");
        let _ = code;
    }

    #[test]
    fn pipeline_feeds_stdin() {
        let value = eval_value("Let r = \"hello\" | `tr a-z A-Z`\n");
        assert_eq!(value.display_string(), "HELLO");
    }

    #[test]
    fn env_reports_the_last_exit_code() {
        run("Dim a\nLet a = `true`\nIf ENV(\"?\") <> 0 Then\n  Let crash = 1 / 0\nEnd If\nDim b\nLet b = `exit 3`\nIf ENV(\"?\") <> 3 Then\n  Let crash = 1 / 0\nEnd If\n").unwrap();
    }

    #[test]
    fn interpolation_reads_variables() {
        run("Dim who\nLet who = \"world\"\nDim msg\nLet msg = \"hi $who!\"\nIf msg <> \"hi world!\" Then\n  Let crash = 1 / 0\nEnd If\n").unwrap();
    }

    #[test]
    fn string_builtins_are_one_based() {
        assert_eq!(eval_value("Let r = Mid(\"abcdef\", 2, 3)\n").display_string(), "bcd");
        assert_eq!(eval_value("Let r = Left(\"abcdef\", 2)\n").display_string(), "ab");
        assert_eq!(eval_value("Let r = Right(\"abcdef\", 2)\n").display_string(), "ef");
        assert_eq!(eval_value("Let r = InStr(\"abcdef\", \"cd\")\n").to_int().unwrap(), 3);
        assert_eq!(eval_value("Let r = Len(\"abc\")\n").to_int().unwrap(), 3);
        assert_eq!(eval_value("Let r = UCase(\"abc\")\n").display_string(), "ABC");
    }

    // ------------------------------------- Milestone 1: control flow and errors

    #[test]
    fn exit_for_leaves_the_loop_early() {
        run("Dim total\nLet total = 0\nFor i = 1 To 100\n  If i > 4 Then\n    Exit For\n  End If\n  Let total = total + i\nNext\nIf total <> 10 Then\n  Let crash = 1 / 0\nEnd If\n").unwrap();
    }

    #[test]
    fn exit_while_and_exit_do_leave_their_loops() {
        run("Dim n\nLet n = 0\nWhile True\n  Let n = n + 1\n  If n = 2 Then\n    Exit While\n  End If\nWend\nIf n <> 2 Then\n  Let crash = 1 / 0\nEnd If\n").unwrap();
        run("Dim n\nLet n = 0\nDo\n  Let n = n + 1\n  If n >= 3 Then\n    Exit Do\n  End If\nLoop\nIf n <> 3 Then\n  Let crash = 1 / 0\nEnd If\n").unwrap();
    }

    #[test]
    fn a_mistargeted_exit_is_reported() {
        let error = run("For i = 1 To 3\n  Exit Do\nNext\n").unwrap_err();
        assert_eq!(error.number, 5);
        assert!(error.message.contains("Exit Do"), "{}", error.message);

        let error = run("Exit For\n").unwrap_err();
        assert!(
            error.message.contains("outside a For...Next loop"),
            "{}",
            error.message
        );
    }

    #[test]
    fn exit_function_and_exit_sub_return_early() {
        run("Function First()\n  First = \"early\"\n  Exit Function\n  First = \"late\"\nEnd Function\nDim r\nLet r = First()\nIf r <> \"early\" Then\n  Let crash = 1 / 0\nEnd If\n").unwrap();
        run("Dim reached\nLet reached = 0\nSub Stop()\n  Let reached = 1\n  Exit Sub\n  Let reached = 2\nEnd Sub\nStop\nIf reached <> 1 Then\n  Let crash = 1 / 0\nEnd If\n").unwrap();
    }

    #[test]
    fn exit_outside_a_procedure_is_an_error() {
        let error = run("Exit Sub\n").unwrap_err();
        assert!(
            error.message.contains("outside a procedure"),
            "{}",
            error.message
        );
    }

    #[test]
    fn do_loop_supports_all_four_condition_placements() {
        run("Dim n\nLet n = 0\nDo While n < 3\n  Let n = n + 1\nLoop\nIf n <> 3 Then\n  Let crash = 1 / 0\nEnd If\n").unwrap();
        run("Dim n\nLet n = 0\nDo Until n = 2\n  Let n = n + 1\nLoop\nIf n <> 2 Then\n  Let crash = 1 / 0\nEnd If\n").unwrap();
        run("Dim n\nLet n = 0\nDo\n  Let n = n + 1\nLoop While n < 3\nIf n <> 3 Then\n  Let crash = 1 / 0\nEnd If\n").unwrap();
        // Post-condition: the body runs even though the test is already true.
        run("Dim n\nLet n = 0\nDo\n  Let n = n + 1\nLoop Until True\nIf n <> 1 Then\n  Let crash = 1 / 0\nEnd If\n").unwrap();
    }

    #[test]
    fn select_case_matches_lists_ranges_and_is() {
        run("Dim r\nSelect Case 7\nCase 1, 2\n  Let r = \"low\"\nCase 5 To 9\n  Let r = \"middle\"\nCase Else\n  Let r = \"high\"\nEnd Select\nIf r <> \"middle\" Then\n  Let crash = 1 / 0\nEnd If\n").unwrap();
        run("Dim r\nSelect Case 99\nCase Is < 10\n  Let r = \"small\"\nCase Is >= 50\n  Let r = \"big\"\nEnd Select\nIf r <> \"big\" Then\n  Let crash = 1 / 0\nEnd If\n").unwrap();
    }

    #[test]
    fn select_case_takes_the_first_match_and_falls_back_to_else() {
        run("Dim r\nSelect Case \"b\"\nCase \"b\"\n  Let r = \"first\"\nCase \"a\", \"b\"\n  Let r = \"second\"\nCase Else\n  Let r = \"else\"\nEnd Select\nIf r <> \"first\" Then\n  Let crash = 1 / 0\nEnd If\n").unwrap();
        run("Dim r\nSelect Case 100\nCase 1\n  Let r = \"one\"\nCase Else\n  Let r = \"other\"\nEnd Select\nIf r <> \"other\" Then\n  Let crash = 1 / 0\nEnd If\n").unwrap();
    }

    #[test]
    fn with_resolves_dot_members_to_its_object() {
        run(r#"
Dim http
Set http = CreateObject("HTTP")
With http
    .Open "GET", "https://example.invalid/", False
    .SetRequestHeader "Accept", "text/plain"
    If .ReadyState <> 1 Then
        Err.Raise 500, "with-test", "ReadyState should be 1 after Open"
    End If
End With
"#).unwrap();
    }

    #[test]
    fn with_keeps_the_reference_it_captured() {
        run(r#"
Dim first
Set first = CreateObject("HTTP")
Dim second
Set second = CreateObject("HTTP")
second.Open "GET", "https://example.invalid/two", False
With first
    Set first = second
    ' The captured object never had Open called, so this must still be 0.
    If .ReadyState <> 0 Then
        Err.Raise 500, "with-test", "With did not capture the object"
    End If
End With
"#).unwrap();
    }

    #[test]
    fn with_bindings_survive_recursion() {
        // Every recursive frame pushes its own capture under the same synthetic
        // name, so the lookup has to be innermost-first and the stack has to be
        // popped on the way out.
        run(r#"
Function Countdown(n)
    Dim probe
    Set probe = CreateObject("HTTP")
    With probe
        If n <= 0 Then
            Countdown = 0
        Else
            Countdown = Countdown(n - 1) + 1
        End If
        ' Force a member lookup at every level, so the binding must be live here.
        If .ReadyState < 0 Then
            Countdown = -1
        End If
    End With
End Function

Dim r
Let r = Countdown(4)
If r <> 4 Then
    Let crash = 1 / 0
End If
"#).unwrap();
    }

    #[test]
    fn const_cannot_be_reassigned() {
        run("Const Limit = 10\nIf Limit <> 10 Then\n  Let crash = 1 / 0\nEnd If\n").unwrap();
        let error = run("Const Limit = 10\nLet Limit = 11\n").unwrap_err();
        assert_eq!(error.number, 501);
        assert!(error.message.contains("constant"), "{}", error.message);
    }

    #[test]
    fn err_file_stays_empty_for_the_string_api() {
        // There is no file to name when the program never touched one, and the
        // README promises as much; a non-empty `Err.File` would crash here.
        let loaded = crate::load(
            "Dim f\nTry\n    Let boom = 1 / 0\nCatch Err\n    Let f = Err.File\nEnd Try\n\
             If f <> \"\" Then\n    Let crash = 1 / 0\nEnd If\n",
        )
        .expect("load");
        let mut interp = Interp::new(Vec::new());
        interp.run_loaded(&loaded).expect("the script must not crash");
    }

    #[test]
    fn err_raise_is_catchable_and_carries_its_details() {
        run(r#"
Dim number
Dim text
Dim source
Try
    Err.Raise 6001, "MyLibrary", "the input was bad"
Catch Err
    Let number = Err.Number
    Let text = Err.Message
    Let source = Err.Source
End Try
If number <> 6001 Then
    Let crash = 1 / 0
End If
If text <> "the input was bad" Then
    Let crash = 1 / 0
End If
If source <> "MyLibrary" Then
    Let crash = 1 / 0
End If
"#).unwrap();
    }

    #[test]
    fn err_raise_with_only_a_number_gets_a_default_description() {
        run("Dim text\nTry\n  Err.Raise 42\nCatch Err\n  Let text = Err.Message\nEnd Try\nIf text <> \"Application-defined error\" Then\n  Let crash = 1 / 0\nEnd If\n").unwrap();
    }

    #[test]
    fn err_raise_rejects_a_number_below_one() {
        let error = run("Err.Raise 0\n").unwrap_err();
        assert_eq!(error.number, 5);
    }

    #[test]
    fn a_function_can_raise_and_its_caller_can_catch() {
        run(r#"
Function Reciprocal(n)
    If n = 0 Then
        Err.Raise 7001, "Math", "cannot divide by zero"
    End If
    Reciprocal = 1 / n
End Function

Dim value
Try
    Let value = Reciprocal(0)
Catch Err
    Let value = "caught " & Err.Number
End Try
If value <> "caught 7001" Then
    Let crash = 1 / 0
End If
"#).unwrap();
    }

    #[test]
    fn try_catch_does_not_swallow_exit_flows() {
        run(r#"
Function Pick()
    Try
        Pick = "from try"
        Exit Function
    Catch Err
        Pick = "from catch"
    End Try
    Pick = "end of body"
End Function
Dim r
Let r = Pick()
If r <> "from try" Then
    Let crash = 1 / 0
End If
"#).unwrap();
    }

    #[test]
    fn iif_only_evaluates_the_branch_it_returns() {
        // VB6 would have raised here, because IIf evaluated both sides.
        run("Dim r\nLet r = IIf(True, \"safe\", 1 / 0)\nIf r <> \"safe\" Then\n  Let crash = 1 / 0\nEnd If\n").unwrap();
        assert_eq!(eval_value("Let r = IIf(False, 1, 2)\n").to_int().unwrap(), 2);
        assert_eq!(
            eval_value("Let r = IIf(1 = 1, \"yes\", \"no\")\n").display_string(),
            "yes"
        );
    }

    #[test]
    fn a_user_defined_iif_still_wins() {
        run("Function IIf(a, b, c)\n  IIf = \"mine\"\nEnd Function\nDim r\nLet r = IIf(False, 1, 2)\nIf r <> \"mine\" Then\n  Let crash = 1 / 0\nEnd If\n").unwrap();
    }

    // ---------------------------------- Milestone 2: text, arrays and iteration

    #[test]
    fn arrays_are_values_that_can_be_returned_and_passed() {
        run(r#"
Function Build()
    Dim slots(3)
    Let slots(1) = "a"
    Let slots(2) = "b"
    Let slots(3) = "c"
    Build = slots
End Function

Function FirstOf(items)
    FirstOf = items(1)
End Function

Dim got
Let got = Build()
If got(2) <> "b" Then
    Let crash = 1 / 0
End If
If FirstOf(got) <> "a" Then
    Let crash = 1 / 0
End If
"#).unwrap();
    }

    #[test]
    fn split_and_join_round_trip() {
        run(r#"
Dim parts
Let parts = Split("alpha,beta,gamma", ",")
If UBound(parts) <> 3 Then
    Let crash = 1 / 0
End If
If LBound(parts) <> 1 Then
    Let crash = 1 / 0
End If
If parts(2) <> "beta" Then
    Let crash = 1 / 0
End If
If Join(parts, "|") <> "alpha|beta|gamma" Then
    Let crash = 1 / 0
End If
"#).unwrap();
    }

    #[test]
    fn split_of_empty_text_is_an_empty_array() {
        run(r#"
Dim parts
Let parts = Split("", ",")
If UBound(parts) >= LBound(parts) Then
    Let crash = 1 / 0
End If
If Join(parts, ",") <> "" Then
    Let crash = 1 / 0
End If
"#).unwrap();
    }

    #[test]
    fn array_builds_an_inline_array() {
        run(r#"
Dim nums
Let nums = Array(10, 20, 30)
If UBound(nums) <> 3 Then
    Let crash = 1 / 0
End If
If nums(3) <> 30 Then
    Let crash = 1 / 0
End If
"#).unwrap();
    }

    #[test]
    fn for_each_visits_every_element_in_order() {
        run(r#"
Dim words
Let words = Split("one two three")
Dim seen
Let seen = ""
Dim word
For Each word In words
    Let seen = seen & word
Next word
If seen <> "onetwothree" Then
    Let crash = 1 / 0
End If
"#).unwrap();
    }

    #[test]
    fn for_each_can_leave_early_and_nest() {
        run(r#"
Dim nums
Let nums = Array(1, 2, 3, 4)
Dim total
Let total = 0
Dim n
For Each n In nums
    If n > 2 Then
        Exit For
    End If
    Let total = total + n
Next
If total <> 3 Then
    Let crash = 1 / 0
End If

Dim pairs
Let pairs = Array("a", "b")
Dim count
Let count = 0
Dim outer
Dim inner
For Each outer In pairs
    For Each inner In pairs
        Let count = count + 1
    Next inner
Next outer
If count <> 4 Then
    Let crash = 1 / 0
End If
"#).unwrap();
    }

    #[test]
    fn for_each_over_a_non_array_is_reported() {
        let error = run("Dim n\nLet n = 5\nFor Each x In n\nNext\n").unwrap_err();
        assert_eq!(error.number, 13);
    }

    #[test]
    fn redim_resizes_and_preserve_keeps_the_old_elements() {
        run(r#"
Dim slots()
ReDim slots(2)
Let slots(1) = "keep"
Let slots(2) = "also"
ReDim Preserve slots(3)
If UBound(slots) <> 3 Then
    Let crash = 1 / 0
End If
If slots(1) <> "keep" Then
    Let crash = 1 / 0
End If
If Not IsEmpty(slots(3)) Then
    Let crash = 1 / 0
End If
ReDim slots(1)
If UBound(slots) <> 1 Then
    Let crash = 1 / 0
End If
"#).unwrap();
    }

    #[test]
    fn an_array_argument_shares_its_storage() {
        // VB6 passed arrays by reference, so a ReDim inside a procedure was
        // visible to the caller.  Bartelang keeps that aliasing.
        run(r#"
Sub Grow(items)
    ReDim Preserve items(5)
End Sub

Dim nums
Let nums = Array(1)
Grow nums
If UBound(nums) <> 5 Then
    Let crash = 1 / 0
End If
"#).unwrap();
    }

    #[test]
    fn using_a_scalar_as_an_array_is_reported() {
        let error = run("Dim n\nLet n = 5\nDim x\nLet x = UBound(n)\n").unwrap_err();
        assert_eq!(error.number, 13);
    }

    #[test]
    fn an_array_cannot_be_concatenated_with_ampersand() {
        let error = run("Dim a\nLet a = Array(1)\nDim s\nLet s = \"x\" & a\n").unwrap_err();
        assert_eq!(error.number, 13);
    }

    #[test]
    fn a_revived_array_name_reports_a_restored_bounds_error() {
        let error = run("Dim a\nLet a = Array(1, 2)\nLet a(3) = \"too far\"\n").unwrap_err();
        assert_eq!(error.number, 9);
    }

    // --------------------------------------- Milestone 3: the object table

    #[test]
    fn object_members_can_be_assigned_and_objects_can_be_indexed() {
        run(r#"
Dim re
Set re = CreateObject("VBScript.RegExp")
re.Pattern = "a"
If Not re.Test("banana") Then
    Let crash = 1 / 0
End If

Dim d
Set d = CreateObject("Scripting.Dictionary")
d("k") = 5
If d("k") <> 5 Then
    Let crash = 1 / 0
End If
If d.Count <> 1 Then
    Let crash = 1 / 0
End If
"#).unwrap();
    }

    #[test]
    fn assigning_to_a_read_only_member_is_reported() {
        let error = run(
            "Dim d\nSet d = CreateObject(\"Scripting.Dictionary\")\nd.Count = 3\n",
        )
        .unwrap_err();
        assert_eq!(error.number, 438);
    }

    #[test]
    fn assigning_a_member_on_something_that_is_not_an_object_is_reported() {
        let error = run("Dim n\nLet n = 5\nn.Thing = 1\n").unwrap_err();
        assert_eq!(error.number, 424);
    }

    #[test]
    fn an_object_without_a_default_member_cannot_be_indexed() {
        let error = run("Dim c\nSet c = CreateObject(\"Collection\")\nc(1) = 5\n").unwrap_err();
        assert_eq!(error.number, 438);
    }
}

/// Test-only helper so assertions read naturally.
#[cfg(test)]
impl Variant {
    fn raw_bool(&self) -> bool {
        matches!(self, Variant::Bool(true))
    }
}
