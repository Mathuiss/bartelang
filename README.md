# bartelang

A 1998 Visual Basic 6 dialect running on a Unix stream engine.

Bartelang is an esoteric scripting language that fuses the corporate-macro
aesthetics of VB6 — `Dim`, `Let`, `Set`, vintage file channels, a native
`CreateObject("HTTP")` — with the raw stream-processing power of `sh`. A fast,
memory-safe Rust interpreter hides behind the ugly, comforting syntax.

```vb
' sys_fetch.btm
Dim sysName
Let sysName = `uname -n`

Dim http
Set http = CreateObject("HTTP")
http.Open "GET", "https://wttr.in/Delft?format=3", False
http.Send

Dim bold
Let bold = http.ResponseText | `tr '[a-z]' '[A-Z]'`
Debug.Print "[$sysName] $bold"
```

## Install and run

Install the released binary from crates.io:

```sh
cargo install bartelang
```

Releases follow `<major>.<functionality>.<hotfix>`: new functionality bumps the
middle number, a fix the last, and a breaking change the major.

Or build from a source checkout:

```sh
cargo build --release
./target/release/bartelang run examples/sys_fetch.btm Delft
./target/release/bartelang run examples/tour.btm
./target/release/bartelang parse examples/sys_fetch.btm     # dump the AST
```

The crate is both a binary and a library, so the lexer, parser and interpreter
can be embedded directly:

```rust
use bartelang::{Interp, execute, load};

// Parse without running...
let loaded = load("Debug.Print \"hello from bartelang\"\n")?;
let mut interp = Interp::new(vec![]);
interp.run_loaded(&loaded)?;

// ...or parse and run in one step, then inspect the final state.
let interp = execute("Dim code\nLet code = `exit 4`\n", vec![])?;
assert_eq!(interp.last_exit_code(), 4);
# Ok::<(), Box<dyn std::error::Error>>(())
```

A program that uses `Include` loads through `load_file` (or a `Loader`
carrying include directories), which resolves the paths and keeps every file's
text so errors can name it.

## CLI

- `bartelang run [-I <dir>]... <file.btm> [args...]` — parse and execute. `args`
  populate `ARGS` and are handed over untouched, hyphens included.
- `bartelang parse [-I <dir>]... <file.btm>` — dump the parsed AST with `{:#?}`.
- `bartelang version` / `--version`, `bartelang help` / `--help`
- `bartelang script.btm` — shorthand for `run` when the path exists or ends in `.btm`.

`-I <dir>` (also spelled `--include-dir <dir>`, repeatable, and `-I<dir>` works)
adds a directory to the include search path, and `$BARTELANG_PATH` adds more,
colon-separated. See [Modules and includes](#modules-and-includes).

Exit status: `0` on success, `1` for a syntax, load or runtime error, `2` for a
usage problem (including a script that cannot be read at all).

Errors name the file they came from and quote the line back:

```text
bartelang: runtime error 11 in lib/math.btm at line 3: Division by zero
  3 | Let y = 1 / 0
```

Syntax errors point a caret at the exact column:

```text
bartelang: syntax error in script.btm at line 2, column 15: expected ')', found end of line
  2 | Let x = (1 + 2
    |              ^
```

## Language tour

### Lexical rules

- **Case insensitive.** `DIM`, `dim` and `Dim` are the same keyword; identifiers
  are normalised to lowercase, so `sysName` and `SYSTEMNAME` are one variable
  (and a record called `Type Point` reports its name as `point`).
- **Newlines terminate statements.** No semicolons. `:` also separates statements
  on a physical line.
- **Line continuation.** A space, an underscore and a newline (` _\n`) is
  invisible to the lexer.
- **Comments.** `'` ignores the rest of the line.
- **Shebang.** A leading `#!` line is ignored, so a script can be run directly:
  `#!/usr/bin/env bartelang`.
- **Strings.** `"..."`, with `""` inside a string meaning a literal quote. `$name`
  and `${name}` interpolate a variable's value; `\$` escapes a literal dollar
  sign. A single-line string may not span lines.
- **Multi-line strings.** `"""` ... `"""` spans lines and is kept exactly as
  typed, including the newlines next to the delimiters — so `Trim` is usually
  what you want. Interpolation still applies.
- **Command substitution.** `` `...` `` is captured verbatim, so quotes inside it
  are not comments.
- **File channels.** `#` introduces a file number, which may be any expression:
  `#1`, `#channel`, `#FreeFile()`.

### Variables, arrays and records

`Dim` allocates, `Let` assigns a value, `Set` assigns an object.

```vb
Dim total
Let total = 2 + 3 * 4
Set http = CreateObject("HTTP")     ' Let is not needed for objects
```

Everything is an implicit `Variant` — integer, double, string, boolean, `Empty`,
`Null`, array, record, or object.

Arrays are 1-based, declared with a size, and are ordinary values, so they can be
returned from functions and passed around:

```vb
Dim slots(3)
Let slots(1) = "alpha"
Debug.Print UBound(slots)           ' 3
Debug.Print LBound(slots)           ' 1

Dim dynamic()
ReDim dynamic(2)                    ' or ReDim Preserve dynamic(5)
```

An out-of-range subscript raises error 9. An empty array reports `UBound = 0`
against `LBound = 1`, so `UBound < LBound` detects it.

Records are declared with `Type` and are **value** types — assigning one copies
it:

```vb
Type Server
    Name As String
    Port As Integer
    Tags(2) As String
End Type

Dim web As Server
web.Name = "example.invalid"
web.Port = 8080

Dim backup As Server
Let backup = web
backup.Port = 9090
Debug.Print web.Port                ' still 8080
```

`Type` declarations are hoisted, so a `Dim` may precede the block. There are no
classes, no methods and no inheritance — records are data, deliberately.

VB6 also lets you drop `Let` entirely, and a bare `Name arg1, arg2` calls a `Sub`.
Both forms work here, as does `Call Name(args)`.

### Operators

Precedence, loosest first: `|`, `Or`, `And`, `Not`, comparisons, `&`, `+ -`,
`Mod`, `\`, `* /`, unary `+ -`, `^` (right associative).

- `+` **attempts maths first.** Number plus number adds; a numeric-looking string
  is parsed and added. A string that will not parse raises a type mismatch
  (error 13) rather than silently concatenating.
- `&` **strictly concatenates** as strings, and treats `Null` as empty.
- `\` is integer division, `Mod` is the remainder, `/` is floating division.
- `=` is comparison inside an expression; a statement that *starts* with
  `name = ...` is an assignment (VB6 lets you drop `Let`).
- `And`/`Or` short-circuit and are logical (not bitwise) on non-boolean values.

### Control flow

```vb
If score > 10 Then
    Debug.Print "big"
ElseIf score > 5 Then
    Debug.Print "medium"
Else
    Debug.Print "small"
End If

If score > 10 Then Debug.Print "one-liner" Else Debug.Print "other"

Select Case score
Case 1, 2
    Debug.Print "low"
Case 3 To 9
    Debug.Print "middle"
Case Is > 100
    Debug.Print "huge"
Case Else
    Debug.Print "other"
End Select

For i = 1 To 10 Step 2
    Debug.Print i
Next i

For Each word In Split("one two three")
    Debug.Print word
Next word

While i < 100
    Let i = i * 2
Wend

Do                                  ' also `Do While c`, `Do Until c`
    Let i = i + 1
    If i = 5 Then Exit Do
Loop Until i >= 10
```

`Select Case` evaluates its subject once and runs the first matching arm; there is
no fall-through. `Exit For` leaves a `For` or a `For Each`, `Exit While` a
`While`, `Exit Do` a `Do` — and each has to match the innermost loop, so a
mistargeted exit is reported rather than silently breaking out.

### Procedures

```vb
Sub Banner(text)
    Debug.Print "== $text =="
End Sub

Function Fact(n)
    If n <= 1 Then
        Fact = 1          ' a Function returns by assigning to its own name
        Exit Function     ' and can leave early
    End If
    Fact = n * Fact(n - 1)
End Function
```

There is no `return` keyword. Declarations are hoisted, so a procedure may be
called before it appears in the file. Parameters are `ByVal`; `ByVal`/`ByRef`
prefixes and `As <Type>` suffixes are accepted and ignored.

### Modules and includes

A program can span several files. `Include "path.btm"` brings another file's
declarations in, exactly as if they had been typed at that point:

```vb
' report.btm
Include "lib/text.btm"

Banner "monthly report"
Debug.Print "words: " & WordCount(sentence)
```

- **The path is a string literal**, resolved once at load time. A variable or a
  glob is a syntax error.
- **Top level only.** An `Include` inside an `If` or a procedure is a syntax
  error, so everything it brings in is hoisted like any other declaration.
- **One program, one namespace.** `Sub`, `Function`, `Type` and `Const` from an
  included file are visible everywhere, and a library may call back into the
  script that included it. A name defined twice — in one file or across files —
  is error `1005`, reported with both places; VB6's silent last-wins is how a
  library quietly stops working.
- **A file is read once** per canonical path, so the diamond (`a` includes `b`
  and `c`, and both include `lib`) runs `lib`'s top-level code once. A cycle is
  error `1004`, which prints the chain.
- **Top-level code runs at the include point**, in include order, so a library
  can set up state before the script continues.
- **Resolution order:** an absolute path; else the including file's directory;
  else each `-I <dir>`; else each `$BARTELANG_PATH` entry. The current directory
  is deliberately not searched, so a script behaves the same from any shell — and
  when a name exists there but not on the search path, the error says so.
- **Every file is a `.btm`.** A library is a script whose top level happens to
  hold only declarations; there is no second extension to learn.

Because every file is kept, an error inside an included file names that file and
quotes its line, and `Err.File` reports it to a `Catch` block.

### Error handling and the `Err` object

```vb
Function Reciprocal(n)
    If n = 0 Then
        Err.Raise 7001, "Math", "cannot divide by zero"
    End If
    Reciprocal = 1 / n
End Function

Try
    Debug.Print Reciprocal(0)
Catch Err
    Debug.Print Err.Number & " at line " & Err.Line & ": " & Err.Description
End Try
```

Any runtime error — a failed HTTP request, a bad subscript, a division by zero, a
missing file — jumps straight to the `Catch` block. `Err.Raise` lets your own
code fail on purpose, which is what makes `Catch` useful across a library rather
than only around interpreter calls.

The `Err` object carries `Number`, `Description` (also spelled `Message`),
`Source`, `Line` and `File`. `Catch <name>` binds the same object under the name
you chose. `Err.Clear` resets it; otherwise it keeps the last error, as in VB6.

`Line` is a line number *within* `File` — the included file the failing
statement came from, named the way the loader names it, or the entry script.
VB6 had one file per program, so a bare line number was enough; with includes it
is not, which is why `File` exists. It is empty when the program was loaded from
a string (`load` / `execute`), where there is no file to name.

### Bash integration

```vb
Dim who
Let who = `uname -n`                    ' command substitution, stdout back to a variant

Dim shout
Let shout = "hello unix" | `tr '[a-z]' '[A-Z]'`   ' pipe a value into a command's stdin

Dim result
Set result = CreateObject("WScript.Shell").Exec("ls -l /nonexistent")
Debug.Print result.StdOut               ' and .StdErr, .ExitCode, .Status

Debug.Print "on $who"
Debug.Print ARGS(1)                     ' 1-based CLI arguments, "" when out of range
Debug.Print ENV("?")                    ' integer exit code of the last shell command
Debug.Print ENV("HOME")                 ' any environment variable
```

Backtick output has its trailing newline stripped. Commands run through `sh -c`
with stdin closed unless a pipeline supplies data. Because Bartelang is meant to
be a real Unix citizen it restores the default `SIGPIPE` disposition, so
`bartelang run x.btm | head` exits quietly instead of panicking, and pipeline
payloads are written from a helper thread so large streams cannot deadlock.

### Objects

`CreateObject` takes a plain name that the interpreter looks up in a table — there
is no COM, no registry and nothing Windows-specific behind it. The names follow
the era's convention because that is what period code says, and each has a short
alias for when you would rather not type it. Both are matched
case-insensitively.

| Type | Alias | What it is |
| --- | --- | --- |
| `HTTP` | — | a blocking HTTP client |
| `Scripting.FileSystemObject` | `FS` | paths, directories, files, and `TextStream` |
| `Scripting.Dictionary` | `DICT` | an associative container, in insertion order |
| `Collection` | `COLL`, `LIST` | the 1-based, order-preserving collection |
| `VBScript.RegExp` | `REGEX` | regular expressions |
| `WScript.Shell` | `PROC` | running processes with both streams captured |

`HTTP` is the one type with no alias: it was reduced to a plain mnemonic when the
`MSXML2.XMLHTTP` ProgID was removed, so it is already the short name.

**`HTTP`** — `Open method, url, async`, `SetRequestHeader name, value`,
`SetTimeouts ms...`, `Send [body]`, `Abort`, and the properties `ResponseText`,
`Status`, `StatusText`, `ReadyState`. Backed by safe `reqwest` with rustls for
TLS — no system OpenSSL to install — and a 30 second default timeout.

**`Scripting.FileSystemObject`** — `FileExists`, `FolderExists`, `CreateFolder`
(`mkdir -p` semantics), `DeleteFile`, `DeleteFolder` (recursive only when you pass
`True`), `CopyFile`, `MoveFile`, `GetBaseName`, `GetExtensionName`,
`GetParentFolderName`, `BuildPath`, `GetTempName`, `GetAbsolutePathName`,
`FileSize`, `FileDateTime`, `OpenTextFile(filename, mode, create)`,
`CreateTextFile(filename, overwrite)`. Modes are 1 = read, 2 = write,
8 = append.

**`TextStream`** — `ReadAll`, `ReadLine`, `Write`, `WriteLine`, `AtEndOfStream`,
`Line`, `Close`.

**`Scripting.Dictionary`** — `Add key, value`, `Exists(key)`, `Item(key)`,
`Remove(key)`, `RemoveAll`, `Keys()`, `Items()`, and `Count`. Reading a missing
key raises rather than silently creating it (VB6 created it); assignment
(`d(k) = v`) does create the key.

**`Collection`** — `Add item, [key]`, `Item(position_or_key)`, `Remove`, `Count`.
Indexes are 1-based and keys are optional. `Collection.Item` is read-only, as in
VB6.

**`VBScript.RegExp`** — settable `Pattern`, `Global`, `IgnoreCase`; then `Test(text)`
and `Replace(text, replacement)`, with the same `$1` group syntax VBScript used.

**`WScript.Shell`** — `Run(command, [wait])` hands the terminal to the command and
returns its exit code; `Exec(command)` captures both streams and returns an object
with `StdOut`, `StdErr`, `ExitCode` and `Status`.

Any other name raises error 429, which reports every canonical name and alias
that does exist.

### File channels

```vb
Dim f
Let f = FreeFile()
Open "weather_log.txt" For Append As #f
Print #f, "[$sysName] $formattedWeather"
Write #f, sysName, 21.5, True          ' quoted, comma-separated records
Close #f

Open "weather_log.txt" For Input As #2
Dim line
While Not EOF(#2)
    Line Input #2, line                ' one line at a time
    Debug.Print "read: $line"
Wend
Input #2, aName, aNumber, aFlag        ' or parse a Write record back
Close #2
```

Modes are `Input`, `Output` and `Append`. `Close` with no handle closes
everything. Writing to or closing a channel that is not open raises error 52.
`LOF(n)` is the length, `Loc(n)`/`Seek(n)` the position, and `FreeFile()` the
lowest unused channel — which is why a file number may be an expression.

`Print` separates values with `;` (nothing) or `,` (a tab), and a trailing `;`
suppresses the line ending.

### Built-in functions

- **Globals:** `ARGS(x)`, `ENV(name)`, `InputBox(prompt)`, `CreateObject("HTTP")`
- **Strings:** `Len`, `Left`, `Right`, `Mid`, `InStr`, `InStrRev`, `UCase`, `LCase`,
  `Trim`, `LTrim`, `RTrim`, `Replace`, `Space`, `String`, `Chr`, `Asc`
- **Conversions:** `Val`, `Str`, `CStr`, `CInt`, `CLng`, `CDbl`, `CSng`, `CBool`,
  `Hex`, `Oct`
- **Maths:** `Abs`, `Int`, `Fix`, `Sgn`, `Sqr`, `Round`, `Rnd`, `Randomize`
- **Numbers:** `Format(value, mask)`
- **Arrays:** `Array`, `Split`, `Join`, `LBound`, `UBound`
- **Introspection:** `TypeName`, `IsNumeric`, `IsEmpty`, `IsNull`, `IsObject`,
  `IsArray`, `IsDate`, `IIf`
- **Dates:** `Now`, `Date`, `Time`, `Timer`, `DateAdd`, `DateDiff`, `DatePart`
- **Files:** `EOF(#n)`, `LOF(#n)`, `Loc(#n)`, `Seek(#n)`, `FreeFile`
- **Statements:** `Debug.Print`, `Open`/`Print #n`/`Write #n`/`Input #n`/
  `Line Input #n`/`Close`
- **Constants:** `True`, `False`, `Nothing`, `Empty`, `Null`, `vbCrLf`, `vbLf`,
  `vbCr`, `vbTab`, `vbNullString`

All positions are 1-based, as required.

`Format` accepts the named formats (`Standard`, `Currency`, `Percent`,
`Scientific`, `Yes/No`, `Short Date`, `Long Time`, ...) and user masks —
`"#,##0.00"`, `"$0.00;(0.00)"`, `"yyyy-mm-dd"`, `"dddd, mmmm d, yyyy"`. Watch the
era's trap: in a date mask `m` is month and `n` is minute, and `h`/`hh` use a
12-hour clock only when the mask also carries `AM/PM`.

### Console I/O

There is no dialog box. `Debug.Print` writes a line to stdout. `InputBox` writes
its prompt to stdout without a trailing newline, flushes, and reads one line from
stdin — so it works interactively and with piped input alike.

## Type coercion

- `Empty` is `0` numerically and `""` as text; `True` is `-1` and `False` is `0`
  in numeric contexts, the VBA convention.
- Truthiness: `0`, `Empty`, `False`, `Null` and `""` are false; everything else is
  true.
- Two strings compare as strings; otherwise comparison tries a numeric comparison
  first and falls back to text.
- `CInt`/`CLng` round half to even, like VB6.
- `Int` rounds toward negative infinity while `Fix` truncates toward zero: the
  classic VB6 pair `Int(-2.5)` is `-3` but `Fix(-2.5)` is `-2`.
- `Null` is a distinct value, produced only by the `Null` literal. Using it in
  arithmetic or a comparison raises error 94 and points at `IsNull`, rather than
  quietly propagating the way VB6 did.
- Records cannot be compared, and are the only value type that copies on
  assignment; arrays share their storage, as they did in VB6.

## Verified behaviour

`cargo test` runs the whole suite: unit tests across the lexer, parser, loader,
value, object, interpreter and library modules, end-to-end tests that drive the
real binary, and a doctest that compiles the Rust example above — so this
document cannot drift far from the code. Run `cargo test` for the current count.

The suite covers command substitution and exit codes, a ~1.1 MB pipeline payload
(which would deadlock if writing stdin and reading stdout raced), file channel
round-trips including `Write`/`Input` records, records and their copy semantics,
recursion, every documented runtime error, every documented call syntax, error
line numbers, and broken-pipe handling. For includes it covers declarations
shared in both directions, include-once diamonds, cycle chains, the resolution
order with `-I` and `$BARTELANG_PATH`, duplicate declarations, and an error
raised inside an included file naming that file — both in the CLI output and
through `Err.File`.

`examples/tour.btm`, `examples/log_stats.btm` and `examples/count_words.btm` are
executed end to end by the suite. `examples/sys_fetch.btm` is parsed by the suite
and also run by hand against `wttr.in`.

## Source layout

```text
src/error.rs      numbered VB-style errors, diagnostics, and error rendering
src/lexer.rs      source text -> tokens
src/ast.rs        Expr / Stmt / Program
src/parser.rs     recursive-descent parser
src/loader.rs     Include resolution, the source map, and duplicate checks
src/value.rs      Variant, the VB coercion quirks, and value-vs-reference rules
src/record.rs     user-defined record types
src/datetime.rs   local wall-clock time and date arithmetic
src/format.rs     the Format(value, mask) implementation
src/records.rs    the Write #n / Input #n record format
src/random.rs     the Rnd / Randomize generator
src/objects.rs    the BartObject trait, the object table, and the HTTP client
src/objects/      filesystem, collections, regex and process objects
src/interp.rs     tree-walk interpreter, environments, shell engine, file channels
src/builtins.rs   the native standard library
src/lib.rs        library surface: load / load_file / execute / execute_file
src/main.rs       CLI
tests/cli.rs      end-to-end tests that drive the real binary
examples/        sys_fetch.btm, tour.btm, log_stats.btm, count_words.btm + lib/text.btm
```

## Implementation notes and deliberate decisions

- **The object type is `"HTTP"`, not `"MSXML2.XMLHTTP"`.** `CreateObject` is a
  plain lookup, so there was never any COM behind it. The ProgID and its version
  aliases are gone, and an unknown name reports which types do exist.
- **Object names carry short aliases** — `FS`, `DICT`, `COLL`/`LIST`, `REGEX`,
  `PROC` — so `CreateObject("dict")` and `CreateObject("Scripting.Dictionary")`
  are the same thing. The table that defines them generates the error listing too,
  so the two cannot drift apart. `HTTP` needs no alias, having already been
  reduced to one.
- **`MsgBox` is removed.** A terminal language has no dialog box, and aliasing it
  to `Debug.Print` would have hidden that. `MsgBox "hi"` reports error 35.
- **`+` on a non-numeric string is an error** rather than a silent concatenation;
  `&` is the concatenation operator.
- **Reading an undeclared variable raises error 500** rather than materialising
  `Empty`. Assigning to a new name still creates it.
- **`And`/`Or` are logical, not bitwise.**
- **`IIf` short-circuits.** VB6's was an ordinary function and evaluated both
  branches, so `IIf(x <> 0, 1 / x, 0)` divided by zero anyway. A user-defined
  `Function IIf` still shadows the built-in.
- **Multi-line strings exist**, as the one knowing break from the 1998 rules.
- **`Const` is enforced**, not merely documented: assigning to one raises 501.
- **`Null` does not propagate.** VB6's silent propagation is how `If x = Null`
  became quietly false; here it is error 94.
- **`DeleteFolder` is not recursive unless you ask.** A destructive default is not
  one worth inheriting.
- **Dictionary reads of a missing key raise** instead of auto-creating the key.
- **`Exit` must match the innermost loop**, so a mistargeted one is reported.
- **Notes on the era's own traps.** `DeleteFolder` and `CreateFolder` differ from
  VB6 in the two places above; `Format`'s date masks use `nn` for minutes; the
  comma in a `Print` list is a tab rather than a fixed 14-column zone; `Run` waits
  by default, where VB6 returned a meaningless 0 unless told to wait.
- **`Include` is a statement, not a metacommand.** QBasic's `'$INCLUDE:` was the
  era-correct spelling and is deliberately unsupported: a comment that sometimes
  executes contradicts the language's own rule that `'` ignores the rest of the
  line. `Include "path.btm"` is greppable, and takes a literal path only, so it
  resolves before anything runs.
- **Includes resolve from the file, not the shell.** The including file's
  directory comes first, then `-I` directories, then `$BARTELANG_PATH`; the
  current directory is never searched implicitly. `Open` stays CWD-relative (as
  in VB6), and when the two rules would disagree the diagnostic says which one
  applied.
- **A file is included once, and a cycle is an error.** Diamonds are normal in a
  library, so re-including is a no-op; a cycle is reported with its chain rather
  than quietly producing half a program.
- **Duplicate declarations are load errors.** VB6 kept the last definition
  silently; across files that is how a library stops being called without anyone
  noticing.
- **`Err.File` is a Bartelang addition.** VB6's `Err` had no file to name — one
  file per program — so `Err.Line` alone becomes ambiguous the moment a program
  has includes.
- **TLS is rustls, not OpenSSL.** `reqwest` 0.13 defaults to rustls with
  `aws-lc-rs`, verifying against the platform trust store, so HTTPS still honours
  the system's certificates. Building needs a C toolchain and cmake (for
  `aws-lc-rs`) but no OpenSSL headers — the trade that lets `cargo install
  bartelang` work on a bare Linux box.
- **Known gap.** Assigning *through* a non-variable member receiver
  (`points(1).X = 5`) is not supported: reading works, writing would need an
  lvalue path in the interpreter.

### Error numbers

Codes follow VB6 wherever one exists:

- `5` invalid procedure call, `6` overflow, `9` subscript out of range,
  `11` division by zero, `13` type mismatch, `35` sub or function not defined,
  `52` bad file name or number, `53` file not found, `58` file already exists,
  `67` too many files, `76` path not found, `94` invalid use of Null,
  `424` object required, `429` cannot create an object of that type,
  `438` object doesn't support this property or method,
  `450` wrong number of arguments, `457` key already in use or not found.
- Bartelang-specific: `500` variable not defined, `501` assignment to a constant,
  `1001` the shell could not be spawned, `1002` HTTP transport failure,
  `1003` call-stack overflow, `1004` circular include, `1005` declaration defined
  more than once.

A missing `Include` reports `53` and lists every directory it searched; the
other load-time failures use the Bartelang-specific numbers above.

### Limits

- Nested procedure calls are capped at 256; exceeding that raises error 1003
  instead of crashing the process. The interpreter also runs on a 64 MB stack, so
  ordinary deep recursion is comfortable.
- The HTTP object defaults to a 30 second timeout; `http.SetTimeouts` overrides it.
- `For` loops have no iteration cap — an unbounded loop runs until interrupted.

## License

MIT. See `LICENSE`.
