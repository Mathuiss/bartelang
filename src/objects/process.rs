//! `WScript.Shell`: structured process execution.
//!
//! This fills a real gap.  A backtick command gives you stdout or nothing, and
//! stderr goes straight to the terminal, so there was no way to capture stdout,
//! stderr *and* the exit code from a single command - which is table stakes for
//! a language whose whole point is running commands.

use std::cell::RefCell;
use std::process::{Command, Stdio};
use std::rc::Rc;

use crate::error::BrtError;
use crate::objects::{BartObject, Member};
use crate::value::Variant;

/// `CreateObject("WScript.Shell")`.
pub struct WshShell;

impl Default for WshShell {
    fn default() -> Self {
        Self::new()
    }
}

impl WshShell {
    /// A handle on process execution.  It holds no state.
    pub fn new() -> Self {
        WshShell
    }
}

fn command_argument(method: &str, args: &[Variant]) -> Result<String, BrtError> {
    match args.first() {
        Some(value) => value.as_string(),
        None => Err(BrtError::wrong_arg_count(method, 0, "at least 1")),
    }
}

/// Strips one trailing newline, matching what a backtick command returns.
fn trim_newline(text: &str) -> String {
    let trimmed = text.strip_suffix('\n').unwrap_or(text);
    trimmed.strip_suffix('\r').unwrap_or(trimmed).to_string()
}

impl BartObject for WshShell {
    fn type_name(&self) -> String {
        "WScript.Shell".to_string()
    }

    fn member_kind(&self, name: &str) -> Member {
        match name.to_ascii_lowercase().as_str() {
            "run" | "exec" => Member::Method,
            _ => Member::Unknown,
        }
    }

    fn call_method(&mut self, method: &str, args: Vec<Variant>) -> Result<Variant, BrtError> {
        match method.to_ascii_lowercase().as_str() {
            // `Run` hands the command the terminal, so its output is visible.
            "run" => {
                let command = command_argument("WScript.Shell.Run", &args)?;
                let wait = args.get(1).map(|flag| flag.truthy()).unwrap_or(true);
                let mut child = Command::new("sh")
                    .arg("-c")
                    .arg(&command)
                    .stdin(Stdio::null())
                    .stdout(Stdio::inherit())
                    .stderr(Stdio::inherit())
                    .spawn()
                    .map_err(|e| BrtError::shell(format!("Run could not start: {e}")))?;
                if !wait {
                    return Ok(Variant::Int(0));
                }
                let status = child
                    .wait()
                    .map_err(|e| BrtError::shell(format!("Run failed: {e}")))?;
                Ok(Variant::Int(status.code().unwrap_or(-1) as i64))
            }
            // `Exec` captures both streams separately and never shows them.
            "exec" => {
                let command = command_argument("WScript.Shell.Exec", &args)?;
                let output = Command::new("sh")
                    .arg("-c")
                    .arg(&command)
                    .stdin(Stdio::null())
                    .output()
                    .map_err(|e| BrtError::shell(format!("Exec could not start: {e}")))?;
                let exec = ScriptExec {
                    stdout: trim_newline(&String::from_utf8_lossy(&output.stdout)),
                    stderr: trim_newline(&String::from_utf8_lossy(&output.stderr)),
                    exit_code: output.status.code().unwrap_or(-1) as i64,
                };
                Ok(Variant::Object(Rc::new(RefCell::new(exec))))
            }
            other => Err(BrtError::no_such_member(&self.type_name(), other)),
        }
    }

    fn get_property(&self, prop_name: &str) -> Result<Variant, BrtError> {
        Err(BrtError::no_such_member(&self.type_name(), prop_name))
    }
}

/// What `Exec` returns: a finished command's streams and status.
pub struct ScriptExec {
    stdout: String,
    stderr: String,
    exit_code: i64,
}

impl BartObject for ScriptExec {
    fn type_name(&self) -> String {
        "WshScriptExec".to_string()
    }

    fn member_kind(&self, name: &str) -> Member {
        match name.to_ascii_lowercase().as_str() {
            "stdout" | "stderr" | "exitcode" | "status" => Member::Property,
            _ => Member::Unknown,
        }
    }

    fn call_method(&mut self, method: &str, _args: Vec<Variant>) -> Result<Variant, BrtError> {
        Err(BrtError::no_such_member(&self.type_name(), method))
    }

    fn get_property(&self, prop_name: &str) -> Result<Variant, BrtError> {
        match prop_name.to_ascii_lowercase().as_str() {
            "stdout" => Ok(Variant::String(self.stdout.clone())),
            "stderr" => Ok(Variant::String(self.stderr.clone())),
            "exitcode" => Ok(Variant::Int(self.exit_code)),
            // Always 0: Bartelang's Exec waits for the command to finish, so
            // there is no "still running" state to report.
            "status" => Ok(Variant::Int(0)),
            other => Err(BrtError::no_such_member(&self.type_name(), other)),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn shell() -> WshShell {
        WshShell::new()
    }

    fn exec_object(value: &Variant) -> &Rc<RefCell<dyn BartObject>> {
        match value {
            Variant::Object(handle) => handle,
            other => panic!("expected an object, got {other:?}"),
        }
    }

    #[test]
    fn exec_captures_stdout_stderr_and_the_exit_code_together() {
        let mut shell = shell();
        let value = shell
            .call_method(
                "exec",
                vec![Variant::string("echo out; echo err >&2; exit 3")],
            )
            .unwrap();
        let exec = exec_object(&value).borrow();
        assert_eq!(exec.get_property("stdout").unwrap().display_string(), "out");
        assert_eq!(exec.get_property("stderr").unwrap().display_string(), "err");
        assert_eq!(exec.get_property("exitcode").unwrap().to_int().unwrap(), 3);
    }

    #[test]
    fn exec_strips_one_trailing_newline() {
        let mut shell = shell();
        let value = shell
            .call_method("exec", vec![Variant::string("printf 'a\\nb\\n'")])
            .unwrap();
        let exec = exec_object(&value).borrow();
        assert_eq!(exec.get_property("stdout").unwrap().display_string(), "a\nb");
    }

    #[test]
    fn run_returns_the_exit_code() {
        let mut shell = shell();
        let code = shell
            .call_method("run", vec![Variant::string("exit 7")])
            .unwrap();
        assert_eq!(code.to_int().unwrap(), 7);
    }

    #[test]
    fn unknown_members_are_reported() {
        let shell = shell();
        assert_eq!(
            shell.member_kind("nope"),
            Member::Unknown
        );
    }
}
