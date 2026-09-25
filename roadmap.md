# Bartelang roadmap

What we want to add to Bartelang next, why, and in what order.

**Bartelang is released.** From `1.0.0` on, the version number is
`<major>.<functionality>.<hotfix>` — a breaking change bumps the major, a new
capability the middle number, a fix the last. The milestones below are the record
of how the language got here; everything still to come lands as a functionality
update, under the same two rules.

Every item below is something that closes a real gap in the language, not a
feature added for nostalgia's sake. The VB6 surface is a *design language* — it
tells us how things should read — but nothing goes in unless it earns its keep on
a Linux machine, and unless the interpreter can actually back it with real
behaviour rather than costume.

Two rules for every step:

1. **It ships with tests.** Unit tests for parser/interpreter behaviour, plus
   end-to-end tests that drive the real binary.
2. **It ships with documentation.** README and rustdoc are updated in the same
   step, not at the end. The README is compiled as a doctest, so its Rust
   examples must build.

---

## Decisions already made

- **`Type` yes, `Class` no.** User-defined *records* are in scope. Class modules,
  `Property Get/Let/Set` and inheritance are out: that is the line between a
  scripting language with objects and a general-purpose OOP language.
- **Multi-line strings yes**, as a knowing break from the 1998 rule. It is the
  one place we deliberately leave the era behind.
- **Objects use the canonical VB6-era class names** (`Scripting.FileSystemObject`,
  `Scripting.Dictionary`, `VBScript.RegExp`, `WScript.Shell`), so period code
  reads correctly.
- **No VB6 *source* compatibility guarantee.** We simplify wherever a
  Windows-only name or behaviour would be a lie the interpreter cannot back up.
  This is already in effect: `MsgBox` was removed, and `CreateObject("HTTP")`
  replaced the `MSXML2.XMLHTTP` ProgID.
- **1-based indexing everywhere** — arrays, string positions, and collections.
- **Released, and versioned `<major>.<functionality>.<hotfix>`.** `1.0.0` is the
  first version under this policy (0.1.0 was the pre-release): a breaking change
  bumps the major, new functionality the middle number, and a fix the last, so
  `cargo install bartelang` and a pinned `Cargo.toml` both mean something
  predictable. The version is bumped in the same step as the change, not at
  release time. `1.1.0` was the first functionality update; `1.2.0` is the
  second (commands and the environment).
- **The CLI is `clap` and TLS is `rustls`.** clap (builder API, no proc macro)
  owns argument parsing, `--help` and usage errors; `reqwest` 0.13 defaults to
  rustls with the platform trust store, so builds stop needing OpenSSL while
  HTTPS keeps using the system's certificates. Both landed in `1.1.0`, the first
  functionality update.

### A note on object naming

New objects follow the VB6 convention above. The one exception is `HTTP`, which
keeps the plain name adopted when the `MSXML2.XMLHTTP` ProgID was removed: there
was no single canonical VB6 name for it (MSXML2 vs WinHttp vs
`ServerXMLHTTP`, each with its own version suffix), and the vendor prefix was
carrying no meaning. If we would rather have one rule instead of two, unifying is
a small change — see *Open questions*.

---

## Milestone 1 — Finish the error and control-flow model  ✅ done

These are the gaps you hit in ordinary code, and each one is small.

- [x] **`Err.Raise number, source, description`** — today `Try/Catch` can only
      catch errors the *interpreter* raises. A user-written `Function` has no way
      to signal "bad input", so reusable libraries must return sentinel values
      and every caller must check them. `Err.Raise` completes the error model and
      is what makes `Catch` useful across a shared library rather than only
      around interpreter calls.
- [x] **`Exit Sub` / `Exit Function`** — no early return exists today, so guard
      clauses and validation turn into deeply nested `If`s. The other half of the
      contract above.
- [x] **`Exit For` / `Exit While` / `Exit Do`** — there is currently no way to
      break a loop early. Workarounds require a flag variable and an `If` around
      every remaining statement. `Exit Do` was added alongside the roadmap's two
      because a `Do ... Loop` with no way out is not usable. Each exit must match
      the innermost loop, so `Exit For` inside a `Do` is reported rather than
      silently breaking out.
- [x] **`Do ... Loop`** — both pre-condition (`Do While`, `Do Until`) and
      post-condition (`Do ... Loop While/Until`) forms, plus bare `Do ... Loop`
      with `Exit Do`. `While/Wend` forces a variable to be pre-initialised before
      every loop, which is the classic source of "it ran once when the list was
      empty". A post-condition loop is a correctness win, not just a period
      mannerism.
- [x] **`Select Case`** — with `Case 1 To 5` ranges, `Case Is > x` comparisons,
      comma-separated lists, and `Case Else`. Replaces the `ElseIf` ladders that
      period code is full of, and maps cleanly onto a Rust `match`.
- [x] **`With ... End With`** — object-heavy code stops repeating the variable
      name. The HTTP request path is the immediate beneficiary. The subject is
      captured on entry, so rebinding the variable inside the block does not
      change what `.member` means — which is how VB6 behaved.
- [x] **`Const name = value`** — named constants. They are genuinely immutable:
      assigning to one raises error 501 rather than quietly overwriting it.
- [x] **`IIf(condition, truepart, falsepart)`** — one-line conditional. Note:
      VB6's `IIf` is a *function*, so it evaluates **both** branches. Keeping that
      is faithful and occasionally surprising; short-circuiting is safer but is
      not what 1998 code expects. **Decided: short-circuits.** A user-defined
      `Function IIf` still takes precedence.

Bugs found and fixed while landing this milestone: `Try/Catch` had to learn that
`Exit` flows are control transfer rather than errors (otherwise `Exit Function`
inside a `Try` would have been swallowed by its own `Catch`).

## Milestone 2 — Text, arrays and iteration  ✅ done

Text is the other half of a shell language, right after running commands.

- [x] **Make arrays first-class values.** *This blocked everything else in this
      milestone.* Arrays used to live in a per-scope map keyed by name, so one
      could not be returned from a function or produced by a builtin.  They are
      now an ordinary `Variant` case (`Variant::Array`), which is what makes
      `Split`, `Array(...)` and `For Each` possible at all.
- [x] **`Split(expression, delimiter)`** — a string becomes an array. Today there
      is no way to turn `"a,b,c"` into fields, which is a hole for anything that
      consumes command output.  `Split("")` is an empty array; a `limit`
      argument leaves the remainder in the final piece.
- [x] **`Join(array, delimiter)`** — the inverse.
- [x] **`Array(...)`** — inline array constructor, so arrays need not be declared
      and filled one slot at a time.
- [x] **`LBound(array)` / `UBound(array)`** — array bounds. Without them there is
      no way to iterate an array whose size you did not hardcode.  An empty array
      reports `UBound = 0` against `LBound = 1`, so `UBound < LBound` detects it.
- [x] **`For Each ... Next`** over arrays. Pairs with 1-based indexing, and is the
      foundation for iterating collections added later.  `Exit For` leaves a
      `For Each` as well.  The loop walks a snapshot taken on entry, so a body
      that resizes the array cannot disturb the iteration.
- [x] **`ReDim` / `ReDim Preserve`** — dynamic arrays. A companion to the above
      rather than a separate idea: functions that build up a result need a
      resizable array.  `Dim buffer()` declares an empty array for a later
      `ReDim`, and resizing happens in place, so a procedure that `ReDim`s its
      array argument changes the caller's array too — as VB6 did.

Behaviour change worth knowing: because arrays are variables now, reaching for a
procedure-local array from outside it reports error **500** (variable not
defined) instead of the old array-specific **9**. Bounds errors are still 9, and
using a scalar where an array is expected is now a type mismatch (**13**).

## Milestone 3 — The object table  ✅ done (apart from the optional item)

`CreateObject` knows six types now, up from one.

- [x] **`Scripting.FileSystemObject`** — the biggest single usability win *and*
      the biggest safety win. Paths, existence checks, directories, copy/move/
      delete, basename/dirname/extension, temp names. Today all of this goes
      through backticks, which means it is slower and — more importantly — **any
      value interpolated into a shell command is an injection vector**. A native
      object sidesteps that entirely.
      Core: `FileExists`, `FolderExists`, `CreateFolder`, `DeleteFile`,
      `DeleteFolder`, `CopyFile`, `MoveFile`, `GetBaseName`, `GetExtensionName`,
      `GetParentFolderName`, `BuildPath`, `GetTempName`, `GetAbsolutePathName`,
      `OpenTextFile`, `CreateTextFile`.
      Plain-value stand-ins for the `File` sub-object: `FileSize(path)`,
      `FileDateTime(path)`.  The `Drive` / `Folder` / `File` sub-objects
      (`.Size`, `.DateLastModified`, `.Files`) are still a later layer.
      Two deliberate choices: `CreateFolder` has `mkdir -p` semantics rather
      than failing on a missing parent, and `DeleteFolder` is **not** recursive
      unless you pass `True` as the second argument — a destructive default is
      not one to inherit.
- [x] **`TextStream`** (returned by `fso.OpenTextFile` / `CreateTextFile`) —
      `ReadAll`, `ReadLine`, `Write`, `WriteLine`, `AtEndOfStream`, `Line`,
      `Close`.  Reading a whole file used to mean a `Line Input` loop; `ReadAll`
      alone justifies this.
- [x] **`Scripting.Dictionary`** — the associative container the language
      completely lacked, which ruled out counting, grouping and de-duplication.
      `Add`, `Exists`, `Item`, `Keys`, `Items`, `Remove`, `RemoveAll`, `Count`,
      in insertion order.  One divergence: a *read* of a missing key raises
      rather than silently creating it, as VB6 did — use `Exists` to test.
      Assignment (`d(k) = v`) still creates the key, as in VB6.
- [x] **`Collection`** — VB6's built-in collection. Worth having on its own merits
      for a Bartelang-specific reason: **it is 1-based and order-preserving**, so
      `col.Item(1)` is the first element, exactly as the language's indexing rule
      promises. Reached as `CreateObject("Collection")` — the one name that was
      never a ProgID, since VB6 wrote `New Collection`; keeping `CreateObject` as
      the single mechanism seemed worth more than adding a `New` keyword.
- [x] **`VBScript.RegExp`** — `Pattern`, `Global`, `IgnoreCase`, `Test`,
      `Replace`. The natural partner to `Split` for text munging. Adds the
      `regex` crate, in the same spirit as `reqwest`. `Replace` uses the same
      `$1` group syntax VBScript did. Still to come: `Execute` returning match
      objects.
- [x] **`WScript.Shell`** — structured process execution. The old shell
      interface was one-shot: you got stdout or nothing, stderr leaked to the
      terminal, and there was no way to capture stdout, stderr **and** the exit
      code from one command. `Exec` returns a `WshScriptExec` with `StdOut`,
      `StdErr`, `ExitCode` and `Status`; `Run` hands the terminal to the command
      and returns its exit code.
      Note this is Windows Script Host rather than VB6 proper — VB6's own `Shell()`
      function could not capture output at all.  Two simplifications: `StdOut`
      and `StdErr` are plain strings rather than stream objects to call
      `ReadAll()` on, and `Run` waits by default (VB6 returned a meaningless 0
      unless you asked it to wait).  Not yet done: per-command working directory,
      environment and timeout — see the remaining items below.
- [ ] **`ADODB.Stream`** *(optional, deliberately deferred)* — binary and
      encoding-aware streams.  `TextStream` covers the common cases, and nothing
      in the language currently needs bytes rather than text, so this waits until
      something does.

Supporting language work this milestone needed, none of which was on the
original list:

- **Member assignment** (`obj.Member = value`) now parses and dispatches to
  `BartObject::set_property`.  It was previously documented as unsupported, and
  `re.Pattern = "..."` made it unavoidable.
- **Default members.**  `dict(key)` reads as `dict.Item(key)`, and
  `dict(key) = v` assigns through `BartObject::set_item`.  Indexed assignment now
  dispatches on the target: an array subscript must be a whole number, an object
  subscript is a key of any type.
- **Short object aliases** were added after the fact: `FS`, `DICT`, `COLL`/`LIST`,
  `REGEX` and `PROC`, all case-insensitive, alongside the canonical names.
  `OBJECT_TYPES` is the single table the lookup *and* the 429 error listing come
  from, so a name cannot be advertised without being constructible — a test
  asserts exactly that, for every canonical name and alias.
- **`Now()` was wrong by the time-zone offset.**  It read UTC seconds and
  presented them as wall-clock time, so on a CEST machine it ran two hours behind
  `date`.  Time now goes through `localtime_r`, and `Now()`, `Date()`, `Time()`
  and `FileDateTime()` agree with the shell in every zone.

Still outstanding from this milestone: `ADODB.Stream`, the `Drive` / `Folder` /
`File` sub-objects, `RegExp.Execute` with match objects, and per-command working
directory, environment and timeout on `WScript.Shell`.

## Milestone 4 — Standard library completion  ✅ done

Small, expected, occasionally essential.

- [x] **`Format(value, mask)`** — user-defined masks (`"#,##0.00"`, `"yyyy-mm-dd"`)
      and the named ones (`Standard`, `Currency`, `Percent`, `Scientific`,
      `Yes/No`, `Short Date`, `Long Time`, ...), with `;`-separated sections for
      negative and zero values. Careful: VB6's minute mask is `nn`, because `m`
      is month — and `h`/`hh` switch to a 12-hour clock only when the mask also
      carries `AM/PM`.  Date masks are told apart from numeric ones by their
      tokens (`y d h n s`), so a numeric mask with literal text like `"0.0 days"`
      would be misread; that is the documented trade.
- [x] **`Round(x, n)`** — banker's rounding, consistent with the existing `CInt`.
- [x] **`Rnd` / `Randomize`** — a dependency-free xorshift generator, seeded from
      clock, pid and heap address.  Not cryptographic, and no more so than the
      `Rnd` it replaces.
- [x] **`Timer`** — useful for timing the shell commands the language is built to
      run.
- [x] **`DateAdd` / `DateDiff` / `DatePart`** — `Now` exists but there is no way
      to compute "yesterday" or "seven days ago", which is the first thing any
      scheduled script wants.  Calendar units clamp the day (`Jan 31` plus a
      month is `Feb 28`), and `DateDiff` counts boundaries crossed, as VB6 did.
- [x] **Introspection:** `TypeName`, `IsObject`, `IsArray`, `IsDate`, and `IsNull`
      as distinct from `IsEmpty`.  That last one needed a `Variant::Null` to
      exist at all, so the language now has one.  VB6 *propagated* Null through
      arithmetic and comparisons, which is how `If x = Null` became silently
      false; here using Null that way raises error 94 and points at `IsNull`.
- [x] **File-channel completeness:** `FreeFile`, `LOF`, `Loc`, `Seek`; `Write #n`
      and `Input #n` (the paired quoted/CSV-style writer and field parser); and
      `Print` separators — `;` tight, `,` a tab, plus a trailing `;` that
      suppresses the line ending.  VB6 advanced the comma to a fixed 14-column
      zone; a tab needs no per-stream column bookkeeping and behaves sensibly in
      both files and terminals.
      This item forced one more fix worth recording: file numbers used to be
      literals only (`#1`), which made `FreeFile` unusable.  `#` now introduces an
      *expression*, so `Open "f" For Output As #f` and `As #FreeFile()` both
      work — as they did in VB6.
      Still to come: `Get`/`Put` for binary records.

## Milestone 5 — Language extensions  ✅ done

The two deliberate departures.

- [x] **`Type ... End Type`** — user-defined records with field access
      (`p.Name`). Values copy on assignment, as VB6 records do — which is the
      opposite of arrays, so `Variant` now has a hand-written `Clone` that
      deep-copies only records. Explicitly *not* classes; no methods, no
      inheritance, no `Property` procedures.  Field array sizes are supported
      (`Tags(3) As String`), definitions are hoisted like procedures, and
      `Dim slots(3) As Point` starts every slot as its own record.
- [x] **Multi-line strings** — our one knowing break from 1998. Config files and
      templates are miserable without them, and building them out of `vbCrLf`
      concatenation is the kind of ugly that is not comforting. **Decided:
      `"""`, kept verbatim (leading and trailing newlines included, so use
      `Trim`), with `$var` interpolation as usual.**  The choice was free
      because a bare `"""` used to be a syntax error.

One thing this milestone forced.  Records made a long-standing gap obvious:
`.member` only worked on a plain variable name, so `points(1).X` — the natural
way to read a field out of an array of records — did not parse, and neither did
`f().X` or `x.Y.Z`.  There is now an `Expr::MemberAccess` for a receiver that is
any expression, so all three work.

Assigning *through* a non-variable receiver (`points(1).X = 5`) is still not
supported: the receiver is evaluated to a value, and for a record that is a copy,
so the write would go nowhere.  Doing it properly needs an lvalue path in the
interpreter — a real follow-up, and listed under *Open questions*.

## Milestone 6 — Observability  ✅ done

- [x] **Line numbers on runtime errors.** Errors used to carry a number and a
      description but no line, which was the last gap between them and syntax
      errors.  Every statement is now tagged with the line it began on as the
      parser builds it (`Stmt::Located`), and the interpreter attaches the
      *innermost* failing line as the error unwinds - so a division that fails
      inside a procedure reports that procedure's line, not the call site.  The
      CLI quotes the offending line back, and `Err.Line` exposes it to scripts
      (0 once `Err.Clear` runs, matching VB6's `Erl`).
      Side effect worth knowing: `bartelang parse` now shows the `Located`
      wrappers, which makes the dump more verbose but also shows each
      statement's line.

---

## Milestone 7 — More than one file  ✅ done

A program used to be one file.  `WScript.Shell.Exec "bartelang lib.btm"` runs a
*second* interpreter, so procedures, constants and records could not be shared -
the last thing standing between Bartelang and a reusable library.

- [x] **`Include "path.btm"`** — a real statement in the shape of VB6's own
      file-taking verbs (`Open`, `Kill`, `FileCopy`).  The path is a string
      literal resolved at load time.  A file is read once per canonical path, so
      diamond-shaped dependencies just work and a library's top-level code runs
      once; a cycle is error `1004` and prints the chain.  The file's statements
      are inserted at the `Include`: declarations hoist as they do in one file,
      top-level statements run once, in include order.  `Include` is legal only
      at a file's top level.  All files are `.btm` — a library is a script whose
      top level happens to hold only declarations — so there is no second
      extension to learn.
      Rejected: QBasic's `'$INCLUDE:` metacommand — era-correct, but a comment
      that sometimes executes is a lie in a language whose manual says `'`
      ignores the rest of the line.  Rejected: `Import`, for namespacing that is
      not happening.
- [x] **Resolution that respects the script, not the shell** — absolute path,
      else relative to the including file, else `-I <dir>` (repeatable), else
      `$BARTELANG_PATH`.  The CWD is deliberately not searched, and when a name
      does exist there the error says so and points at `-I .`.
- [x] **Diagnostics that name the file** — `Loaded` grew a `SourceMap`, and
      `Stmt::Located` a unit id, so `runtime error 11 in lib/math.btm at line 2`
      quotes *that* file's line, `bartelang parse` shows each statement's unit,
      and `Err.File` tells a `Catch` block which file failed.  A missing include
      reuses `53` (listing every directory tried); `1004` is a cycle and `1005` a
      declaration defined more than once — in one file or across files, at the
      top level or buried in a block, where it used to be silent last-wins.
- [x] **Library surface** — `load_file` / `execute_file`, and a `Loader` that
      carries include directories for embedders.
- [x] **A `#!` line is skipped**, so `#!/usr/bin/env bartelang` works.  Small,
      but it is the first thing a Unix user types.
- [x] **Tests and docs** — hoisting in both directions across files, include-once
      diamonds, cycle chains, resolution order with `-I` and `$BARTELANG_PATH`,
      errors inside an included file naming that file (CLI output and
      `Err.File`), duplicate declarations, and a shipped example:
      `examples/count_words.btm` with `examples/lib/text.btm`.  README gained a
      Modules section; the CLI gained `-I` and the usage text.

Behaviour change worth knowing: `Loaded.source_lines` is now the source map
(`loaded.sources`), `BrtError::render` and `render_syntax_error` take it, and
`Interp::run_loaded` is what a file-loaded program runs through.  `run` still
works unchanged — it simply has no file to name.

Deferred, deliberately: module-private state (the VB6-faithful model, where a
module's `Dim`s are private to it — it needs per-unit environments, and would
arrive together with `Private Sub` and qualified calls), dynamic
`ExecuteGlobal`-style loading, project manifests in the `.vbp` sense, and glob
includes.

---

## Milestone 8 — Commands and the environment  ✅ done (1.2.0)

Getting a script's values into a shell command used to mean one of two things:
build the string and hand it to `WScript.Shell.Exec`, or write the values into
the command text and hope. The first is verbose; the second is the `$` problem.

- [x] **`Capture(command)`** — a command substitution written as a string, for
      commands assembled from parts: `Capture("curl -s " & url)`. The same shell
      path and semantics as backticks (stdout, trailing newline stripped, exit
      code left in `ENV("?")`). VB6 built command strings and handed them to
      `Shell`, so this is the period-correct route with the ceremony removed.
- [x] **`$` in front of a backtick opts into interpolation.** Writing
      ``$`curl "$url"` `` evaluates like a string — `$name`, `${name}`, `\$` —
      while plain backticks stay pure shell text. The pipeline form takes the
      same marker, and the string form after `|` (legal, but silently *raw*
      until now) interpolates like every other string.
- [x] **`SetEnv name, value`** — completes `ENV(name)` in the other direction:
      the value is visible to `ENV()` and to every command run afterwards, so a
      plain backtick command can read it as `"$NAME"` and the shell does the
      quoting. The reserved `"?"` (last exit code) and an empty name are refused.
      VB6 had no way to hand its own environment to a child.
- [x] **Tests and docs** — the suite pins the three modes apart: a plain backtick
      expanding the *shell's* `$name` to nothing, the `$`-marked form expanding
      the script's, `\$` escaping through to the shell, an undefined variable
      inside an interpolated command reporting 500, `Capture` and the pipeline
      string form, and `SetEnv` reaching both `ENV()` and a child process.
      README gained the three-routes section and the lexical rule, and
      `examples/env_report.btm` runs every route end to end.

Why opt-in, and not "backticks interpolate, `\$` escapes": inside a shell command
there is no spare punctuation. `$` is the shell's; `@` is `user@host`; `~` is
tilde expansion; `\` is escaping; `#` starts a comment at a word start; `^`
anchors a regex; `%name%` collides with `date +%Y%m%d`; `{name}` collides with the
odd single-identifier `awk`/`jq` block. A marker makes the mode visible at the
call site and leaves every existing script's meaning alone — silently redefining
`$HOME` inside backticks is the one break this design refuses to make.

Deferred, deliberately: `UnsetEnv` (setting a variable to `""` is not the same as
removing it, so it is a real gap — nobody has needed it yet), an `Exec(cmd,
args...)` argv form that bypasses the shell entirely, and scoped environment
blocks (`WithEnv ... End WithEnv`).

---

## Out of scope, deliberately

Recorded here so the language stays coherent and nobody re-litigates it by
accident.

- **`GoTo`, `On Error Resume Next`, `On Error GoTo`, `GoSub`/`Return`** — the
  design document already replaced these with `Try/Catch`. Re-adding them would
  undo the single best decision in the specification.
- **`Clipboard`, `Screen`, `SendKeys`, `AppActivate`, `DoEvents`** — these assume
  a desktop session and a message pump. On a headless Linux box they are pure
  costume with no behaviour behind them, which is the same trap as
  `"MSXML2.XMLHTTP"`.
- **`ADODB.Connection` / `Recordset` / databases** — an enormous surface and the
  wrong era-fit for a shell tool.
- **`Class ... End Class`, `Property Get/Let/Set`, inheritance** — see the `Type`
  decision above.
- **`Option Explicit`** — its practical benefit already exists: reading an
  undeclared variable raises error 500.
- **VB6 source compatibility** — not a goal, by decision.

---

## Final stage — documentation

Once the milestones land:

- [x] Extend the README language tour with each new statement and function.
- [x] Add an object reference section covering every `CreateObject` type, its
      members and its failure modes.
- [x] Update the built-in function list, the error-number table, and the
      deliberate-decisions list.
- [x] Refresh the rustdoc (`//!` module docs and doc comments on every public
      type, function and method).  What is *not* individually documented is the
      mechanical vocabulary - token variants, and the `left`/`operator`-style
      payload fields of AST nodes - which the enclosing type and variant docs
      describe instead.  `cargo doc` builds without warnings.
- [x] Add at least one new example script that exercises the new objects
      together: `examples/log_stats.btm`, which is also run by the test suite.

---

## Open questions

1. **~~`IIf` evaluation.~~** *Resolved: short-circuits.* VB6's version evaluated
   both branches, which meant `IIf(x <> 0, 1 / x, 0)` divided by zero anyway.
   Bartelang only evaluates the branch it returns, and a user-defined
   `Function IIf` still shadows the built-in.
2. **~~Multi-line string syntax.~~** *Resolved:* `"""` with `$var`
   interpolation, kept verbatim.  A bare `"""` was previously a syntax error, so
   there was nothing to collide with.
3. **~~How is `Collection` reached?~~** *Resolved:* `CreateObject("Collection")`.
   It is the one name that was never a ProgID, and keeping a single construction
   mechanism beat adding a `New` keyword for one type.
4. **~~`HTTP` naming.~~** *Resolved:* kept plain.  It is the one type with no
   single canonical VB6 name (MSXML2 vs WinHttp vs ServerXMLHTTP, each with its
   own version suffix), and the vendor prefix was carrying no meaning.
5. **~~`Rnd`/`Randomize` backing.~~** *Resolved:* a dependency-free xorshift
   generator.  `Rnd` is not a cryptographic need, and its predecessor was a weak
   LCG.
6. **~~Milestone order.~~** *Resolved:* implemented in the order written.
7. **New:** assigning *through* a non-variable member receiver
   (`points(1).X = 5`) needs an lvalue path in the interpreter.  Reading such a
   receiver works today.
8. **~~Should `Err` learn the file name?~~** *Resolved: yes.*  `Err.File` joins
   `Err.Line`, because a bare line number stops meaning anything the moment a
   program has more than one file.  It is empty for the string API, where there
   is no file to name.
9. **~~A second extension for include-only libraries (`.bti`)?~~** *Resolved:
   no.*  Every file is `.btm`; a library is a script whose top level happens to
   hold only declarations.
