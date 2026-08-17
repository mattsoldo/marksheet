//! Command-line interface for inspecting, validating, formatting, calculating,
//! editing, comparing, and converting Marksheet workbooks.
//!
//! The command layer deliberately stays thin: parsing and serialization remain
//! in `marksheet-syntax`, so native clients and future bindings observe the
//! same behavior.

mod automation;
mod commands;
mod render;

use std::{path::PathBuf, process::ExitCode};

use clap::{Parser, Subcommand, ValueEnum};

#[derive(Debug, Parser)]
#[command(
    name = "marksheet",
    version,
    about = "Inspect, validate, calculate, edit, compare, and convert Marksheet workbooks"
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
    /// Inspect workbook structure, names, tables, extensions, and diagnostics.
    Inspect {
        /// Marksheet workbook to inspect.
        path: PathBuf,
    },
    /// Read an explicit sheet-qualified range, workbook name, or table.
    Get {
        /// Marksheet workbook to query.
        path: PathBuf,
        /// Stable name/table ID or a target such as `inputs!A1:B4`.
        target: String,
        /// Include calculated values alongside authored source values.
        #[arg(long, default_value_t = true, action = clap::ArgAction::Set)]
        calculated: bool,
    },
    /// Apply one source-aware cell or single-cell-name edit atomically.
    Set {
        /// Marksheet workbook to edit.
        path: PathBuf,
        /// Stable single-cell name or target such as `inputs!G2`.
        target: String,
        /// Strict scalar spelling or formula beginning with `=`.
        #[arg(allow_hyphen_values = true)]
        value_or_formula: String,
    },
    /// Append one source-aware table data row atomically.
    AppendTableRow {
        /// Marksheet workbook to edit.
        path: PathBuf,
        /// Stable table identifier.
        table: String,
        /// One strict scalar spelling per table column, in header order.
        #[arg(long = "value", allow_hyphen_values = true)]
        values: Vec<String>,
    },
    /// Explicitly rewrite a workbook using canonical Marksheet formatting.
    Fmt {
        /// Exit nonzero when the workbook is not canonically formatted.
        #[arg(long)]
        check: bool,
        /// Formatting result output format.
        #[arg(long, value_enum, default_value_t = OutputFormat::Human)]
        format: OutputFormat,
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
    /// Convert between Marksheet, XLSX, and explicitly selected CSV.
    ///
    /// The destination artifact is written atomically. A
    /// `marksheet-conversion@1` report is emitted as JSON on standard output.
    Convert {
        /// Destination format.
        #[arg(long, value_enum)]
        to: ConversionTarget,
        /// Destination artifact (defaults to a sibling with the target extension).
        #[arg(long)]
        output: Option<PathBuf>,
        /// Source sheet for CSV export, or target sheet identifier for CSV import.
        #[arg(long)]
        sheet: Option<marksheet_model::SheetId>,
        /// Target sheet label for CSV import.
        #[arg(long)]
        label: Option<String>,
        /// Explicit source or target rectangle for CSV conversion.
        #[arg(long)]
        range: Option<marksheet_model::Range>,
        /// Source table for CSV export, or target table identifier for CSV import.
        #[arg(long)]
        table: Option<marksheet_model::TableId>,
        /// Target table anchor for CSV import.
        #[arg(long)]
        anchor: Option<marksheet_model::Coordinate>,
        /// Source workbook or CSV file.
        path: PathBuf,
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

#[derive(Clone, Copy, Debug, Eq, PartialEq, ValueEnum)]
enum ConversionTarget {
    Marksheet,
    Xlsx,
    Csv,
}

fn main() -> ExitCode {
    let cli = Cli::parse();
    let result = match cli.command {
        Command::Check { format, path } => commands::check(&path, format),
        Command::Inspect { path } => automation::inspect(&path),
        Command::Get {
            path,
            target,
            calculated,
        } => automation::get(&path, &target, calculated),
        Command::Set {
            path,
            target,
            value_or_formula,
        } => automation::set(&path, &target, &value_or_formula),
        Command::AppendTableRow {
            path,
            table,
            values,
        } => automation::append_table_row(&path, &table, &values),
        Command::Fmt {
            check,
            format,
            path,
        } => match format {
            OutputFormat::Human => commands::format(&path, check),
            OutputFormat::Json => automation::format(&path, check),
        },
        Command::Calc {
            sheet,
            range,
            format,
            path,
        } => commands::calculate(&path, sheet, range, format),
        Command::Diff { format, old, new } => commands::diff(&old, &new, format),
        Command::Convert {
            to,
            output,
            sheet,
            label,
            range,
            table,
            anchor,
            path,
        } => commands::convert(
            &path,
            &commands::ConvertOptions {
                target: to,
                output: output.as_deref(),
                sheet,
                label,
                range,
                table,
                anchor,
            },
        ),
    };

    match result {
        Ok(code) => code,
        Err(error) => {
            eprintln!("marksheet: {error}");
            ExitCode::from(2)
        }
    }
}
