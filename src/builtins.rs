//! The Bartelang standard library: the globals `ARGS`/`ENV` and the native
//! built-in functions.
//!
//! Everything that touches string positions is strictly 1-based, as the
//! language specification demands.

use std::cell::RefCell;
use std::fs::File;
use std::io::Seek;
use std::rc::Rc;

use crate::error::BrtError;
use crate::interp::{channel_at_eof, Interp};
use crate::objects::create_object;
use crate::value::Variant;

/// Checks a built-in's arity, producing VB6 error 450 when it is wrong.
fn require(name: &str, args: &[Variant], min: usize, max: usize) -> Result<(), BrtError> {
    if args.len() < min || args.len() > max {
        let wanted = if min == max {
            min.to_string()
        } else {
            format!("{min} to {max}")
        };
        return Err(BrtError::wrong_arg_count(name, args.len(), &wanted));
    }
    Ok(())
}

fn first_is_not_object(name: &str, value: &Variant) -> Result<String, BrtError> {
    value
        .as_string()
        .map_err(|_| BrtError::invalid_call(format!("{name} cannot be applied to an object")))
}

impl Interp {
    /// Borrows an open file channel, or reports error 52.
    pub(crate) fn channel(&mut self, channel: u8) -> Result<&mut File, BrtError> {
        self.globals
            .file_channels
            .get_mut(&channel)
            .ok_or_else(|| BrtError::bad_file_channel(channel))
    }

    pub(crate) fn call_builtin(
        &mut self,
        name: &str,
        args: Vec<Variant>,
    ) -> Result<Variant, BrtError> {
        match name {
            // ------------------------------------------------- global state
            "args" => {
                require(name, &args, 1, 1)?;
                let index = args[0].to_int()?;
                if index < 1 {
                    return Err(BrtError::subscript(
                        "ARGS is 1-based: ARGS(1) is the first argument",
                    ));
                }
                Ok(Variant::String(
                    self.cli_args
                        .get((index - 1) as usize)
                        .cloned()
                        .unwrap_or_default(),
                ))
            }
            "env" => {
                require(name, &args, 1, 1)?;
                let key = first_is_not_object(name, &args[0])?;
                if key == "?" {
                    // The exit code of the most recent shell command.
                    return Ok(Variant::Int(self.globals.last_exit_code as i64));
                }
                Ok(Variant::String(std::env::var(&key).unwrap_or_default()))
            }
            "inputbox" => {
                require(name, &args, 1, 1)?;
                let prompt = first_is_not_object(name, &args[0])?;
                crate::emit_prompt(&prompt);
                let mut line = String::new();
                std::io::stdin()
                    .read_line(&mut line)
                    .map_err(|e| BrtError::invalid_call(format!("InputBox failed: {e}")))?;
                let line = line.strip_suffix('\n').unwrap_or(&line);
                let line = line.strip_suffix('\r').unwrap_or(line);
                Ok(Variant::String(line.to_string()))
            }
            "createobject" => {
                require(name, &args, 1, 1)?;
                let type_name = first_is_not_object(name, &args[0])?;
                create_object(&type_name)
            }

            // ------------------------------------------------------ strings
            "len" => {
                require(name, &args, 1, 1)?;
                Ok(Variant::Int(
                    first_is_not_object(name, &args[0])?.chars().count() as i64,
                ))
            }
            "ucase" => {
                require(name, &args, 1, 1)?;
                Ok(Variant::String(
                    first_is_not_object(name, &args[0])?.to_uppercase(),
                ))
            }
            "lcase" => {
                require(name, &args, 1, 1)?;
                Ok(Variant::String(
                    first_is_not_object(name, &args[0])?.to_lowercase(),
                ))
            }
            "trim" => {
                require(name, &args, 1, 1)?;
                Ok(Variant::String(
                    first_is_not_object(name, &args[0])?.trim().to_string(),
                ))
            }
            "ltrim" => {
                require(name, &args, 1, 1)?;
                Ok(Variant::String(
                    first_is_not_object(name, &args[0])?
                        .trim_start()
                        .to_string(),
                ))
            }
            "rtrim" => {
                require(name, &args, 1, 1)?;
                Ok(Variant::String(
                    first_is_not_object(name, &args[0])?
                        .trim_end()
                        .to_string(),
                ))
            }
            "left" => {
                require(name, &args, 2, 2)?;
                let text = first_is_not_object(name, &args[0])?;
                let count = args[1].to_int()?;
                if count < 0 {
                    return Err(BrtError::invalid_call("Left length cannot be negative"));
                }
                Ok(Variant::String(
                    text.chars().take(count as usize).collect(),
                ))
            }
            "right" => {
                require(name, &args, 2, 2)?;
                let text = first_is_not_object(name, &args[0])?;
                let count = args[1].to_int()?;
                if count < 0 {
                    return Err(BrtError::invalid_call("Right length cannot be negative"));
                }
                let chars: Vec<char> = text.chars().collect();
                let take = (count as usize).min(chars.len());
                Ok(Variant::String(
                    chars[chars.len() - take..].iter().collect(),
                ))
            }
            "mid" => {
                require(name, &args, 2, 3)?;
                let text = first_is_not_object(name, &args[0])?;
                // 1-based, unlike Rust.
                let start = args[1].to_int()?;
                if start < 1 {
                    return Err(BrtError::subscript(
                        "Mid is 1-based: the start position must be at least 1",
                    ));
                }
                let chars: Vec<char> = text.chars().collect();
                let from = (start - 1) as usize;
                if from >= chars.len() {
                    return Ok(Variant::String(String::new()));
                }
                let available = chars.len() - from;
                let take = match args.get(2) {
                    Some(value) => {
                        let length = value.to_int()?;
                        if length < 0 {
                            return Err(BrtError::invalid_call(
                                "Mid length cannot be negative",
                            ));
                        }
                        (length as usize).min(available)
                    }
                    None => available,
                };
                Ok(Variant::String(chars[from..from + take].iter().collect()))
            }
            "instr" => {
                require(name, &args, 2, 3)?;
                let (start, haystack, needle) = match args.len() {
                    2 => (1i64, first_is_not_object(name, &args[0])?, first_is_not_object(name, &args[1])?),
                    3 => (
                        args[0].to_int()?,
                        first_is_not_object(name, &args[1])?,
                        first_is_not_object(name, &args[2])?,
                    ),
                    _ => unreachable!(),
                };
                if start < 1 {
                    return Err(BrtError::subscript(
                        "InStr is 1-based: the start position must be at least 1",
                    ));
                }
                let hay: Vec<char> = haystack.chars().collect();
                let from = (start - 1) as usize;
                if from > hay.len() {
                    return Ok(Variant::Int(0));
                }
                let tail: String = hay[from..].iter().collect();
                match tail.find(&needle) {
                    Some(byte_offset) => {
                        // Convert the byte offset back into a 1-based character index.
                        let prefix_chars = tail[..byte_offset].chars().count();
                        Ok(Variant::Int((from + prefix_chars + 1) as i64))
                    }
                    None => Ok(Variant::Int(0)),
                }
            }
            "instrrev" => {
                require(name, &args, 2, 2)?;
                let haystack = first_is_not_object(name, &args[0])?;
                let needle = first_is_not_object(name, &args[1])?;
                match haystack.rfind(&needle) {
                    Some(byte_offset) => Ok(Variant::Int(
                        (haystack[..byte_offset].chars().count() + 1) as i64,
                    )),
                    None => Ok(Variant::Int(0)),
                }
            }
            "replace" => {
                require(name, &args, 3, 3)?;
                let text = first_is_not_object(name, &args[0])?;
                let from = first_is_not_object(name, &args[1])?;
                let to = first_is_not_object(name, &args[2])?;
                if from.is_empty() {
                    return Ok(Variant::String(text));
                }
                Ok(Variant::String(text.replace(&from, &to)))
            }
            "space" => {
                require(name, &args, 1, 1)?;
                let count = args[0].to_int()?.max(0) as usize;
                Ok(Variant::String(" ".repeat(count)))
            }
            "string" => {
                require(name, &args, 2, 2)?;
                let count = args[0].to_int()?.max(0) as usize;
                let filler = first_is_not_object(name, &args[1])?;
                let ch = filler.chars().next().unwrap_or(' ');
                Ok(Variant::String(ch.to_string().repeat(count)))
            }
            "chr" => {
                require(name, &args, 1, 1)?;
                let code = args[0].to_int()?;
                let ch = u32::try_from(code)
                    .ok()
                    .and_then(char::from_u32)
                    .ok_or_else(|| {
                        BrtError::invalid_call(format!("Chr: {code} is not a character code"))
                    })?;
                Ok(Variant::String(ch.to_string()))
            }
            "asc" => {
                require(name, &args, 1, 1)?;
                let text = first_is_not_object(name, &args[0])?;
                match text.chars().next() {
                    Some(ch) => Ok(Variant::Int(ch as i64)),
                    None => Err(BrtError::invalid_call("Asc of an empty string")),
                }
            }
            "val" => {
                require(name, &args, 1, 1)?;
                let text = first_is_not_object(name, &args[0])?;
                Ok(parse_numeric_prefix(&text))
            }
            "str" => {
                require(name, &args, 1, 1)?;
                let value = args[0].to_num()?;
                let text = match value {
                    crate::value::Num::Int(v) => v.to_string(),
                    crate::value::Num::Float(v) => crate::value::format_float(v),
                };
                // VB6 reserves a leading space for the sign.
                Ok(Variant::String(if text.starts_with('-') {
                    text
                } else {
                    format!(" {text}")
                }))
            }
            "hex" => {
                require(name, &args, 1, 1)?;
                let value = args[0].to_int()?;
                Ok(Variant::String(if value < 0 {
                    format!("{:X}", value as i32 as u32)
                } else {
                    format!("{value:X}")
                }))
            }
            "oct" => {
                require(name, &args, 1, 1)?;
                let value = args[0].to_int()?;
                Ok(Variant::String(if value < 0 {
                    format!("{:o}", value as i32 as u32)
                } else {
                    format!("{value:o}")
                }))
            }

            // --------------------------------------------------- conversions
            "cstr" => {
                require(name, &args, 1, 1)?;
                Ok(Variant::String(first_is_not_object(name, &args[0])?))
            }
            "cint" | "clng" => {
                require(name, &args, 1, 1)?;
                // VB6 rounds rather than truncates, half to even.
                Ok(Variant::Int(round_half_even(args[0].to_float()?) as i64))
            }
            "cdbl" | "csng" => {
                require(name, &args, 1, 1)?;
                Ok(Variant::Float(args[0].to_float()?))
            }
            "cbool" => {
                require(name, &args, 1, 1)?;
                Ok(Variant::Bool(args[0].truthy()))
            }

            // --------------------------------------------------------- maths
            "abs" => {
                require(name, &args, 1, 1)?;
                Ok(match args[0].to_num()? {
                    crate::value::Num::Int(v) => Variant::Int(v.wrapping_abs()),
                    crate::value::Num::Float(v) => Variant::Float(v.abs()),
                })
            }
            "int" => {
                require(name, &args, 1, 1)?;
                // VB6's Int rounds *down*, toward negative infinity: Int(-2.5)
                // is -3.  It is Fix that truncates toward zero.
                Ok(Variant::Float(args[0].to_float()?.floor()))
            }
            "fix" => {
                require(name, &args, 1, 1)?;
                Ok(Variant::Float(args[0].to_float()?.trunc()))
            }
            "sgn" => {
                require(name, &args, 1, 1)?;
                let value = args[0].to_float()?;
                Ok(Variant::Int(if value > 0.0 {
                    1
                } else if value < 0.0 {
                    -1
                } else {
                    0
                }))
            }
            "sqr" => {
                require(name, &args, 1, 1)?;
                let value = args[0].to_float()?;
                if value < 0.0 {
                    return Err(BrtError::invalid_call("Sqr of a negative number"));
                }
                Ok(Variant::Float(value.sqrt()))
            }

            // --------------------------------------------------------- dates
            "now" => {
                require(name, &args, 0, 0)?;
                let (date, time) = current_date_time();
                Ok(Variant::String(format!("{date} {time}")))
            }
            "date" => {
                require(name, &args, 0, 0)?;
                let (date, _) = current_date_time();
                Ok(Variant::String(date))
            }
            "time" => {
                require(name, &args, 0, 0)?;
                let (_, time) = current_date_time();
                Ok(Variant::String(time))
            }

            // -------------------------------------------------- introspection
            "isnumeric" => {
                require(name, &args, 1, 1)?;
                Ok(Variant::Bool(args[0].to_num().is_ok()))
            }
            "isempty" => {
                require(name, &args, 1, 1)?;
                Ok(Variant::Bool(matches!(args[0], Variant::Empty)))
            }
            "eof" => {
                require(name, &args, 1, 1)?;
                let channel = args[0].to_int()?;
                let channel = u8::try_from(channel)
                    .map_err(|_| BrtError::bad_file_channel(0))?;
                let file = self
                    .globals
                    .file_channels
                    .get_mut(&channel)
                    .ok_or_else(|| BrtError::bad_file_channel(channel))?;
                let at_end = channel_at_eof(file)
                    .map_err(|e| BrtError::invalid_call(format!("EOF(#{channel}) failed: {e}")))?;
                Ok(Variant::Bool(at_end))
            }

            // ---------------------------------------------------------- arrays
            "array" => Ok(Variant::Array(Rc::new(RefCell::new(args)))),
            "split" => {
                require(name, &args, 1, 3)?;
                let source = first_is_not_object(name, &args[0])?;
                let delimiter = match args.get(1) {
                    Some(value) => value.as_string()?,
                    None => " ".to_string(),
                };
                let limit = match args.get(2) {
                    Some(value) => value.to_int()?.max(0) as usize,
                    None => 0,
                };
                let parts = split_into_parts(&source, &delimiter, limit);
                Ok(Variant::Array(Rc::new(RefCell::new(parts))))
            }
            "join" => {
                require(name, &args, 1, 2)?;
                let storage = expect_array(name, &args[0])?;
                let delimiter = match args.get(1) {
                    Some(value) => value.as_string()?,
                    None => " ".to_string(),
                };
                let elements = storage.borrow();
                let pieces: Vec<String> = elements
                    .iter()
                    .map(|element| element.display_string())
                    .collect();
                Ok(Variant::String(pieces.join(&delimiter)))
            }
            "lbound" => {
                require(name, &args, 1, 2)?;
                expect_array(name, &args[0])?;
                // Arrays are always 1-based here, whatever their length.
                Ok(Variant::Int(1))
            }
            "ubound" => {
                require(name, &args, 1, 2)?;
                let storage = expect_array(name, &args[0])?;
                // An empty array reports 0, so `UBound < LBound` detects it.
                Ok(Variant::Int(storage.borrow().len() as i64))
            }

            // ------------------------------------------------ file channels
            "freefile" => {
                require(name, &args, 0, 0)?;
                for candidate in 1u8..=255 {
                    if !self.globals.file_channels.contains_key(&candidate) {
                        return Ok(Variant::Int(candidate as i64));
                    }
                }
                Err(BrtError::new(67, "Too many files open"))
            }
            "lof" => {
                require(name, &args, 1, 1)?;
                let channel = channel_argument(name, &args[0])?;
                let length = self
                    .channel(channel)?
                    .metadata()
                    .map_err(|e| BrtError::new(5, format!("LOF(#{channel}) failed: {e}")))?
                    .len();
                Ok(Variant::Int(length as i64))
            }
            // `Loc` and `Seek` both report the byte position of the next read or
            // write; VB6 distinguished them for random-access files, which
            // Bartelang does not have.
            "loc" | "seek" => {
                require(name, &args, 1, 1)?;
                let channel = channel_argument(name, &args[0])?;
                let position = self
                    .channel(channel)?
                    .stream_position()
                    .map_err(|e| BrtError::new(5, format!("{name}(#{channel}) failed: {e}")))?;
                Ok(Variant::Int(position as i64))
            }

            // -------------------------------------------------- introspection
            "typename" => {
                require(name, &args, 1, 1)?;
                // Records and objects report the name they were declared or
                // created with, as VB6 did; everything else reports its Variant
                // type.
                Ok(Variant::String(match &args[0] {
                    Variant::Record(record) => record.borrow().type_name.clone(),
                    Variant::Object(handle) => handle.borrow().type_name(),
                    other => other.type_name().to_string(),
                }))
            }
            "isobject" => {
                require(name, &args, 1, 1)?;
                Ok(Variant::Bool(matches!(args[0], Variant::Object(_))))
            }
            "isarray" => {
                require(name, &args, 1, 1)?;
                Ok(Variant::Bool(matches!(args[0], Variant::Array(_))))
            }
            "isnull" => {
                require(name, &args, 1, 1)?;
                Ok(Variant::Bool(matches!(args[0], Variant::Null)))
            }
            "isdate" => {
                require(name, &args, 1, 1)?;
                Ok(Variant::Bool(match &args[0] {
                    Variant::String(text) => crate::datetime::parse_timestamp(text).is_some(),
                    _ => false,
                }))
            }

            // -------------------------------------------------------- numbers
            "format" => {
                require(name, &args, 1, 2)?;
                let mask = match args.get(1) {
                    Some(value) => value.as_string()?,
                    None => "General Number".to_string(),
                };
                Ok(Variant::String(crate::format::apply(&mask, &args[0])?))
            }
            "round" => {
                require(name, &args, 1, 2)?;
                let places = match args.get(1) {
                    Some(value) => value.to_int()?.clamp(0, 15) as i32,
                    None => 0,
                };
                let factor = 10f64.powi(places);
                // VB6 rounded half to even, like the `CInt` above.
                Ok(Variant::Float(round_half_even(
                    args[0].to_float()? * factor,
                ) / factor))
            }
            "rnd" => {
                require(name, &args, 0, 1)?;
                // VB6's argument asked for the next value or a repeat; the
                // selector carried no interesting behaviour, so it is ignored.
                Ok(Variant::Float(self.rng.next_f64()))
            }
            "randomize" => {
                require(name, &args, 0, 1)?;
                self.rng.reseed();
                Ok(Variant::Empty)
            }
            "timer" => {
                require(name, &args, 0, 0)?;
                let now = crate::datetime::now();
                let fraction = std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map(|elapsed| elapsed.as_secs_f64().fract())
                    .unwrap_or(0.0);
                Ok(Variant::Float(
                    now.hour as f64 * 3600.0
                        + now.minute as f64 * 60.0
                        + now.second as f64
                        + fraction,
                ))
            }

            // ---------------------------------------------------------- dates
            "dateadd" => {
                require(name, &args, 3, 3)?;
                let interval = args[0].as_string()?.to_ascii_lowercase();
                let amount = args[1].to_int()?;
                let time = timestamp_argument(name, &args[2])?;
                match crate::datetime::add_interval(&time, &interval, amount) {
                    Some(result) => Ok(Variant::String(result.date_time())),
                    None => Err(BrtError::invalid_call(format!(
                        "DateAdd: unknown interval {interval:?} (use yyyy, q, m, d, w, ww, h, n or s)"
                    ))),
                }
            }
            "datediff" => {
                require(name, &args, 3, 3)?;
                let interval = args[0].as_string()?.to_ascii_lowercase();
                let start = timestamp_argument(name, &args[1])?;
                let end = timestamp_argument(name, &args[2])?;
                match crate::datetime::diff_interval(&start, &end, &interval) {
                    Some(value) => Ok(Variant::Int(value)),
                    None => Err(BrtError::invalid_call(format!(
                        "DateDiff: unknown interval {interval:?} (use yyyy, q, m, d, w, ww, h, n or s)"
                    ))),
                }
            }
            "datepart" => {
                require(name, &args, 2, 2)?;
                let interval = args[0].as_string()?.to_ascii_lowercase();
                let time = timestamp_argument(name, &args[1])?;
                match crate::datetime::part_interval(&time, &interval) {
                    Some(value) => Ok(Variant::Int(value)),
                    None => Err(BrtError::invalid_call(format!(
                        "DatePart: unknown interval {interval:?} (use yyyy, q, m, d, y, w, ww, h, n or s)"
                    ))),
                }
            }

            other => Err(BrtError::not_defined(other)),
        }
    }
}

/// A file-channel argument such as the `1` in `LOF(1)`.
fn channel_argument(name: &str, value: &Variant) -> Result<u8, BrtError> {
    let channel = value.to_int()?;
    u8::try_from(channel).map_err(|_| {
        BrtError::new(52, format!("{name}: #{channel} is not a valid file channel"))
    })
}

/// A timestamp argument: one of the strings `Now()` and `FileDateTime()` make.
fn timestamp_argument(name: &str, value: &Variant) -> Result<crate::datetime::LocalTime, BrtError> {
    match value {
        Variant::String(text) => crate::datetime::parse_timestamp(text)
            .ok_or_else(|| BrtError::invalid_call(format!("{name}: {text:?} is not a timestamp"))),
        other => Err(BrtError::invalid_call(format!(
            "{name}: expected a timestamp, got {}",
            other.type_name()
        ))),
    }
}

/// The storage behind an array argument, or a type mismatch naming what came.
fn expect_array(name: &str, value: &Variant) -> Result<Rc<RefCell<Vec<Variant>>>, BrtError> {
    match value {
        Variant::Array(storage) => Ok(storage.clone()),
        other => Err(BrtError::type_mismatch(format!(
            "{name} needs an array, got {}",
            other.type_name()
        ))),
    }
}

/// VB6's `Split`: an empty source gives an empty array, a zero `limit` means
/// "no limit", and a non-zero one leaves the remainder in the final piece.
fn split_into_parts(source: &str, delimiter: &str, limit: usize) -> Vec<Variant> {
    if source.is_empty() {
        return Vec::new();
    }
    if delimiter.is_empty() {
        return vec![Variant::String(source.to_string())];
    }
    let pieces: Vec<&str> = if limit == 0 {
        source.split(delimiter).collect()
    } else {
        source.splitn(limit, delimiter).collect()
    };
    pieces
        .into_iter()
        .map(|piece| Variant::String(piece.to_string()))
        .collect()
}

/// VB6 rounds half to even.
fn round_half_even(x: f64) -> f64 {
    let floor = x.floor();
    if (x - floor - 0.5).abs() < f64::EPSILON {
        if (floor as i64) % 2 == 0 {
            floor
        } else {
            floor + 1.0
        }
    } else {
        (x - floor).round() + floor
    }
}

/// `Val` reads the longest numeric prefix and stops at the first junk character.
fn parse_numeric_prefix(text: &str) -> Variant {
    let chars: Vec<char> = text.trim_start().chars().collect();
    let mut i = 0usize;
    let mut literal = String::new();
    let mut seen_digit = false;
    let mut is_float = false;

    if i < chars.len() && (chars[i] == '+' || chars[i] == '-') {
        literal.push(chars[i]);
        i += 1;
    }
    while i < chars.len() && chars[i].is_ascii_digit() {
        literal.push(chars[i]);
        i += 1;
        seen_digit = true;
    }
    if i < chars.len() && chars[i] == '.' {
        is_float = true;
        literal.push(chars[i]);
        i += 1;
        while i < chars.len() && chars[i].is_ascii_digit() {
            literal.push(chars[i]);
            i += 1;
            seen_digit = true;
        }
    }
    if seen_digit && i < chars.len() && (chars[i] == 'e' || chars[i] == 'E') {
        let mut j = i + 1;
        if j < chars.len() && (chars[j] == '+' || chars[j] == '-') {
            j += 1;
        }
        if j < chars.len() && chars[j].is_ascii_digit() {
            is_float = true;
            while j < chars.len() && chars[j].is_ascii_digit() {
                j += 1;
            }
            while i < j {
                literal.push(chars[i]);
                i += 1;
            }
        }
    }

    if !seen_digit {
        return Variant::Int(0);
    }
    if is_float {
        Variant::Float(literal.parse().unwrap_or(0.0))
    } else {
        match literal.parse::<i64>() {
            Ok(value) => Variant::Int(value),
            Err(_) => Variant::Float(literal.parse().unwrap_or(0.0)),
        }
    }
}

/// The current local wall-clock date and time.
fn current_date_time() -> (String, String) {
    let now = crate::datetime::now();
    (now.date(), now.time())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn val_reads_a_numeric_prefix() {
        assert_eq!(parse_numeric_prefix("12abc").to_int().unwrap(), 12);
        assert_eq!(parse_numeric_prefix("  -3.5xyz").to_float().unwrap(), -3.5);
        assert_eq!(parse_numeric_prefix("1e3").to_float().unwrap(), 1000.0);
        assert_eq!(parse_numeric_prefix("abc").to_int().unwrap(), 0);
    }

    #[test]
    fn vb_rounding_is_half_to_even() {
        assert_eq!(round_half_even(0.5), 0.0);
        assert_eq!(round_half_even(1.5), 2.0);
        assert_eq!(round_half_even(2.5), 2.0);
        assert_eq!(round_half_even(-2.5), -2.0);
        assert_eq!(round_half_even(2.6), 3.0);
    }

    #[test]
    fn int_rounds_down_while_fix_truncates() {
        // The classic VB6 pair: Int(-2.5) = -3 but Fix(-2.5) = -2.
        assert_eq!((-2.5f64).floor(), -3.0);
        assert_eq!((-2.5f64).trunc(), -2.0);
    }

    #[test]
    fn split_splits_the_way_vb6_did() {
        let parts = split_into_parts("a,b,c", ",", 0);
        assert_eq!(parts.len(), 3);
        assert_eq!(parts[0].display_string(), "a");
        assert_eq!(parts[2].display_string(), "c");

        // An empty source is an empty array, not one empty element.
        assert!(split_into_parts("", ",", 0).is_empty());
        // A gap between delimiters keeps its empty field.
        assert_eq!(split_into_parts("a,,b", ",", 0).len(), 3);
        // A limit leaves the rest of the text in the final piece.
        let limited = split_into_parts("a,b,c", ",", 2);
        assert_eq!(limited.len(), 2);
        assert_eq!(limited[1].display_string(), "b,c");
        // An empty delimiter cannot split anything.
        assert_eq!(split_into_parts("abc", "", 0).len(), 1);
    }
}
