//! `Scripting.FileSystemObject`, plus the `TextStream` it hands out.
//!
//! The era's most-used object after the dictionary, and the one with the best
//! reason to exist here: doing this work natively avoids interpolating paths
//! into a shell command, which is how a quoting slip becomes an injection bug.

use std::cell::RefCell;
use std::fs::{self, File, OpenOptions};
use std::io::{Read, Write};
use std::path::Path;
use std::rc::Rc;

use crate::datetime::format_timestamp;
use crate::error::BrtError;
use crate::interp::read_line;
use crate::objects::{BartObject, Member};
use crate::value::Variant;

const FOR_READING: i64 = 1;
const FOR_WRITING: i64 = 2;
const FOR_APPENDING: i64 = 8;

/// `CreateObject("Scripting.FileSystemObject")`.
pub struct FileSystemObject;

impl Default for FileSystemObject {
    fn default() -> Self {
        Self::new()
    }
}

impl FileSystemObject {
    /// A handle on the filesystem.  It holds no state; every call goes straight
    /// to the operating system.
    pub fn new() -> Self {
        FileSystemObject
    }
}

fn argument(method: &str, args: &[Variant], index: usize) -> Result<String, BrtError> {
    match args.get(index) {
        Some(value) => value.as_string(),
        None => Err(BrtError::wrong_arg_count(
            method,
            args.len(),
            &format!("at least {}", index + 1),
        )),
    }
}

fn require_dir(method: &str, path: &str) -> Result<(), BrtError> {
    if Path::new(path).is_dir() {
        Ok(())
    } else {
        Err(BrtError::new(76, format!("{method}: path not found: {path}")))
    }
}

/// A temporary file name.  Like VB6's `GetTempName`, it is a name only - the
/// file is not created.
fn temp_name() -> String {
    let stamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|elapsed| elapsed.subsec_nanos())
        .unwrap_or(0);
    std::env::temp_dir()
        .join(format!("bartelang-{}-{stamp}.tmp", std::process::id()))
        .to_string_lossy()
        .into_owned()
}

impl BartObject for FileSystemObject {
    fn type_name(&self) -> String {
        "Scripting.FileSystemObject".to_string()
    }

    fn member_kind(&self, name: &str) -> Member {
        match name.to_ascii_lowercase().as_str() {
            "fileexists" | "folderexists" | "createfolder" | "deletefile" | "deletefolder"
            | "copyfile" | "movefile" | "getbasename" | "getextensionname"
            | "getparentfoldername" | "buildpath" | "gettempname" | "getabsolutepathname"
            | "filesize" | "filedatetime" | "opentextfile" | "createtextfile" => Member::Method,
            _ => Member::Unknown,
        }
    }

    fn call_method(&mut self, method: &str, args: Vec<Variant>) -> Result<Variant, BrtError> {
        match method.to_ascii_lowercase().as_str() {
            "fileexists" => {
                let path = argument("FileExists", &args, 0)?;
                Ok(Variant::Bool(Path::new(&path).is_file()))
            }
            "folderexists" => {
                let path = argument("FolderExists", &args, 0)?;
                Ok(Variant::Bool(Path::new(&path).is_dir()))
            }
            "createfolder" => {
                let path = argument("CreateFolder", &args, 0)?;
                // `mkdir -p` semantics: missing parents are created as well.
                fs::create_dir_all(&path).map_err(|e| {
                    BrtError::new(5, format!("CreateFolder {path} failed: {e}"))
                })?;
                Ok(Variant::Empty)
            }
            "deletefile" => {
                let path = argument("DeleteFile", &args, 0)?;
                if !Path::new(&path).is_file() {
                    return Err(BrtError::path_not_found(&path));
                }
                fs::remove_file(&path)
                    .map_err(|e| BrtError::new(5, format!("DeleteFile {path} failed: {e}")))?;
                Ok(Variant::Empty)
            }
            "deletefolder" => {
                let path = argument("DeleteFolder", &args, 0)?;
                // Deleting a tree is destructive, so it takes an explicit flag
                // rather than being the default.
                let recursive = args.get(1).is_some_and(|flag| flag.truthy());
                require_dir("DeleteFolder", &path)?;
                if recursive {
                    fs::remove_dir_all(&path).map_err(|e| {
                        BrtError::new(5, format!("DeleteFolder {path} failed: {e}"))
                    })?;
                } else {
                    fs::remove_dir(&path).map_err(|e| {
                        BrtError::new(
                            5,
                            format!(
                                "DeleteFolder {path} failed (it must be empty; \
                                 pass True as the second argument to delete \
                                 it recursively): {e}"
                            ),
                        )
                    })?;
                }
                Ok(Variant::Empty)
            }
            "copyfile" => {
                let from = argument("CopyFile", &args, 0)?;
                let to = argument("CopyFile", &args, 1)?;
                fs::copy(&from, &to).map_err(|e| {
                    BrtError::new(53, format!("CopyFile {from} -> {to} failed: {e}"))
                })?;
                Ok(Variant::Empty)
            }
            "movefile" => {
                let from = argument("MoveFile", &args, 0)?;
                let to = argument("MoveFile", &args, 1)?;
                fs::rename(&from, &to).map_err(|e| {
                    BrtError::new(53, format!("MoveFile {from} -> {to} failed: {e}"))
                })?;
                Ok(Variant::Empty)
            }
            "getbasename" => {
                let path = argument("GetBaseName", &args, 0)?;
                Ok(Variant::String(
                    Path::new(&path)
                        .file_stem()
                        .map(|stem| stem.to_string_lossy().into_owned())
                        .unwrap_or_default(),
                ))
            }
            "getextensionname" => {
                let path = argument("GetExtensionName", &args, 0)?;
                Ok(Variant::String(
                    Path::new(&path)
                        .extension()
                        .map(|extension| extension.to_string_lossy().into_owned())
                        .unwrap_or_default(),
                ))
            }
            "getparentfoldername" => {
                let path = argument("GetParentFolderName", &args, 0)?;
                Ok(Variant::String(
                    Path::new(&path)
                        .parent()
                        .map(|parent| parent.to_string_lossy().into_owned())
                        .unwrap_or_default(),
                ))
            }
            "buildpath" => {
                let base = argument("BuildPath", &args, 0)?;
                let name = argument("BuildPath", &args, 1)?;
                Ok(Variant::String(
                    Path::new(&base)
                        .join(name)
                        .to_string_lossy()
                        .into_owned(),
                ))
            }
            "gettempname" => Ok(Variant::String(temp_name())),
            "getabsolutepathname" => {
                let path = argument("GetAbsolutePathName", &args, 0)?;
                // Resolved against the working directory, and it need not exist.
                let absolute = std::path::absolute(&path).unwrap_or_else(|_| path.clone().into());
                Ok(Variant::String(absolute.to_string_lossy().into_owned()))
            }
            "filesize" => {
                let path = argument("FileSize", &args, 0)?;
                let metadata =
                    fs::metadata(&path).map_err(|_| BrtError::path_not_found(&path))?;
                Ok(Variant::Int(metadata.len() as i64))
            }
            "filedatetime" => {
                let path = argument("FileDateTime", &args, 0)?;
                let metadata =
                    fs::metadata(&path).map_err(|_| BrtError::path_not_found(&path))?;
                let modified = metadata
                    .modified()
                    .map_err(|e| BrtError::new(5, format!("FileDateTime {path} failed: {e}")))?;
                Ok(Variant::String(format_timestamp(modified)))
            }
            "opentextfile" => open_text_file(&args),
            "createtextfile" => create_text_file(&args),
            other => Err(BrtError::no_such_member(&self.type_name(), other)),
        }
    }

    fn get_property(&self, prop_name: &str) -> Result<Variant, BrtError> {
        Err(BrtError::no_such_member(&self.type_name(), prop_name))
    }
}

fn stream(file: File, writable: bool) -> Variant {
    Variant::Object(Rc::new(RefCell::new(TextStream::new(file, writable))))
}

/// `OpenTextFile(path, [mode], [create], [format])`
fn open_text_file(args: &[Variant]) -> Result<Variant, BrtError> {
    let path = argument("OpenTextFile", args, 0)?;
    let mode = match args.get(1) {
        Some(value) => value.to_int()?,
        None => FOR_READING,
    };
    let create = args.get(2).is_some_and(|flag| flag.truthy());
    let file = match mode {
        FOR_WRITING => OpenOptions::new()
            .write(true)
            .create(create)
            .truncate(true)
            .open(&path),
        FOR_APPENDING => OpenOptions::new().append(true).create(create).open(&path),
        FOR_READING => File::open(&path),
        other => {
            return Err(BrtError::invalid_call(format!(
                "OpenTextFile: unknown mode {other} (1 = read, 2 = write, 8 = append)"
            )))
        }
    }
    .map_err(|e| BrtError::new(53, format!("OpenTextFile {path} failed: {e}")))?;
    Ok(stream(file, mode != FOR_READING))
}

/// `CreateTextFile(path, [overwrite])`
fn create_text_file(args: &[Variant]) -> Result<Variant, BrtError> {
    let path = argument("CreateTextFile", args, 0)?;
    let overwrite = args.get(1).map(|flag| flag.truthy()).unwrap_or(true);
    if !overwrite && Path::new(&path).exists() {
        return Err(BrtError::new(58, format!("File already exists: {path}")));
    }
    let file = OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .open(&path)
        .map_err(|e| BrtError::new(53, format!("CreateTextFile {path} failed: {e}")))?;
    Ok(stream(file, true))
}

/// The `TextStream` behind `OpenTextFile` and `CreateTextFile`.
pub struct TextStream {
    file: File,
    writable: bool,
    /// Length seen at open time, so `AtEndOfStream` can answer from `&self`.
    length: u64,
    position: u64,
    line: i64,
    closed: bool,
}

impl TextStream {
    /// Wraps an open file.  `writable` decides whether it reads or writes, and
    /// the length seen now is what `AtEndOfStream` reports against.
    pub fn new(file: File, writable: bool) -> Self {
        let length = file
            .metadata()
            .map(|metadata| metadata.len())
            .unwrap_or(0);
        TextStream {
            file,
            writable,
            length,
            position: if writable { length } else { 0 },
            line: 0,
            closed: false,
        }
    }

    fn ensure_open(&self) -> Result<(), BrtError> {
        if self.closed {
            return Err(BrtError::invalid_call("This TextStream has been closed"));
        }
        Ok(())
    }

    fn ensure_readable(&self) -> Result<(), BrtError> {
        self.ensure_open()?;
        if self.writable {
            return Err(BrtError::invalid_call(
                "This TextStream was opened for writing, not reading",
            ));
        }
        Ok(())
    }

    fn ensure_writable(&self) -> Result<(), BrtError> {
        self.ensure_open()?;
        if !self.writable {
            return Err(BrtError::invalid_call(
                "This TextStream was opened for reading, not writing",
            ));
        }
        Ok(())
    }

    fn at_end(&self) -> bool {
        !self.writable && self.position >= self.length
    }
}

impl BartObject for TextStream {
    fn type_name(&self) -> String {
        "TextStream".to_string()
    }

    fn member_kind(&self, name: &str) -> Member {
        match name.to_ascii_lowercase().as_str() {
            "readall" | "readline" | "write" | "writeline" | "close" => Member::Method,
            "atendofstream" | "line" => Member::Property,
            _ => Member::Unknown,
        }
    }

    fn call_method(&mut self, method: &str, args: Vec<Variant>) -> Result<Variant, BrtError> {
        match method.to_ascii_lowercase().as_str() {
            "readall" => {
                self.ensure_readable()?;
                let mut text = String::new();
                self.file
                    .read_to_string(&mut text)
                    .map_err(|e| BrtError::new(5, format!("ReadAll failed: {e}")))?;
                self.position = self.length;
                self.line += text.lines().count() as i64;
                Ok(Variant::String(text))
            }
            "readline" => {
                self.ensure_readable()?;
                match read_line(&mut self.file) {
                    Ok(Some(line)) => {
                        self.position += line.len() as u64 + 1;
                        self.line += 1;
                        Ok(Variant::String(line))
                    }
                    Ok(None) => Ok(Variant::Empty),
                    Err(e) => Err(BrtError::new(5, format!("ReadLine failed: {e}"))),
                }
            }
            "write" => {
                self.ensure_writable()?;
                let text = argument("Write", &args, 0)?;
                self.file
                    .write_all(text.as_bytes())
                    .map_err(|e| BrtError::new(5, format!("Write failed: {e}")))?;
                self.position += text.len() as u64;
                Ok(Variant::Empty)
            }
            "writeline" => {
                self.ensure_writable()?;
                let text = match args.first() {
                    Some(value) => value.as_string()?,
                    None => String::new(),
                };
                let mut line = text;
                line.push('\n');
                self.file
                    .write_all(line.as_bytes())
                    .map_err(|e| BrtError::new(5, format!("WriteLine failed: {e}")))?;
                self.position += line.len() as u64;
                self.line += 1;
                Ok(Variant::Empty)
            }
            "close" => {
                self.ensure_open()?;
                self.file
                    .flush()
                    .map_err(|e| BrtError::new(5, format!("Close failed: {e}")))?;
                self.closed = true;
                Ok(Variant::Empty)
            }
            other => Err(BrtError::no_such_member(&self.type_name(), other)),
        }
    }

    fn get_property(&self, prop_name: &str) -> Result<Variant, BrtError> {
        match prop_name.to_ascii_lowercase().as_str() {
            "atendofstream" => Ok(Variant::Bool(self.at_end())),
            "line" => Ok(Variant::Int(self.line)),
            other => Err(BrtError::no_such_member(&self.type_name(), other)),
        }
    }
}
