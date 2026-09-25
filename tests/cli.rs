//! End-to-end tests: they drive the real `bartelang` binary, not the library,
//! so the lexer, parser, interpreter, shell engine and CLI are all exercised
//! together.

use std::fs;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicUsize, Ordering};

static COUNTER: AtomicUsize = AtomicUsize::new(0);

struct Outcome {
    stdout: String,
    stderr: String,
    code: i32,
}

fn binary() -> &'static str {
    env!("CARGO_BIN_EXE_bartelang")
}

fn temp_dir(tag: &str) -> PathBuf {
    let id = COUNTER.fetch_add(1, Ordering::SeqCst);
    let dir = std::env::temp_dir().join(format!("bartelang-it-{}-{id}-{tag}", std::process::id()));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).expect("create the temporary script directory");
    dir
}

fn run(dir: &Path, source: &str, args: &[&str]) -> Outcome {
    let path = dir.join("script.btm");
    fs::write(&path, source).expect("write the script");
    let output = Command::new(binary())
        .arg("run")
        .arg(&path)
        .args(args)
        .current_dir(dir)
        .output()
        .expect("spawn bartelang");
    Outcome {
        stdout: String::from_utf8_lossy(&output.stdout).into_owned(),
        stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
        code: output.status.code().unwrap_or(-1),
    }
}

/// Runs an inline script in its own directory and asserts it succeeded.
fn run_ok(tag: &str, source: &str) -> Outcome {
    let dir = temp_dir(tag);
    let outcome = run(&dir, source, &[]);
    assert_eq!(
        outcome.code, 0,
        "expected success for {tag}\nstdout:\n{}\nstderr:\n{}",
        outcome.stdout, outcome.stderr
    );
    outcome
}

#[test]
fn arithmetic_concatenation_and_interpolation() {
    let outcome = run_ok(
        "arith",
        r#"
Dim count
Let count = 2 + 3 * 4
Dim label
Let label = "count=" & count & " items"
Debug.Print label
Debug.Print "interpolated: $count"
"#,
    );
    assert_eq!(outcome.stdout, "count=14 items\ninterpolated: 14\n");
}

#[test]
fn plus_is_maths_and_amp_is_text() {
    let outcome = run_ok(
        "plus",
        r#"
Debug.Print "5" + 3
Debug.Print 1 + 2 & 3
"#,
    );
    // "5" + 3 is numeric; 1 + 2 & 3 is (1+2) concatenated with 3, because &
    // binds looser than + in VB6.
    assert_eq!(outcome.stdout, "8\n33\n");
}

#[test]
fn case_insensitivity_and_optional_let() {
    let outcome = run_ok(
        "case",
        r#"
DIM x
dim y
LET x = 1
y = 2
DEBUG.PRINT "sum: " & X + y
"#,
    );
    assert_eq!(outcome.stdout, "sum: 3\n");
}

#[test]
fn command_substitution_sets_env_exit_code() {
    let outcome = run_ok(
        "shell",
        r#"
Dim who
Let who = `printf 'unix'`
Debug.Print "got: $who"
Dim ignored
Let ignored = `exit 7`
Debug.Print "exit code: " & ENV("?")
"#,
    );
    assert_eq!(outcome.stdout, "got: unix\nexit code: 7\n");
}

#[test]
fn cross_boundary_pipe_feeds_stdin() {
    let outcome = run_ok(
        "pipe",
        r#"
Dim shout
Let shout = "bartelang speaks unix" | `tr '[a-z]' '[A-Z]'`
Debug.Print shout
Dim counted
Let counted = "a" & CHR(10) & "b" & CHR(10) & "c" & CHR(10) | `wc -l`
Debug.Print TRIM(counted)
"#,
    );
    assert_eq!(outcome.stdout, "BARTELANG SPEAKS UNIX\n3\n");
}

#[test]
fn loops_and_branching() {
    let outcome = run_ok(
        "loops",
        r#"
Dim i
Dim total
Let total = 0
For i = 1 To 10
    Let total = total + i
Next i
Debug.Print "for total: $total"

For i = 3 To 1 Step -1
    Debug.Print "countdown $i"
Next

Let i = 0
While i < 2
    Let i = i + 1
Wend
Debug.Print "while: $i"

If total > 100 Then
    Debug.Print "huge"
ElseIf total > 50 Then
    Debug.Print "big"
Else
    Debug.Print "small"
End If
"#,
    );
    assert_eq!(
        outcome.stdout,
        "for total: 55\ncountdown 3\ncountdown 2\ncountdown 1\nwhile: 2\nbig\n"
    );
}

#[test]
fn inline_if_on_one_line() {
    let outcome = run_ok(
        "inline",
        "Dim x\nLet x = 5\nIf x > 3 Then Debug.Print \"yes\" Else Debug.Print \"no\"\n",
    );
    assert_eq!(outcome.stdout, "yes\n");
}

#[test]
fn subs_and_functions() {
    let outcome = run_ok(
        "procs",
        r#"
Dim calls
Let calls = 0

Sub Bump()
    Let calls = calls + 1
End Sub

Sub Banner(text)
    Debug.Print "== $text =="
End Sub

Function Fact(n)
    If n <= 1 Then
        Fact = 1
    Else
        Fact = n * Fact(n - 1)
    End If
End Function

Bump
Bump
Banner "arguments without parentheses"
Debug.Print "calls: $calls, 6! = " & Fact(6)
"#,
    );
    assert_eq!(
        outcome.stdout,
        "== arguments without parentheses ==\ncalls: 2, 6! = 720\n"
    );
}

#[test]
fn try_catch_exposes_the_err_object() {
    let outcome = run_ok(
        "trycatch",
        r#"
Try
    Dim boom
    Let boom = 1 / 0
    Debug.Print "unreachable"
Catch Err
    Debug.Print "number: " & Err.Number
    Debug.Print "message: " & Err.Message
    Debug.Print "description: " & Err.Description
End Try
Debug.Print "still running"
"#,
    );
    assert_eq!(
        outcome.stdout,
        "number: 11\nmessage: Division by zero\ndescription: Division by zero\nstill running\n"
    );
}

#[test]
fn try_catch_recovers_from_a_bad_shell_command() {
    let outcome = run_ok(
        "shellcatch",
        r#"
Try
    Dim http
    Set http = CreateObject("ADODB.Connection")
Catch e
    Debug.Print "caught " & e.Number
End Try
"#,
    );
    assert_eq!(outcome.stdout, "caught 429\n");
}

#[test]
fn arrays_are_one_based_and_bounds_checked() {
    let outcome = run_ok(
        "arrays",
        r#"
Dim slots(3)
Let slots(1) = "alpha"
Let slots(2) = "beta"
Let slots(3) = "gamma"
Dim i
For i = 1 To 3
    Debug.Print slots(i)
Next i
Try
    Let slots(0) = "nope"
Catch Err
    Debug.Print "blocked subscript, error " & Err.Number
End Try
"#,
    );
    assert_eq!(outcome.stdout, "alpha\nbeta\ngamma\nblocked subscript, error 9\n");
}

#[test]
fn vintage_file_channels_round_trip() {
    let dir = temp_dir("files");
    let outcome = run(
        &dir,
        r#"
Dim name
Let name = "bartelang"
Open "log.txt" For Output As #1
Print #1, "hello $name"
Print #1, "second line"
Close #1

Open "log.txt" For Input As #2
Dim line
While Not EOF(#2)
    Line Input #2, line
    Debug.Print "read: $line"
Wend
Close #2

Open "log.txt" For Append As #3
Print #3, "appended"
Close #3
"#,
        &[],
    );
    assert_eq!(outcome.code, 0, "stderr: {}", outcome.stderr);
    assert_eq!(
        outcome.stdout,
        "read: hello bartelang\nread: second line\n"
    );
    let contents = fs::read_to_string(dir.join("log.txt")).expect("log.txt exists");
    assert_eq!(contents, "hello bartelang\nsecond line\nappended\n");
}

#[test]
fn cli_arguments_are_one_based() {
    let dir = temp_dir("args");
    let outcome = run(&dir, "Debug.Print ARGS(1)\nDebug.Print ARGS(2)\nDebug.Print \"<\" & ARGS(9) & \">\"\n", &["first", "second"]);
    assert_eq!(outcome.code, 0, "stderr: {}", outcome.stderr);
    assert_eq!(outcome.stdout, "first\nsecond\n<>\n");
}

#[test]
fn type_mismatch_stops_the_script() {
    let dir = temp_dir("mismatch");
    let outcome = run(&dir, "Debug.Print \"abc\" + 1\nDebug.Print \"never\"\n", &[]);
    assert_eq!(outcome.code, 1);
    assert!(outcome.stdout.is_empty(), "stdout: {}", outcome.stdout);
    assert!(outcome.stderr.contains("error 13"), "stderr: {}", outcome.stderr);
    assert!(!outcome.stderr.contains("never"));
}

#[test]
fn syntax_errors_point_at_the_line() {
    let dir = temp_dir("syntax");
    let outcome = run(&dir, "Dim x\nLet x = (1 + 2\n", &[]);
    assert_eq!(outcome.code, 1);
    assert!(
        outcome.stderr.contains("syntax error in ")
            && outcome.stderr.contains("script.btm at line 2, column 15"),
        "stderr: {}",
        outcome.stderr
    );
    assert!(outcome.stderr.contains('^'), "stderr: {}", outcome.stderr);
    // The quoted source line and the caret line must line up.
    let lines: Vec<&str> = outcome.stderr.lines().collect();
    let carets = lines.last().expect("a caret line");
    assert_eq!(
        carets.find('^'),
        Some(lines[lines.len() - 2].len() - "Let x = (1 + 2".len() + 14),
        "stderr:\n{}",
        outcome.stderr
    );
}

#[test]
fn line_continuations_join_a_statement() {
    let outcome = run_ok(
        "continuation",
        "Dim s\nLet s = \"one\" _\n    & \" two\" _\n    & \" three\"\nDebug.Print s\n",
    );
    assert_eq!(outcome.stdout, "one two three\n");
}

#[test]
fn comments_and_blank_lines_are_ignored() {
    let outcome = run_ok(
        "comments",
        "' a leading comment\n\nDim x ' trailing comment\nLet x = 1\nDebug.Print x\n",
    );
    assert_eq!(outcome.stdout, "1\n");
}

#[test]
fn string_builtins_are_one_based() {
    let outcome = run_ok(
        "builtins",
        r#"
Debug.Print LEN("hello")
Debug.Print MID("abcdef", 2, 3)
Debug.Print LEFT("abcdef", 2) & "|" & RIGHT("abcdef", 2)
Debug.Print INSTR("banana", "na")
Debug.Print UCASE("quiet") & LCASE("LOUD")
Debug.Print REPLACE("a-b-c", "-", "+")
Debug.Print CHR(65) & ASC("B")
Debug.Print VAL("42 things") + 1
Debug.Print CINT(2.5) & "/" & CINT(3.5)
Debug.Print ISNUMERIC("12") & ISNUMERIC("abc")
"#,
    );
    assert_eq!(
        outcome.stdout,
        "5\nbcd\nab|ef\n3\nQUIETloud\na+b+c\nA66\n43\n2/4\nTrueFalse\n"
    );
}

#[test]
fn err_description_reports_unknown_object_types() {
    let outcome = run_ok(
        "unknown-object",
        r#"
Try
    Dim d
    Set d = CreateObject("ADODB.Connection")
Catch Err
    Debug.Print Err.Number
    Debug.Print Err.Message
End Try
"#,
    );
    assert_eq!(
        outcome.stdout,
        "429\nCannot create object \"ADODB.Connection\" (known types: HTTP, \
Scripting.FileSystemObject (FS), Scripting.Dictionary (DICT), Collection (COLL, LIST), \
VBScript.RegExp (REGEX), WScript.Shell (PROC))\n"
    );
}

#[test]
fn booleans_follow_vba_semantics() {
    let outcome = run_ok(
        "bool",
        r#"
Debug.Print CInt(True)
Debug.Print CStr(False)
If "" Then
    Debug.Print "empty is true"
Else
    Debug.Print "empty is false"
End If
If 0 Then
    Debug.Print "zero is true"
Else
    Debug.Print "zero is false"
End If
Debug.Print TRUE AND FALSE
Debug.Print NOT 0
"#,
    );
    assert_eq!(
        outcome.stdout,
        "-1\nFalse\nempty is false\nzero is false\nFalse\nTrue\n"
    );
}

#[test]
fn parse_subcommand_dumps_the_ast() {
    let dir = temp_dir("parse");
    let path = dir.join("sample.btm");
    fs::write(&path, "Function Add(a, b)\n  Add = a + b\nEnd Function\n").unwrap();
    let output = Command::new(binary())
        .arg("parse")
        .arg(&path)
        .output()
        .expect("spawn bartelang");
    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("FuncDeclaration"), "stdout: {stdout}");
    assert!(stdout.contains("BinaryOp"), "stdout: {stdout}");
}

#[test]
fn version_and_usage_are_available() {
    let version = Command::new(binary()).arg("version").output().unwrap();
    assert!(String::from_utf8_lossy(&version.stdout).contains("bartelang"));

    let usage = Command::new(binary()).output().unwrap();
    assert_eq!(usage.status.code(), Some(2));
    assert!(String::from_utf8_lossy(&usage.stdout).contains("USAGE"));
}

#[test]
fn the_shipped_tour_example_runs() {
    let examples = Path::new(env!("CARGO_MANIFEST_DIR")).join("examples");
    let dir = temp_dir("tour");
    let output = Command::new(binary())
        .arg("run")
        .arg(examples.join("tour.btm"))
        .current_dir(&dir)
        .output()
        .expect("spawn bartelang");
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert_eq!(output.status.code(), Some(0), "stdout:\n{stdout}\nstderr:\n{stderr}");
    assert!(stdout.contains("for step: 5"), "stdout:\n{stdout}");
    assert!(stdout.contains("triple(7) = 21"), "stdout:\n{stdout}");
    assert!(stdout.contains("slot 3: gamma"), "stdout:\n{stdout}");
    assert!(stdout.contains("BARTELANG SPEAKS UNIX"), "stdout:\n{stdout}");
    assert!(stdout.contains("caught error 11"), "stdout:\n{stdout}");
    assert!(stdout.contains("done"), "stdout:\n{stdout}");
    assert!(dir.join("tour_output.txt").is_file());
}

#[test]
fn a_large_pipeline_payload_does_not_deadlock() {
    // 100000 lines of "abcdefghij\n" is 1100000 bytes; the backtick strips the
    // final newline, so the piped payload is exactly 1099999 bytes.  If stdin
    // writing and stdout reading ever raced, this would hang.
    let outcome = run_ok(
        "bigpipe",
        "Dim big\nLet big = `yes abcdefghij | head -n 100000`\nDim n\nLet n = big | `wc -c`\nDebug.Print TRIM(n)\n",
    );
    assert_eq!(outcome.stdout, "1099999\n");
}

#[test]
fn documented_call_forms_and_object_members_work() {
    // Every call syntax quoted in the README, plus the HTTP object member
    // surface that does not need a network round trip.
    let outcome = run_ok(
        "callforms",
        r#"
Sub Bump(n)
    Debug.Print "bump " & n
End Sub

Bump 3
Bump(4)
Call Bump(5)
Dim f
Let f = 6
Bump f

Dim http
Set http = CreateObject("HTTP")
http.SetTimeouts 5000
http.SetRequestHeader "Accept", "text/plain"
http.Open "GET", "https://example.invalid/", False
http.Abort
Debug.Print "readystate after Abort: " & http.ReadyState
Debug.Print "headers: [" & http.GetAllResponseHeaders & "]"
Debug.Print "status before send: " & http.Status
"#,
    );
    assert_eq!(
        outcome.stdout,
        "bump 3\nbump 4\nbump 5\nbump 6\nreadystate after Abort: 0\nheaders: []\nstatus before send: 0\n"
    );
}

#[test]
fn input_box_reads_a_line_from_stdin() {
    let dir = temp_dir("inputbox");
    let path = dir.join("prompt.btm");
    fs::write(
        &path,
        "Dim city\nLet city = InputBox(\"Enter city: \")\nDebug.Print \"got [$city]\"\n",
    )
    .unwrap();

    let mut child = Command::new(binary())
        .arg("run")
        .arg(&path)
        .current_dir(&dir)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn bartelang");
    child
        .stdin
        .take()
        .expect("child stdin")
        .write_all(b"Delft\n")
        .expect("feed stdin");

    let output = child.wait_with_output().expect("wait for bartelang");
    assert_eq!(output.status.code(), Some(0));
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert_eq!(stdout, "Enter city: got [Delft]\n");
}

#[test]
fn env_reads_process_variables_and_the_exit_code() {
    let dir = temp_dir("env");
    let path = dir.join("env.btm");
    fs::write(
        &path,
        "Debug.Print ENV(\"BARTELANG_TEST_VAR\")\nDebug.Print ENV(\"?\")\n",
    )
    .unwrap();
    let output = Command::new(binary())
        .arg("run")
        .arg(&path)
        .current_dir(&dir)
        .env("BARTELANG_TEST_VAR", "from-the-environment")
        .output()
        .expect("spawn bartelang");
    assert_eq!(output.status.code(), Some(0));
    // No shell command has run, so the tracked exit code starts at 0.
    assert_eq!(
        String::from_utf8_lossy(&output.stdout),
        "from-the-environment\n0\n"
    );
}

#[test]
fn numeric_and_string_edges_match_vb6() {
    let outcome = run_ok(
        "edges",
        r#"
Debug.Print 7 Mod 4
Debug.Print -7 Mod 3
Debug.Print 7 \ 2
Debug.Print 2 ^ 10
Debug.Print Int(2.5) & " " & Int(-2.5)
Debug.Print Fix(2.5) & " " & Fix(-2.5)
Debug.Print Sgn(-9) & Sgn(0) & Sgn(9)
Debug.Print Sqr(16) & " " & Hex(255) & " " & Oct(8)
Debug.Print Space(2) & "|"
Debug.Print String(3, "*")
Debug.Print InStr(3, "banana", "na") & "/" & InStrRev("banana", "na")
Debug.Print Replace("aaa", "a", "bb")
Debug.Print Mid("abcdef", 3)
Debug.Print Len("")
Debug.Print Val("  12.5xyz") + 0.5
Debug.Print "[" & Trim("  padded  ") & "]"
Debug.Print "abc" = "ABC"
Debug.Print "10" > "9"
Debug.Print IsEmpty(Nothing) & "/" & IsNumeric("3.5")
"#,
    );
    assert_eq!(
        outcome.stdout,
        "3\n-1\n3\n1024\n2 -3\n2 -2\n-101\n4 FF 10\n  |\n***\n3/5\nbbbbbb\ncdef\n0\n13\n[padded]\nFalse\nFalse\nTrue/True\n"
    );
}

#[test]
fn local_state_is_scoped_to_a_procedure() {
    let outcome = run_ok(
        "scoping",
        r#"
Function SumSlots()
    Dim slots(4)
    Let slots(1) = 1
    Let slots(2) = 2
    Let slots(3) = 3
    Let slots(4) = 4
    SumSlots = slots(1) + slots(2) + slots(3) + slots(4)
End Function
Debug.Print SumSlots()
Debug.Print SumSlots()
Try
    Debug.Print slots(1)
Catch Err
    Debug.Print "outside scope: " & Err.Number
End Try
"#,
    );
    // Arrays are ordinary variables now, so a local one that is out of scope is
    // an undefined variable (500) rather than the old array-specific 9.
    assert_eq!(outcome.stdout, "10\n10\noutside scope: 500\n");
}

#[test]
fn every_runtime_failure_is_catchable() {
    let outcome = run_ok(
        "errors",
        r#"
Function Forever(n)
    Forever = Forever(n + 1)
End Function

Sub NeverCalled()
    Debug.Print "unused"
End Sub

Try
    Set thing = 42
Catch Err
    Debug.Print "non-object Set: " & Err.Number
End Try

Try
    Dim r
    Let r = Forever(1)
Catch Err
    Debug.Print "runaway recursion: " & Err.Number
End Try

Try
    Dim q
    Let q = NoSuchThing(1)
Catch Err
    Debug.Print "unknown procedure: " & Err.Number
End Try

Try
    Print #7, "nope"
Catch Err
    Debug.Print "unopened channel: " & Err.Number
End Try

Try
    Dim d
    Let d = CreateObject("ADODB.Connection")
Catch Err
    Debug.Print "unknown object type: " & Err.Number
End Try

Try
    Dim z
    Let z = 1 / 0
Catch Err
    Debug.Print "division by zero: " & Err.Number
End Try

Debug.Print "alive"
"#,
    );
    assert_eq!(
        outcome.stdout,
        "non-object Set: 424\nrunaway recursion: 1003\nunknown procedure: 35\n\
unopened channel: 52\nunknown object type: 429\ndivision by zero: 11\nalive\n"
    );
}

#[test]
fn date_and_time_have_vb_shapes() {
    let outcome = run_ok(
        "dates",
        "Debug.Print Len(Now())\nDebug.Print Len(Date())\nDebug.Print Len(Time())\nDebug.Print Left(Now(), 4) > \"1900\"\n",
    );
    assert_eq!(outcome.stdout, "19\n10\n8\nTrue\n");
}

#[test]
fn a_closed_stdout_pipe_is_not_a_panic() {
    let dir = temp_dir("sigpipe");
    let path = dir.join("loud.btm");
    let source = "Dim i\nFor i = 1 To 20000\n  Debug.Print \"a fairly long line of output\"\nNext\n";
    fs::write(&path, source).unwrap();

    let mut child = Command::new(binary())
        .arg("run")
        .arg(&path)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn bartelang");

    // Read one byte, then abandon the pipe the way `head -c1` would.
    if let Some(mut stdout) = child.stdout.take() {
        let mut byte = [0u8; 1];
        let _ = stdout.read(&mut byte);
    }
    let output = child.wait_with_output().expect("wait for bartelang");
    let stderr = String::from_utf8_lossy(&output.stderr);

    assert!(
        !stderr.contains("panicked"),
        "interpreter panicked on a closed pipe:\n{stderr}"
    );
    assert_ne!(
        output.status.code(),
        Some(101),
        "Rust panic exit code seen; stderr:\n{stderr}"
    );
}

#[test]
fn the_shipped_log_stats_example_runs() {
    let examples = Path::new(env!("CARGO_MANIFEST_DIR")).join("examples");
    let dir = temp_dir("log-stats");
    let output = Command::new(binary())
        .arg("run")
        .arg(examples.join("log_stats.btm"))
        .current_dir(&dir)
        .output()
        .expect("spawn bartelang");
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert_eq!(
        output.status.code(),
        Some(0),
        "stdout:\n{stdout}\nstderr:\n{stderr}"
    );
    // The example writes its own sample log, so it is deterministic.
    assert!(stdout.contains("200 x3"), "stdout:\n{stdout}");
    assert!(stdout.contains("404 x1"), "stdout:\n{stdout}");
    assert!(stdout.contains("/index x2"), "stdout:\n{stdout}");
    assert!(
        stdout.contains("records: 5, skipped: 1"),
        "stdout:\n{stdout}"
    );
}

#[test]
fn the_parse_dump_survives_a_closed_pipe() {
    let examples = Path::new(env!("CARGO_MANIFEST_DIR")).join("examples");
    let mut child = Command::new(binary())
        .arg("parse")
        .arg(examples.join("sys_fetch.btm"))
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn bartelang");
    if let Some(mut stdout) = child.stdout.take() {
        let mut byte = [0u8; 1];
        let _ = stdout.read(&mut byte);
    }
    let output = child.wait_with_output().expect("wait for bartelang");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(!stderr.contains("panicked"), "stderr:\n{stderr}");
}

#[test]
fn milestone_one_control_flow_end_to_end() {
    let outcome = run_ok(
        "flow",
        r#"
Const Limit = 5

Sub Classify(n)
    Select Case n
    Case Is < 0
        Debug.Print "negative"
    Case 0 To Limit
        Debug.Print "in range"
    Case Else
        Debug.Print "too big"
    End Select
End Sub

Dim i
For i = -1 To 9
    Classify i
Next i

Dim n
Let n = 0
Do
    Let n = n + 1
    If n = 3 Then
        Exit Do
    End If
Loop
Debug.Print "do loop stopped at $n"

Function Guarded(v)
    If v < 0 Then
        Err.Raise 8001, "Guarded", "negative input"
    End If
    Guarded = v * 2
End Function

Try
    Debug.Print "guarded: " & Guarded(4)
    Debug.Print "guarded: " & Guarded(-1)
Catch Err
    Debug.Print "caught " & Err.Number & " from " & Err.Source
End Try

Debug.Print IIf(Limit > 3, "limit is big", "limit is small")
Debug.Print "done"
"#,
    );
    assert_eq!(
        outcome.stdout,
        "negative\nin range\nin range\nin range\nin range\nin range\nin range\n\
too big\ntoo big\ntoo big\ntoo big\ndo loop stopped at 3\nguarded: 8\n\
caught 8001 from Guarded\nlimit is big\ndone\n"
    );
}

#[test]
fn with_blocks_drive_an_object_end_to_end() {
    let outcome = run_ok(
        "with-block",
        r#"
Dim http
Set http = CreateObject("HTTP")
With http
    .Open "GET", "https://example.invalid/", False
    .SetRequestHeader "Accept", "text/plain"
    If .ReadyState <> 1 Then
        Err.Raise 9001, "with-test", "ReadyState should be 1 after Open"
    End If
End With

Debug.Print "readystate outside the block: " & http.ReadyState
"#,
    );
    assert_eq!(outcome.stdout, "readystate outside the block: 1\n");
}

#[test]
fn splitting_shell_output_end_to_end() {
    let outcome = run_ok(
        "split-e2e",
        r#"
Dim listing
Let listing = `printf 'alpha 1\nbeta 22\ngamma 333'`

Dim lines
Let lines = Split(listing, CHR(10))
Debug.Print "lines: " & UBound(lines)

Dim total
Let total = 0
Dim line
For Each line In lines
    Dim fields
    Let fields = Split(line, " ")
    Debug.Print fields(1) & " -> " & fields(2)
    Let total = total + CInt(fields(2))
Next

Debug.Print "total: $total"

Dim report
Let report = Join(lines, " | ")
Debug.Print report
"#,
    );
    assert_eq!(
        outcome.stdout,
        "lines: 3\nalpha -> 1\nbeta -> 22\ngamma -> 333\ntotal: 356\n\
alpha 1 | beta 22 | gamma 333\n"
    );
}

#[test]
fn filesystem_object_and_text_stream_end_to_end() {
    let dir = temp_dir("fso");
    let outcome = run(
        &dir,
        r#"
Dim fso
Set fso = CreateObject("Scripting.FileSystemObject")

Debug.Print "before: " & fso.FileExists("data/notes.txt")
fso.CreateFolder "data"

Dim out
Set out = fso.CreateTextFile("data/notes.txt", True)
out.WriteLine "first"
out.WriteLine "second"
out.Close

Debug.Print "exists: " & fso.FileExists("data/notes.txt")
Debug.Print "size: " & fso.FileSize("data/notes.txt")
Debug.Print "base: " & fso.GetBaseName("data/notes.txt")
Debug.Print "ext: " & fso.GetExtensionName("data/notes.txt")
Debug.Print "parent: " & fso.GetParentFolderName("data/notes.txt")
Debug.Print "join: " & fso.BuildPath("data", "other.txt")

Dim input
Set input = fso.OpenTextFile("data/notes.txt", 1)
Dim text
Let text = input.ReadAll()
Debug.Print "len: " & Len(text)
Debug.Print "at end: " & input.AtEndOfStream
input.Close

fso.CopyFile "data/notes.txt", "data/copy.txt"
fso.MoveFile "data/copy.txt", "data/moved.txt"
Debug.Print "moved: " & fso.FileExists("data/moved.txt")
fso.DeleteFile "data/moved.txt"
Debug.Print "deleted: " & fso.FileExists("data/moved.txt")

fso.DeleteFolder "data", True
Debug.Print "folder gone: " & fso.FolderExists("data")
"#,
        &[],
    );
    assert_eq!(outcome.code, 0, "stderr: {}", outcome.stderr);
    assert_eq!(
        outcome.stdout,
        "before: False\nexists: True\nsize: 13\nbase: notes\next: txt\n\
parent: data\njoin: data/other.txt\nlen: 13\nat end: True\nmoved: True\n\
deleted: False\nfolder gone: False\n"
    );
    assert!(!dir.join("data").exists());
}

#[test]
fn dictionary_and_regex_end_to_end() {
    let outcome = run_ok(
        "dict-regex",
        r#"
Dim log
Let log = `printf 'GET /a 200\nPOST /b 500\nGET /c 200\nGET /d 404\nPOST /e 200'`

Dim tally
Set tally = CreateObject("Scripting.Dictionary")

Dim line
For Each line In Split(log, CHR(10))
    Dim fields
    Let fields = Split(line, " ")
    Dim status
    Let status = fields(3)
    If tally.Exists(status) Then
        tally(status) = tally(status) + 1
    Else
        tally.Add status, 1
    End If
Next

Debug.Print "distinct: " & tally.Count
Dim key
For Each key In tally.Keys
    Debug.Print key & " -> " & tally(key)
Next

Dim re
Set re = CreateObject("VBScript.RegExp")
re.Pattern = "^(GET|POST) (/[a-z]+) (\d+)$"
re.Global = True
Debug.Print "matches: " & re.Test("GET /a 200")
Debug.Print "rewritten: " & re.Replace("GET /a 200", "$3 $1")
"#,
    );
    assert_eq!(
        outcome.stdout,
        "distinct: 3\n200 -> 3\n500 -> 1\n404 -> 1\nmatches: True\nrewritten: 200 GET\n"
    );
}

#[test]
fn wscript_shell_captures_stdout_stderr_and_the_exit_code() {
    let outcome = run_ok(
        "wshshell",
        r#"
Dim shell
Set shell = CreateObject("WScript.Shell")

Dim result
Set result = shell.Exec("echo out; echo err 1>&2; exit 9")
Debug.Print "stdout: " & result.StdOut
Debug.Print "stderr: " & result.StdErr
Debug.Print "exit: " & result.ExitCode
Debug.Print "status: " & result.Status

Debug.Print "run returned: " & shell.Run("exit 4")
"#,
    );
    assert_eq!(
        outcome.stdout,
        "stdout: out\nstderr: err\nexit: 9\nstatus: 0\nrun returned: 4\n"
    );
}

#[test]
fn collection_is_one_based_end_to_end() {
    let outcome = run_ok(
        "collection",
        r#"
Dim items
Set items = CreateObject("Collection")
items.Add "first"
items.Add "second", "two"
items.Add "third"

Debug.Print "count: " & items.Count
Debug.Print "by index: " & items(1) & ", " & items(3)
Debug.Print "by key: " & items("two")
items.Remove 1
Debug.Print "after remove: " & items(1) & " (count " & items.Count & ")"
"#,
    );
    assert_eq!(
        outcome.stdout,
        "count: 3\nby index: first, third\nby key: second\nafter remove: second (count 2)\n"
    );
}

#[test]
fn now_reports_local_wall_clock_time() {
    let dir = temp_dir("clock");
    let outcome = run(&dir, "Debug.Print Left(Now(), 10)\n", &[]);
    assert_eq!(outcome.code, 0, "stderr: {}", outcome.stderr);

    let from_shell = Command::new("date")
        .arg("+%Y-%m-%d")
        .output()
        .expect("run date");
    let expected = String::from_utf8_lossy(&from_shell.stdout).trim().to_string();
    // This is the check that catches treating UTC seconds as wall-clock time;
    // it can only flake if the run straddles midnight.
    assert_eq!(outcome.stdout.trim(), expected);
}

#[test]
fn numeric_library_end_to_end() {
    let outcome = run_ok(
        "numbers",
        r##"
Debug.Print Format(1234567.891, "#,##0.00")
Debug.Print Format(0.5, "Percent")
Debug.Print Format(-3.5, "$0.00")
Debug.Print Format(12345, "Scientific")
Debug.Print Format(True, "Yes/No")
Debug.Print Round(2.5) & " " & Round(3.5) & " " & Round(1.2345, 2)

Randomize
Dim value
Let value = Rnd()
If value >= 0 And value < 1 Then
    Debug.Print "rnd in range"
End If

Dim started
Let started = Timer()
If started >= 0 And started < 86400 Then
    Debug.Print "timer in range"
End If
"##,
    );
    assert_eq!(
        outcome.stdout,
        "1,234,567.89\n50.00%\n$-3.50\n1.23E+04\nYes\n2 4 1.23\nrnd in range\ntimer in range\n"
    );
}

#[test]
fn date_arithmetic_end_to_end() {
    let outcome = run_ok(
        "dates",
        r#"
Dim stamp
Let stamp = "2026-01-31 12:00:00"

Debug.Print DateAdd("d", 1, stamp)
Debug.Print DateAdd("m", 1, stamp)
Debug.Print DateAdd("yyyy", -1, stamp)
Debug.Print DateDiff("d", "2026-01-01", "2026-03-01")
Debug.Print DateDiff("m", "2026-01-31", "2026-02-01")
Debug.Print DatePart("yyyy", stamp) & "-" & DatePart("m", stamp)
Debug.Print DatePart("d", stamp) & "/" & DatePart("h", stamp) & "/" & DatePart("n", stamp)
Debug.Print IsDate(stamp) & " " & IsDate("nope")
Debug.Print Format(stamp, "yyyy-mm-dd hh:nn:ss")
Debug.Print Format(stamp, "dddd, mmmm d, yyyy")
Debug.Print Format(stamp, "h:nn AM/PM")
"#,
    );
    assert_eq!(
        outcome.stdout,
        "2026-02-01 12:00:00\n2026-02-28 12:00:00\n2025-01-31 12:00:00\n59\n1\n2026-1\n\
31/12/0\nTrue False\n2026-01-31 12:00:00\nSaturday, January 31, 2026\n12:00 PM\n"
    );
}

#[test]
fn introspection_end_to_end() {
    let outcome = run_ok(
        "introspection",
        r#"
Dim n
Let n = 5
Dim s
Let s = "text"
Dim a
Let a = Array(1, 2)
Dim o
Set o = CreateObject("Collection")
Dim unset

Debug.Print TypeName(n) & " " & TypeName(s) & " " & TypeName(a) & " " & TypeName(o) & " " & TypeName(unset)
Debug.Print IsArray(a) & " " & IsArray(n)
Debug.Print IsObject(o) & " " & IsObject(n)
Debug.Print IsNull(Null) & " " & IsNull(unset) & " " & IsEmpty(unset)
"#,
    );
    assert_eq!(
        outcome.stdout,
        "Integer String Array Collection Empty\nTrue False\nTrue False\nTrue False True\n"
    );
}

#[test]
fn using_null_in_a_comparison_is_reported() {
    let dir = temp_dir("null-use");
    let outcome = run(&dir, "Dim x\nLet x = Null\nIf x = Null Then\nEnd If\n", &[]);
    assert_eq!(outcome.code, 1);
    assert!(outcome.stderr.contains("error 94"), "stderr: {}", outcome.stderr);
    assert!(outcome.stderr.contains("IsNull"), "stderr: {}", outcome.stderr);
}

#[test]
fn record_file_channels_end_to_end() {
    let dir = temp_dir("records");
    let outcome = run(
        &dir,
        r##"
Dim channel
Let channel = FreeFile()
Open "records.txt" For Output As #channel
Write #channel, "alpha", 42, 1.5, True
Write #channel, "with,comma", 7, False
Write #channel, Null
Print #channel, "a"; "b"; "c"
Print #channel, "zone1", "zone2"
Close #channel

Open "records.txt" For Input As #1
Dim size
Let size = LOF(1)
Dim name
Dim count
Dim ratio
Dim flag
Input #1, name, count, ratio, flag
Debug.Print "$name|$count|$ratio|$flag"
Input #1, name, count, flag
Debug.Print "$name|$count|$flag"
Input #1, name
Debug.Print "recorded null: " & IsNull(name)
Dim line
Line Input #1, line
Debug.Print "[$line]"
Line Input #1, line
Debug.Print "[$line]"
Debug.Print "at end: " & EOF(1)
Debug.Print "has bytes: " & (size > 0)
Close #1
"##,
        &[],
    );
    assert_eq!(outcome.code, 0, "stderr: {}", outcome.stderr);
    assert_eq!(
        outcome.stdout,
        "alpha|42|1.5|True\nwith,comma|7|False\nrecorded null: True\n[abc]\n[zone1\tzone2]\n\
at end: True\nhas bytes: True\n"
    );
}

#[test]
fn records_end_to_end() {
    let outcome = run_ok(
        "records",
        r##"
Type Point
    X As Double
    Y As Double
End Type

Sub Shift(p)
    ' Records are values, so this must not touch the caller's point.
    p.X = 99
End Sub

Dim a As Point
a.X = 1
a.Y = 2
Debug.Print "a: " & a.X & "," & a.Y
Debug.Print "type: " & TypeName(a)

Dim b As Point
Let b = a
b.X = 5
Debug.Print "a.X after copying: " & a.X
Debug.Print "b.X: " & b.X

Shift a
Debug.Print "a.X after a procedure: " & a.X

With a
    .X = 7
    .Y = 8
End With
Debug.Print "with: " & a.X & "," & a.Y

Try
    Debug.Print a.Z
Catch Err
    Debug.Print "unknown field: " & Err.Number
End Try

' A declaration that precedes the Type block still works: definitions are
' hoisted, like procedures.
Dim late As LatePoint
late.Value = 3
Debug.Print "late: " & late.Value

Type LatePoint
    Value As Integer
    Tags(2) As String
End Type
"##,
    );
    assert_eq!(
        outcome.stdout,
        "a: 1,2\ntype: point\na.X after copying: 1\nb.X: 5\n\
a.X after a procedure: 1\nwith: 7,8\nunknown field: 438\nlate: 3\n"
    );
}

#[test]
fn a_record_can_be_returned_from_a_function() {
    let outcome = run_ok(
        "record-return",
        r##"
Type Point
    X As Integer
End Type

Function Origin()
    Dim p As Point
    p.X = 42
    Origin = p
End Function

Dim got As Point
Let got = Origin()
Debug.Print "returned: " & got.X

Dim bag(2) As Point
Debug.Print "array of records: " & IsEmpty(bag(1).X)
"##,
    );
    assert_eq!(outcome.stdout, "returned: 42\narray of records: True\n");
}

#[test]
fn multi_line_strings_end_to_end() {
    let outcome = run_ok(
        "multiline",
        r##"
Dim name
Let name = "world"

Dim report
Let report = """
Hello $name,
this is line two.
"""

Debug.Print Len(report)
Debug.Print "[" & Trim(report) & "]"
"##,
    );
    assert_eq!(
        outcome.stdout,
        "32\n[Hello world,\nthis is line two.]\n"
    );
}

#[test]
fn multi_line_strings_can_build_a_config_file() {
    let dir = temp_dir("multiline-file");
    let outcome = run(
        &dir,
        r##"
Dim host
Let host = "example.invalid"
Dim port
Let port = 8080

Dim config
Let config = """
[server]
host = $host
port = $port
"""

Open "server.conf" For Output As #1
Print #1, Trim(config)
Close #1
"##,
        &[],
    );
    assert_eq!(outcome.code, 0, "stderr: {}", outcome.stderr);
    let written = std::fs::read_to_string(dir.join("server.conf")).expect("server.conf");
    assert_eq!(
        written,
        "[server]\nhost = example.invalid\nport = 8080\n"
    );
}

#[test]
fn runtime_errors_report_the_failing_line() {
    let dir = temp_dir("error-line");
    let outcome = run(&dir, "Dim x\nLet x = 1\nLet y = 1 / 0\n", &[]);
    assert_eq!(outcome.code, 1);
    assert!(
        outcome.stderr.contains("runtime error 11 in ")
            && outcome.stderr.contains("script.btm at line 3"),
        "stderr: {}",
        outcome.stderr
    );
    // The offending source line is quoted back, as syntax errors already were.
    assert!(
        outcome.stderr.contains("3 | Let y = 1 / 0"),
        "stderr: {}",
        outcome.stderr
    );
}

#[test]
fn a_failure_inside_a_procedure_reports_that_procedures_line() {
    let dir = temp_dir("error-line-nested");
    // The caller is on line 8, but the division that failed is on line 3.
    let outcome = run(
        &dir,
        "Sub Boom()\n    Dim y\n    Let y = 1 / 0\nEnd Sub\n\nDim ok\nLet ok = 1\nBoom\n",
        &[],
    );
    assert_eq!(outcome.code, 1);
    assert!(
        outcome.stderr.contains("at line 3"),
        "stderr: {}",
        outcome.stderr
    );
    assert!(
        !outcome.stderr.contains("at line 8"),
        "stderr reported the call site instead: {}",
        outcome.stderr
    );
}

#[test]
fn err_line_is_available_inside_a_catch_block() {
    let outcome = run_ok(
        "err-line",
        r#"
Dim where
Try
    Dim y
    Let y = 1 / 0
Catch Err
    Let where = Err.Line
End Try
Debug.Print "line was " & where

' The Err object keeps the last error until something replaces it or
' Err.Clear runs, as in VB6.
Try
    Dim z
    Let z = 1 / 0
Catch Err
End Try
Debug.Print "still " & Err.Line
Err.Clear
Debug.Print "after Clear: " & Err.Line
"#,
    );
    assert_eq!(
        outcome.stdout,
        "line was 5\nstill 15\nafter Clear: 0\n"
    );
}

#[test]
fn object_aliases_work_case_insensitively_end_to_end() {
    let outcome = run_ok(
        "aliases",
        r#"
Dim fs
Set fs = CreateObject("fs")
Debug.Print TypeName(fs)

Dim dict
Set dict = CreateObject("DiCt")
dict.Add "k", 1
Debug.Print "dict count: " & dict.Count

Dim re
Set re = CreateObject("regex")
re.Pattern = "a"
Debug.Print "regex test: " & re.Test("banana")

Dim shell
Set shell = CreateObject("proc")
Dim result
Set result = shell.Exec("echo via-proc")
Debug.Print "proc: " & result.StdOut

Dim items
Set items = CreateObject("list")
items.Add "first"
Debug.Print "list count: " & items.Count

' The canonical names still work too.
Dim full
Set full = CreateObject("Scripting.FileSystemObject")
Debug.Print "canonical: " & TypeName(full)
"#,
    );
    assert_eq!(
        outcome.stdout,
        "Scripting.FileSystemObject\ndict count: 1\nregex test: True\n\
proc: via-proc\nlist count: 1\ncanonical: Scripting.FileSystemObject\n"
    );
}

#[test]
fn a_const_cannot_be_reassigned_end_to_end() {
    let dir = temp_dir("const");
    let outcome = run(&dir, "Const Limit = 3\nLet Limit = 4\n", &[]);
    assert_eq!(outcome.code, 1);
    assert!(
        outcome.stderr.contains("error 501"),
        "stderr: {}",
        outcome.stderr
    );
}

#[test]
fn a_mistargeted_exit_reports_which_loop_it_is_in() {
    let dir = temp_dir("exit-mismatch");
    let outcome = run(&dir, "Do\n  Exit For\nLoop\n", &[]);
    assert_eq!(outcome.code, 1);
    assert!(
        outcome.stderr.contains("Exit For used inside a Do...Loop"),
        "stderr: {}",
        outcome.stderr
    );
}

// --------------------------------------------------------------- includes

/// Writes a file under `dir`, creating parent directories, and returns its path.
fn write_file(dir: &Path, name: &str, contents: &str) -> PathBuf {
    let path = dir.join(name);
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).expect("create parent directory");
    }
    fs::write(&path, contents).expect("write file");
    path
}

/// Runs an existing script path, with a chosen working directory and a clean
/// environment (unless the test supplies `$BARTELANG_PATH` itself).
fn run_script(dir: &Path, script: &Path, args: &[&str], env: &[(&str, &str)]) -> Outcome {
    let mut command = Command::new(binary());
    command
        .arg("run")
        .arg(script)
        .args(args)
        .current_dir(dir)
        .env_remove("BARTELANG_PATH");
    for (key, value) in env {
        command.env(key, value);
    }
    let output = command.output().expect("spawn bartelang");
    Outcome {
        stdout: String::from_utf8_lossy(&output.stdout).into_owned(),
        stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
        code: output.status.code().unwrap_or(-1),
    }
}

#[test]
fn includes_share_procedures_both_ways() {
    let dir = temp_dir("include-share");
    // `Four` lives in the library and calls `Doubled`, which lives in the entry
    // script: the two files are one program, so a procedure may be on either
    // side of the include.
    write_file(
        &dir,
        "lib.btm",
        "Function Twice(n)\n    Twice = n * 2\nEnd Function\n\nFunction Four(n)\n    Four = Doubled(Twice(n))\nEnd Function\n",
    );
    write_file(
        &dir,
        "script.btm",
        "Include \"lib.btm\"\n\nFunction Doubled(n)\n    Doubled = n * 2\nEnd Function\n\nDebug.Print Four(3)\n",
    );
    let outcome = run_script(&dir, &dir.join("script.btm"), &[], &[]);
    assert_eq!(outcome.code, 0, "stderr: {}", outcome.stderr);
    assert_eq!(outcome.stdout, "12\n");
}

#[test]
fn a_file_is_included_once_even_in_a_diamond() {
    let dir = temp_dir("include-diamond");
    write_file(
        &dir,
        "lib.btm",
        "Debug.Print \"lib init\"\nFunction Tag()\n    Tag = \"tagged\"\nEnd Function\n",
    );
    write_file(&dir, "left.btm", "Include \"lib.btm\"\nDebug.Print \"left\"\n");
    write_file(&dir, "right.btm", "Include \"lib.btm\"\nDebug.Print \"right\"\n");
    write_file(
        &dir,
        "script.btm",
        "Include \"left.btm\"\nInclude \"right.btm\"\nDebug.Print Tag()\n",
    );
    let outcome = run_script(&dir, &dir.join("script.btm"), &[], &[]);
    assert_eq!(outcome.code, 0, "stderr: {}", outcome.stderr);
    assert_eq!(outcome.stdout, "lib init\nleft\nright\ntagged\n");
}

#[test]
fn includes_resolve_next_to_the_script_not_the_shell() {
    let dir = temp_dir("include-relative");
    // A decoy in the working directory: the script's own directory wins.
    write_file(
        &dir,
        "lib.btm",
        "Function Where()\n    Where = \"shell\"\nEnd Function\n",
    );
    write_file(
        &dir,
        "sub/lib.btm",
        "Function Where()\n    Where = \"script\"\nEnd Function\n",
    );
    write_file(
        &dir,
        "sub/script.btm",
        "Include \"lib.btm\"\nDebug.Print Where()\n",
    );
    let outcome = run_script(&dir, &dir.join("sub/script.btm"), &[], &[]);
    assert_eq!(outcome.code, 0, "stderr: {}", outcome.stderr);
    assert_eq!(outcome.stdout, "script\n");
}

#[test]
fn a_missing_include_lists_where_it_looked() {
    let dir = temp_dir("include-missing");
    write_file(&dir, "sub/script.btm", "Include \"lib.btm\"\n");
    // The decoy exists relative to the shell's directory, which is not
    // searched; the error says so rather than leaving the user guessing.
    write_file(&dir, "lib.btm", "Function Where()\nEnd Function\n");
    let outcome = run_script(&dir, &dir.join("sub/script.btm"), &[], &[]);
    assert_eq!(outcome.code, 1);
    assert!(
        outcome.stderr.contains("load error 53"),
        "stderr: {}",
        outcome.stderr
    );
    assert!(
        outcome.stderr.contains("File not found: lib.btm"),
        "stderr: {}",
        outcome.stderr
    );
    assert!(
        outcome.stderr.contains("current directory"),
        "stderr: {}",
        outcome.stderr
    );
    assert!(
        outcome.stderr.contains("1 | Include \"lib.btm\""),
        "stderr: {}",
        outcome.stderr
    );
}

#[test]
fn include_directories_come_from_minus_i_and_the_environment() {
    let dir = temp_dir("include-path");
    write_file(
        &dir,
        "libs/util.btm",
        "Function Greet()\n    Greet = \"hello\"\nEnd Function\n",
    );
    let script = write_file(
        &dir,
        "work/script.btm",
        "Include \"util.btm\"\nDebug.Print Greet()\n",
    );
    let libs = dir.join("libs");

    // `-I <dir>`.
    let mut command = Command::new(binary());
    command
        .arg("run")
        .arg("-I")
        .arg(&libs)
        .arg(&script)
        .current_dir(&dir)
        .env_remove("BARTELANG_PATH");
    let output = command.output().expect("spawn bartelang");
    assert_eq!(output.status.code(), Some(0));
    assert_eq!(String::from_utf8_lossy(&output.stdout), "hello\n");

    // `-I<dir>`, attached.
    let mut command = Command::new(binary());
    command
        .arg("run")
        .arg(format!("-I{}", libs.display()))
        .arg(&script)
        .current_dir(&dir)
        .env_remove("BARTELANG_PATH");
    let output = command.output().expect("spawn bartelang");
    assert_eq!(output.status.code(), Some(0));
    assert_eq!(String::from_utf8_lossy(&output.stdout), "hello\n");

    // With neither, the include is not found...
    let outcome = run_script(&dir, &script, &[], &[]);
    assert_eq!(outcome.code, 1);
    assert!(
        outcome.stderr.contains("File not found: util.btm"),
        "stderr: {}",
        outcome.stderr
    );

    // ...and `$BARTELANG_PATH` finds it.
    let mut command = Command::new(binary());
    command
        .arg("run")
        .arg(&script)
        .current_dir(&dir)
        .env(
            "BARTELANG_PATH",
            format!("{}:{}", dir.join("nope").display(), libs.display()),
        );
    let output = command.output().expect("spawn bartelang");
    assert_eq!(output.status.code(), Some(0), "stdout: {}", String::from_utf8_lossy(&output.stdout));
    assert_eq!(String::from_utf8_lossy(&output.stdout), "hello\n");
}

#[test]
fn errors_inside_an_included_file_name_that_file() {
    let dir = temp_dir("include-error");
    write_file(
        &dir,
        "lib.btm",
        "Function Boom()\n    Boom = 1 / 0\nEnd Function\n",
    );
    write_file(&dir, "script.btm", "Include \"lib.btm\"\n\nDebug.Print Boom()\n");
    let outcome = run_script(&dir, &dir.join("script.btm"), &[], &[]);
    assert_eq!(outcome.code, 1);
    assert!(
        outcome.stderr.contains("runtime error 11 in lib.btm at line 2"),
        "stderr: {}",
        outcome.stderr
    );
    assert!(
        outcome.stderr.contains("2 |     Boom = 1 / 0"),
        "stderr: {}",
        outcome.stderr
    );
}

#[test]
fn err_file_names_the_failing_file() {
    let dir = temp_dir("include-err-file");
    write_file(
        &dir,
        "lib.btm",
        "Sub Boom()\n    Dim y\n    Let y = 1 / 0\nEnd Sub\n",
    );
    write_file(
        &dir,
        "script.btm",
        "Include \"lib.btm\"\n\nTry\n    Boom\nCatch Err\n    Debug.Print Err.File & \" line \" & Err.Line & \" error \" & Err.Number\nEnd Try\n",
    );
    let outcome = run_script(&dir, &dir.join("script.btm"), &[], &[]);
    assert_eq!(outcome.code, 0, "stderr: {}", outcome.stderr);
    assert_eq!(outcome.stdout, "lib.btm line 3 error 11\n");
}

#[test]
fn duplicate_declarations_across_files_are_a_load_error() {
    let dir = temp_dir("include-duplicate");
    write_file(
        &dir,
        "lib.btm",
        "Sub Report()\n    Debug.Print \"lib\"\nEnd Sub\n",
    );
    write_file(
        &dir,
        "script.btm",
        "Include \"lib.btm\"\n\nSub Report()\n    Debug.Print \"script\"\nEnd Sub\n",
    );
    let outcome = run_script(&dir, &dir.join("script.btm"), &[], &[]);
    assert_eq!(outcome.code, 1);
    assert!(
        outcome.stderr.contains("load error 1005"),
        "stderr: {}",
        outcome.stderr
    );
    assert!(
        outcome.stderr
            .contains("Procedure 'report' is defined more than once"),
        "stderr: {}",
        outcome.stderr
    );
    assert!(
        outcome.stderr.contains("first at lib.btm line 1"),
        "stderr: {}",
        outcome.stderr
    );
}

#[test]
fn duplicate_declarations_within_one_file_are_a_load_error() {
    let dir = temp_dir("duplicate-one-file");
    let outcome = run(
        &dir,
        "Sub A()\n    Debug.Print \"first\"\nEnd Sub\nSub A()\n    Debug.Print \"second\"\nEnd Sub\nA\n",
        &[],
    );
    assert_eq!(outcome.code, 1);
    assert!(
        outcome.stderr.contains("load error 1005"),
        "stderr: {}",
        outcome.stderr
    );
    // The second definition used to win silently.
    assert_eq!(outcome.stdout, "");
}

#[test]
fn include_cycles_print_the_chain() {
    let dir = temp_dir("include-cycle");
    write_file(&dir, "a.btm", "Include \"b.btm\"\nDebug.Print \"a\"\n");
    write_file(&dir, "b.btm", "Include \"a.btm\"\nDebug.Print \"b\"\n");
    let outcome = run_script(&dir, &dir.join("a.btm"), &[], &[]);
    assert_eq!(outcome.code, 1);
    assert!(
        outcome.stderr.contains("load error 1004"),
        "stderr: {}",
        outcome.stderr
    );
    assert!(
        outcome
            .stderr
            .contains("Circular include: a.btm -> b.btm -> a.btm"),
        "stderr: {}",
        outcome.stderr
    );
}

#[test]
fn include_rules_are_enforced_at_load_time() {
    let dir = temp_dir("include-rules");
    let nested = run(&dir, "If 1 Then\n    Include \"lib.btm\"\nEnd If\n", &[]);
    assert_eq!(nested.code, 1);
    assert!(
        nested.stderr.contains("top level"),
        "stderr: {}",
        nested.stderr
    );

    let dir = temp_dir("include-rules-literal");
    let dynamic = run(&dir, "Dim name\nLet name = \"lib.btm\"\nInclude name\n", &[]);
    assert_eq!(dynamic.code, 1);
    assert!(
        dynamic.stderr.contains("string literal"),
        "stderr: {}",
        dynamic.stderr
    );
}

#[test]
fn a_leading_shebang_line_is_skipped() {
    let dir = temp_dir("shebang");
    let outcome = run(
        &dir,
        "#!/usr/bin/env bartelang\nDebug.Print \"ran\"\n",
        &[],
    );
    assert_eq!(outcome.code, 0, "stderr: {}", outcome.stderr);
    assert_eq!(outcome.stdout, "ran\n");
}

#[test]
fn parse_expands_includes_and_shows_units() {
    let dir = temp_dir("include-parse");
    write_file(
        &dir,
        "lib.btm",
        "Function Add(a, b)\n    Add = a + b\nEnd Function\n",
    );
    let script = write_file(
        &dir,
        "script.btm",
        "Include \"lib.btm\"\nDebug.Print Add(1, 2)\n",
    );
    let output = Command::new(binary())
        .arg("parse")
        .arg(&script)
        .output()
        .expect("spawn bartelang");
    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("FuncDeclaration"), "stdout: {stdout}");
    // Statements from the included file keep its unit id, which is what makes
    // the file name available on an error.
    assert!(stdout.contains("unit: 1"), "stdout: {stdout}");
}

#[test]
fn the_shipped_include_example_runs() {
    let examples = Path::new(env!("CARGO_MANIFEST_DIR")).join("examples");
    let dir = temp_dir("count-words");
    let output = Command::new(binary())
        .arg("run")
        .arg(examples.join("count_words.btm"))
        .current_dir(&dir)
        .output()
        .expect("spawn bartelang");
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert_eq!(output.status.code(), Some(0), "stderr: {stderr}");
    assert!(stdout.contains("sentence words: 9"), "stdout: {stdout}");
}
