//! The Bartelang lexer.
//!
//! Turns raw source text into a flat `Vec<Token>` plus parallel line/column
//! tables so the parser and interpreter can produce pointed diagnostics.
//!
//! Retro rules implemented here:
//!   * everything is case-insensitive - identifiers are normalised to lowercase
//!   * `\n` and `\r\n` terminate statements; `:` separates statements on a line
//!   * a space followed by `_` then a newline is line continuation, no token
//!   * `'` starts a comment that runs to end of line
//!   * a leading `#!` line is a shebang and is skipped
//!   * `"..."` is a string; `""` inside a string is a literal quote
//!   * `` `...` `` is a command substitution, captured verbatim
//!   * `#1` is a vintage file channel handle

use crate::error::SyntaxError;

/// One lexical token.  Identifiers and keywords arrive lowercased, so every
/// comparison in the parser is case-insensitive for free.
#[derive(Clone, Debug, PartialEq)]
pub enum Token {
    /// Statement terminator (`\n` or `\r\n`).
    Newline,
    /// Statement separator on the same physical line (VB6's `:`).
    Colon,
    /// Identifier or keyword, normalised to lowercase.
    Ident(String),
    /// `#` - introduces a file channel number, as in `#1` or `#channel`.
    Hash,
    Int(i64),
    Float(f64),
    /// String literal contents, with `""` already collapsed to `"`.
    Str(String),
    /// `` `...` `` command substitution, captured exactly as typed.
    Backtick(String),

    Plus,
    Minus,
    Star,
    Slash,
    Backslash,
    Caret,
    Amp,

    Eq,
    Ne,
    Lt,
    Gt,
    Le,
    Ge,

    LParen,
    RParen,
    Comma,
    /// `;` - the tight print separator, and the trailing newline suppresser.
    Semicolon,
    Dot,
    Pipe,

    Eof,
}

impl Token {
    /// Human-readable description, used in parser diagnostics.
    pub fn describe(&self) -> String {
        match self {
            Token::Newline => "end of line".to_string(),
            Token::Colon => "':'".to_string(),
            Token::Ident(name) => format!("'{name}'"),
            Token::Int(v) => format!("'{v}'"),
            Token::Float(v) => format!("'{v}'"),
            Token::Str(_) => "a string literal".to_string(),
            Token::Backtick(_) => "a command substitution".to_string(),
            Token::Hash => "'#'".to_string(),
            Token::Plus => "'+'".to_string(),
            Token::Minus => "'-'".to_string(),
            Token::Star => "'*'".to_string(),
            Token::Slash => "'/'".to_string(),
            Token::Backslash => "'\\'".to_string(),
            Token::Caret => "'^'".to_string(),
            Token::Amp => "'&'".to_string(),
            Token::Eq => "'='".to_string(),
            Token::Ne => "'<>'".to_string(),
            Token::Lt => "'<'".to_string(),
            Token::Gt => "'>'".to_string(),
            Token::Le => "'<='".to_string(),
            Token::Ge => "'>='".to_string(),
            Token::LParen => "'('".to_string(),
            Token::RParen => "')'".to_string(),
            Token::Comma => "','".to_string(),
            Token::Semicolon => "';'".to_string(),
            Token::Dot => "'.'".to_string(),
            Token::Pipe => "'|'".to_string(),
            Token::Eof => "end of file".to_string(),
        }
    }

    /// True when the token ends a statement.
    pub fn ends_statement(&self) -> bool {
        matches!(self, Token::Newline | Token::Colon | Token::Eof)
    }
}

/// A tokenised program: the token stream plus the position tables and the
/// original source lines needed for diagnostics.
#[derive(Clone, Debug)]
pub struct Lexed {
    pub tokens: Vec<Token>,
    /// 1-based line of each token (parallel to `tokens`).
    pub lines: Vec<usize>,
    /// 1-based column of each token (parallel to `tokens`), in characters.
    pub cols: Vec<usize>,
    /// The original source split on newlines, for error rendering.
    pub source_lines: Vec<String>,
}

const KEYWORD_CHARS: &str = "_";

fn is_ident_start(c: char) -> bool {
    c.is_ascii_alphabetic() || KEYWORD_CHARS.contains(c)
}

fn is_ident_continue(c: char) -> bool {
    c.is_ascii_alphanumeric() || KEYWORD_CHARS.contains(c)
}

/// Tokenise `src`.
pub fn tokenize(src: &str) -> Result<Lexed, SyntaxError> {
    let chars: Vec<char> = src.chars().collect();
    let source_lines: Vec<String> = src
        .split('\n')
        .map(|line| line.strip_suffix('\r').unwrap_or(line).to_string())
        .collect();

    let mut tokens: Vec<Token> = Vec::new();
    let mut lines: Vec<usize> = Vec::new();
    let mut cols: Vec<usize> = Vec::new();

    let n = chars.len();
    let mut i = 0usize;
    let mut line = 1usize;
    // Index at which the current line starts, so columns are line-relative.
    let mut line_start = 0usize;

    // A leading `#!` line is a shebang, not source: `#!/usr/bin/env bartelang`
    // lets a script be executed directly.  Only line 1, and the newline is left
    // in place so line numbers keep matching the file.
    if chars.starts_with(&['#', '!']) {
        while i < n && chars[i] != '\n' {
            i += 1;
        }
    }

    while i < n {
        let c = chars[i];
        let col = i - line_start + 1;
        match c {
            ' ' | '\t' | '\r' => {
                i += 1;
            }
            '\n' => {
                tokens.push(Token::Newline);
                lines.push(line);
                cols.push(col);
                line += 1;
                i += 1;
                line_start = i;
            }
            ':' => {
                tokens.push(Token::Colon);
                lines.push(line);
                cols.push(col);
                i += 1;
            }
            '\'' => {
                while i < n && chars[i] != '\n' {
                    i += 1;
                }
            }
            '"' => {
                // Three quotes open a multi-line string, which is our one
                // knowing break from the 1998 rules.  It is unambiguous here
                // because `"""` used to be a syntax error.
                if i + 2 < n && chars[i + 1] == '"' && chars[i + 2] == '"' {
                    i += 3;
                    let mut raw = String::new();
                    let mut closed = false;
                    while i < n {
                        if chars[i] == '"'
                            && i + 2 < n
                            && chars[i + 1] == '"'
                            && chars[i + 2] == '"'
                        {
                            i += 3;
                            closed = true;
                            break;
                        }
                        if chars[i] == '\n' {
                            line += 1;
                            line_start = i + 1;
                        }
                        raw.push(chars[i]);
                        i += 1;
                    }
                    if !closed {
                        return Err(SyntaxError::new(
                            "unterminated multi-line string literal (missing closing \"\"\")",
                            line,
                            col,
                        ));
                    }
                    tokens.push(Token::Str(raw));
                    lines.push(line);
                    cols.push(col);
                } else {
                    i += 1;
                    let mut raw = String::new();
                    let mut closed = false;
                    while i < n {
                        let ch = chars[i];
                        if ch == '"' {
                            if i + 1 < n && chars[i + 1] == '"' {
                                raw.push('"');
                                i += 2;
                                continue;
                            }
                            closed = true;
                            i += 1;
                            break;
                        }
                        if ch == '\n' {
                            break;
                        }
                        raw.push(ch);
                        i += 1;
                    }
                    if !closed {
                        return Err(SyntaxError::new(
                            "unterminated string literal",
                            line,
                            col,
                        ));
                    }
                    tokens.push(Token::Str(raw));
                    lines.push(line);
                    cols.push(col);
                }
            }
            '`' => {
                i += 1;
                let mut cmd = String::new();
                let mut closed = false;
                while i < n {
                    let ch = chars[i];
                    if ch == '`' {
                        closed = true;
                        i += 1;
                        break;
                    }
                    if ch == '\n' {
                        line += 1;
                        line_start = i + 1;
                    }
                    cmd.push(ch);
                    i += 1;
                }
                if !closed {
                    return Err(SyntaxError::new(
                        "unterminated command substitution (missing closing '`')",
                        line,
                        col,
                    ));
                }
                tokens.push(Token::Backtick(cmd));
                lines.push(line);
                cols.push(col);
            }
            '#' => {
                // A file number.  `Token::Hash` only introduces it; the number
                // itself is parsed as an expression, which is what let period
                // code write `As #FreeFile()`.
                tokens.push(Token::Hash);
                lines.push(line);
                cols.push(col);
                i += 1;
            }
            '_' => {
                // Line continuation is "space, underscore, newline".  Anything
                // else starting with an underscore is an ordinary identifier.
                let prev_is_space = i == 0 || chars[i - 1] == ' ' || chars[i - 1] == '\t';
                let mut j = i + 1;
                while j < n && (chars[j] == ' ' || chars[j] == '\t') {
                    j += 1;
                }
                let at_line_end = j >= n || chars[j] == '\n' || chars[j] == '\r';
                if prev_is_space && at_line_end {
                    i = j;
                    if i < n && chars[i] == '\r' {
                        i += 1;
                    }
                    if i < n && chars[i] == '\n' {
                        i += 1;
                        line += 1;
                        line_start = i;
                    }
                } else {
                    let start = i;
                    while i < n && is_ident_continue(chars[i]) {
                        i += 1;
                    }
                    let name: String = chars[start..i].iter().collect();
                    tokens.push(Token::Ident(name.to_lowercase()));
                    lines.push(line);
                    cols.push(col);
                }
            }
            _ if is_ident_start(c) => {
                let start = i;
                while i < n && is_ident_continue(chars[i]) {
                    i += 1;
                }
                let name: String = chars[start..i].iter().collect();
                tokens.push(Token::Ident(name.to_lowercase()));
                lines.push(line);
                cols.push(col);
            }
            _ if c.is_ascii_digit() => {
                let start = i;
                while i < n && chars[i].is_ascii_digit() {
                    i += 1;
                }
                let mut is_float = false;
                if i + 1 < n && chars[i] == '.' && chars[i + 1].is_ascii_digit() {
                    is_float = true;
                    i += 1;
                    while i < n && chars[i].is_ascii_digit() {
                        i += 1;
                    }
                }
                if i < n && (chars[i] == 'e' || chars[i] == 'E') {
                    let mut k = i + 1;
                    if k < n && (chars[k] == '+' || chars[k] == '-') {
                        k += 1;
                    }
                    if k < n && chars[k].is_ascii_digit() {
                        is_float = true;
                        i = k;
                        while i < n && chars[i].is_ascii_digit() {
                            i += 1;
                        }
                    }
                }
                let text: String = chars[start..i].iter().collect();
                if is_float {
                    let value: f64 = text.parse().map_err(|_| {
                        SyntaxError::new(format!("invalid number literal '{text}'"), line, col)
                    })?;
                    tokens.push(Token::Float(value));
                } else {
                    match text.parse::<i64>() {
                        Ok(value) => tokens.push(Token::Int(value)),
                        Err(_) => {
                            let value: f64 = text.parse().map_err(|_| {
                                SyntaxError::new(
                                    format!("invalid number literal '{text}'"),
                                    line,
                                    col,
                                )
                            })?;
                            tokens.push(Token::Float(value));
                        }
                    }
                }
                lines.push(line);
                cols.push(col);
            }
            '<' => {
                if i + 1 < n && chars[i + 1] == '>' {
                    tokens.push(Token::Ne);
                    i += 2;
                } else if i + 1 < n && chars[i + 1] == '=' {
                    tokens.push(Token::Le);
                    i += 2;
                } else {
                    tokens.push(Token::Lt);
                    i += 1;
                }
                lines.push(line);
                cols.push(col);
            }
            '>' => {
                if i + 1 < n && chars[i + 1] == '=' {
                    tokens.push(Token::Ge);
                    i += 2;
                } else {
                    tokens.push(Token::Gt);
                    i += 1;
                }
                lines.push(line);
                cols.push(col);
            }
            '=' => {
                tokens.push(Token::Eq);
                lines.push(line);
                cols.push(col);
                i += 1;
            }
            '+' => {
                tokens.push(Token::Plus);
                lines.push(line);
                cols.push(col);
                i += 1;
            }
            '-' => {
                tokens.push(Token::Minus);
                lines.push(line);
                cols.push(col);
                i += 1;
            }
            '*' => {
                tokens.push(Token::Star);
                lines.push(line);
                cols.push(col);
                i += 1;
            }
            '/' => {
                tokens.push(Token::Slash);
                lines.push(line);
                cols.push(col);
                i += 1;
            }
            '\\' => {
                tokens.push(Token::Backslash);
                lines.push(line);
                cols.push(col);
                i += 1;
            }
            '^' => {
                tokens.push(Token::Caret);
                lines.push(line);
                cols.push(col);
                i += 1;
            }
            '&' => {
                tokens.push(Token::Amp);
                lines.push(line);
                cols.push(col);
                i += 1;
            }
            '(' => {
                tokens.push(Token::LParen);
                lines.push(line);
                cols.push(col);
                i += 1;
            }
            ')' => {
                tokens.push(Token::RParen);
                lines.push(line);
                cols.push(col);
                i += 1;
            }
            ',' => {
                tokens.push(Token::Comma);
                lines.push(line);
                cols.push(col);
                i += 1;
            }
            ';' => {
                tokens.push(Token::Semicolon);
                lines.push(line);
                cols.push(col);
                i += 1;
            }
            '.' => {
                tokens.push(Token::Dot);
                lines.push(line);
                cols.push(col);
                i += 1;
            }
            '|' => {
                tokens.push(Token::Pipe);
                lines.push(line);
                cols.push(col);
                i += 1;
            }
            other => {
                return Err(SyntaxError::new(
                    format!("unexpected character '{other}'"),
                    line,
                    col,
                ));
            }
        }
    }

    // Guarantee the stream ends with a statement terminator.
    if !matches!(
        tokens.last(),
        Some(Token::Newline) | Some(Token::Colon) | None
    ) {
        tokens.push(Token::Newline);
        lines.push(line);
        cols.push(1);
    }
    tokens.push(Token::Eof);
    lines.push(line);
    cols.push(1);

    Ok(Lexed {
        tokens,
        lines,
        cols,
        source_lines,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn kinds(src: &str) -> Vec<Token> {
        tokenize(src).unwrap().tokens
    }

    #[test]
    fn keywords_are_normalised() {
        assert_eq!(
            kinds("DIM x\n"),
            vec![Token::Ident("dim".into()), Token::Ident("x".into()), Token::Newline, Token::Eof]
        );
        assert_eq!(kinds("Dim x\n"), kinds("dim X\n"));
    }

    #[test]
    fn line_continuation_is_invisible() {
        // Two physical lines, but only one statement terminator.
        let continued = kinds("Let x = 1 _\n  + 2\n");
        let flat = kinds("Let x = 1 + 2\n");
        assert_eq!(continued, flat);
        assert_eq!(continued.iter().filter(|t| **t == Token::Newline).count(), 1);
    }

    #[test]
    fn underscore_identifiers_still_work() {
        assert_eq!(
            kinds("Dim my_var\n")[1],
            Token::Ident("my_var".into())
        );
    }

    #[test]
    fn comments_run_to_end_of_line() {
        let without = kinds("Dim x\n");
        // A whole-line comment contributes nothing but its newline.
        let with_comment = kinds("' whole line\nDim x\n");
        assert_eq!(with_comment.len(), without.len() + 1);
        assert_eq!(with_comment[1..], without[..]);
        // A trailing comment is stripped from the statement.
        assert_eq!(kinds("Dim x ' trailing\n"), without);
    }

    #[test]
    fn carved_tokens() {
        assert_eq!(kinds("`ls -la`\n")[0], Token::Backtick("ls -la".into()));
        // `#12` is a hash followed by the number, so the number may be an
        // expression.
        assert_eq!(
            kinds("Open \"f\" For Append As #12\n")[5..7],
            [Token::Hash, Token::Int(12)]
        );
        assert_eq!(kinds("\"a\"\"b\"\n")[0], Token::Str("a\"b".into()));
    }

    #[test]
    fn quotes_inside_backticks_are_not_comments() {
        let toks = kinds("`tr '[a-z]' '[A-Z]'`\n");
        assert_eq!(toks[0], Token::Backtick("tr '[a-z]' '[A-Z]'".into()));
    }

    #[test]
    fn multi_char_operators() {
        assert_eq!(
            kinds("<> <= >=\n")[..3],
            [Token::Ne, Token::Le, Token::Ge]
        );
    }

    #[test]
    fn crlf_terminates_statements() {
        assert_eq!(kinds("Dim x\r\nDim y\r\n").len(), 7);
    }

    #[test]
    fn columns_are_relative_to_their_line() {
        let lexed = tokenize("Dim x\nLet y = 1\n").unwrap();
        let index = lexed
            .lines
            .iter()
            .position(|line| *line == 2)
            .expect("a token on line 2");
        assert_eq!(lexed.tokens[index], Token::Ident("let".into()));
        assert_eq!(lexed.cols[index], 1);
        // The newline at the end of "Let y = 1" sits just past the text.
        let newline = lexed
            .tokens
            .iter()
            .position(|t| *t == Token::Newline && true)
            .unwrap();
        assert!(lexed.cols[newline] <= 10);
    }

    #[test]
    fn multi_line_strings_span_lines_and_keep_everything() {
        let toks = kinds("\"\"\"\nline one\nline two\n\"\"\"\n");
        assert_eq!(toks[0], Token::Str("\nline one\nline two\n".into()));
        // The newlines inside do not become statement terminators.
        assert_eq!(toks.iter().filter(|t| **t == Token::Newline).count(), 1);
    }

    #[test]
    fn multi_line_strings_ignore_comment_and_continuation_rules_inside() {
        let toks = kinds("\"\"\"\n' not a comment\n_ not a continuation\n\"\"\"\n");
        assert_eq!(
            toks[0],
            Token::Str("\n' not a comment\n_ not a continuation\n".into())
        );
    }

    #[test]
    fn an_ordinary_string_still_collapses_doubled_quotes() {
        assert_eq!(kinds("\"a\"\"b\"\n")[0], Token::Str("a\"b".into()));
    }

    #[test]
    fn an_unterminated_multi_line_string_is_reported() {
        let error = tokenize("\"\"\"\nnever closed\n").unwrap_err();
        assert!(error.message.contains("multi-line"), "{error}");
    }

    #[test]
    fn unterminated_string_is_an_error() {
        assert!(tokenize("\"oops\n").is_err());
    }
}
