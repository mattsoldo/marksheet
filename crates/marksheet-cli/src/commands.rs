//! Command implementations.
//!
//! This module owns filesystem effects and exit-status policy. Formatting is
//! deliberately parse-first so malformed input is never overwritten.

use std::{
    fmt, fs,
    io::{self, Write},
    path::{Path, PathBuf},
    process::ExitCode,
    sync::atomic::{AtomicU64, Ordering},
};

use marksheet_model::Severity;

use crate::OutputFormat;

/// Runs `marksheet check`.
pub(crate) fn check(path: &Path, format: OutputFormat) -> Result<ExitCode, CliError> {
    let source = read_source(path)?;
    let document = marksheet_syntax::parse(&source);
    crate::render::render(path, document.source_bytes(), &document.diagnostics, format)
        .map_err(CliError::Render)?;

    Ok(exit_for_diagnostics(&document.diagnostics))
}

/// Runs `marksheet fmt`.
pub(crate) fn format(path: &Path, check: bool) -> Result<ExitCode, CliError> {
    reject_symlink(path)?;
    let source = read_source(path)?;
    let document = marksheet_syntax::parse(&source);
    if document.has_errors() {
        crate::render::render_human(path, document.source_bytes(), &document.diagnostics)
            .map_err(CliError::Render)?;
        return Ok(ExitCode::from(1));
    }

    let formatted = match marksheet_syntax::canonicalize(&document) {
        Ok(formatted) => formatted,
        Err(diagnostics) => {
            // This should normally be unreachable after `has_errors`, but the
            // formatter is allowed to reject a document if a future canonical
            // invariant cannot be represented safely.
            crate::render::render_human(path, document.source_bytes(), &diagnostics)
                .map_err(CliError::Render)?;
            return Ok(ExitCode::from(1));
        }
    };

    if check {
        if source == formatted {
            return Ok(ExitCode::SUCCESS);
        }
        crate::render::print_stderr(&format!("{} is not canonically formatted", path.display()))
            .map_err(CliError::Render)?;
        return Ok(ExitCode::from(1));
    }

    if source != formatted {
        replace_atomically(path, &formatted)?;
    }
    Ok(ExitCode::SUCCESS)
}

fn read_source(path: &Path) -> Result<Vec<u8>, CliError> {
    fs::read(path).map_err(|source| CliError::Read {
        path: path.to_owned(),
        source,
    })
}

/// Formatting replaces a directory entry, so following a symlink here would
/// replace the link itself rather than update its target. Refuse early to make
/// that surprising and potentially destructive behavior impossible.
fn reject_symlink(path: &Path) -> Result<(), CliError> {
    let metadata = fs::symlink_metadata(path).map_err(|source| CliError::Read {
        path: path.to_owned(),
        source,
    })?;
    if metadata.file_type().is_symlink() {
        return Err(CliError::SymbolicLink(path.to_owned()));
    }
    Ok(())
}

fn exit_for_diagnostics(diagnostics: &[marksheet_model::Diagnostic]) -> ExitCode {
    if diagnostics
        .iter()
        .any(|diagnostic| diagnostic.severity == Severity::Error)
    {
        ExitCode::from(1)
    } else {
        ExitCode::SUCCESS
    }
}

/// Replaces `path` using a sibling temporary file. On filesystems where rename
/// is atomic within a directory, observers see either the old complete source
/// or the new complete source, never a partially written workbook.
fn replace_atomically(path: &Path, contents: &[u8]) -> Result<(), CliError> {
    let parent = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    let file_name = path
        .file_name()
        .ok_or_else(|| CliError::InvalidOutputPath(path.to_owned()))?;
    let existing_permissions = fs::metadata(path)
        .ok()
        .map(|metadata| metadata.permissions());
    let (temporary, mut file) = create_temporary_file(parent, file_name)?;

    let write_result = (|| -> io::Result<()> {
        if let Some(permissions) = existing_permissions {
            file.set_permissions(permissions)?;
        }
        file.write_all(contents)?;
        file.sync_all()?;
        Ok(())
    })();
    drop(file);

    if let Err(source) = write_result {
        let _ = fs::remove_file(&temporary);
        return Err(CliError::Write {
            path: temporary,
            source,
        });
    }

    if let Err(source) = fs::rename(&temporary, path) {
        let _ = fs::remove_file(&temporary);
        return Err(CliError::Write {
            path: path.to_owned(),
            source,
        });
    }
    Ok(())
}

fn create_temporary_file(
    parent: &Path,
    file_name: &std::ffi::OsStr,
) -> Result<(PathBuf, fs::File), CliError> {
    static NEXT_TEMPORARY: AtomicU64 = AtomicU64::new(0);
    let file_name = file_name.to_string_lossy();
    for _ in 0..128 {
        let sequence = NEXT_TEMPORARY.fetch_add(1, Ordering::Relaxed);
        let candidate = parent.join(format!(
            ".{file_name}.marksheet-{}-{sequence}.tmp",
            std::process::id()
        ));
        match fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&candidate)
        {
            Ok(file) => return Ok((candidate, file)),
            Err(source) if source.kind() == io::ErrorKind::AlreadyExists => {}
            Err(source) => {
                return Err(CliError::Write {
                    path: candidate,
                    source,
                });
            }
        }
    }
    Err(CliError::TemporaryPath(parent.to_owned()))
}

#[derive(Debug)]
pub(crate) enum CliError {
    Read {
        path: std::path::PathBuf,
        source: io::Error,
    },
    Render(io::Error),
    Write {
        path: PathBuf,
        source: io::Error,
    },
    InvalidOutputPath(PathBuf),
    TemporaryPath(PathBuf),
    SymbolicLink(PathBuf),
}

impl fmt::Display for CliError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Read { path, source } => {
                write!(formatter, "could not read {}: {source}", path.display())
            }
            Self::Render(source) => write!(formatter, "could not write diagnostics: {source}"),
            Self::Write { path, source } => {
                write!(formatter, "could not write {}: {source}", path.display())
            }
            Self::InvalidOutputPath(path) => {
                write!(
                    formatter,
                    "{} does not name a workbook file",
                    path.display()
                )
            }
            Self::TemporaryPath(path) => write!(
                formatter,
                "could not allocate a temporary formatting file in {}",
                path.display()
            ),
            Self::SymbolicLink(path) => write!(
                formatter,
                "refusing to format symbolic link {}; format the target directly",
                path.display()
            ),
        }
    }
}

impl std::error::Error for CliError {}
