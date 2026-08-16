//! Command-line interface for validating and canonically formatting Marksheet
//! workbooks.
//!
//! The command layer deliberately stays thin: parsing and serialization remain
//! in `marksheet-syntax`, so native clients and future bindings observe the
//! same behavior.

mod commands;
mod render;

use std::{path::PathBuf, process::ExitCode};

use clap::{Parser, Subcommand, ValueEnum};

#[derive(Debug, Parser)]
#[command(
    name = "marksheet",
    version,
    about = "Validate and format Marksheet workbooks"
)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Debug, Subcommand)]
enum Command {
    /// Validate Marksheet source and report all recoverable diagnostics.
    Check {
        /// Diagnostic output format.
        #[arg(long, value_enum, default_value_t = OutputFormat::Human)]
        format: OutputFormat,
        /// Marksheet workbook to validate.
        path: PathBuf,
    },
    /// Explicitly rewrite a workbook using canonical Marksheet formatting.
    Fmt {
        /// Exit nonzero when the workbook is not canonically formatted.
        #[arg(long)]
        check: bool,
        /// Marksheet workbook to format.
        path: PathBuf,
    },
    /// Calculate an explicit rectangular selection from a workbook.
    Calc {
        /// Sheet identifier containing the selection.
        #[arg(long)]
        sheet: marksheet_model::SheetId,
        /// Inclusive A1 selection to calculate and print.
        #[arg(long)]
        range: marksheet_model::Range,
        /// Calculated-value output format.
        #[arg(long, value_enum, default_value_t = CalcOutputFormat::Json)]
        format: CalcOutputFormat,
        /// Marksheet workbook to calculate.
        path: PathBuf,
    },
    /// Compare two workbooks by their semantic projection, not source spelling.
    ///
    /// A successful empty comparison exits 0. Differences and invalid inputs
    /// exit 1, so the command is suitable for use in CI and shell guards.
    Diff {
        /// Difference output format.
        #[arg(long, value_enum, default_value_t = DiffOutputFormat::Human)]
        format: DiffOutputFormat,
        /// Baseline Marksheet workbook.
        old: PathBuf,
        /// Candidate Marksheet workbook.
        new: PathBuf,
    },
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, ValueEnum)]
enum OutputFormat {
    #[default]
    Human,
    Json,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, ValueEnum)]
enum CalcOutputFormat {
    Csv,
    #[default]
    Json,
    Text,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, ValueEnum)]
enum DiffOutputFormat {
    #[default]
    Human,
    Json,
}

fn main() -> ExitCode {
    let cli = Cli::parse();
    let result = match cli.command {
        Command::Check { format, path } => commands::check(&path, format),
        Command::Fmt { check, path } => commands::format(&path, check),
        Command::Calc {
            sheet,
            range,
            format,
            path,
        } => commands::calculate(&path, sheet, range, format),
        Command::Diff { format, old, new } => commands::diff(&old, &new, format),
    };

    match result {
        Ok(code) => code,
        Err(error) => {
            eprintln!("marksheet: {error}");
            ExitCode::from(2)
        }
    }
}
