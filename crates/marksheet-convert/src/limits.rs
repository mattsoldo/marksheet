//! Resource budgets applied before allocation or expansion whenever possible.

use crate::{ConvertError, ConvertErrorCode};

/// Limits for untrusted CSV and XLSX input and for generated output.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ConversionLimits {
    pub max_input_bytes: u64,
    pub max_output_bytes: u64,
    pub max_zip_entries: usize,
    pub max_zip_entry_uncompressed_bytes: u64,
    pub max_zip_total_uncompressed_bytes: u64,
    /// Maximum accepted uncompressed-to-compressed ratio for one ZIP entry.
    pub max_zip_compression_ratio: u64,
    /// Maximum XML events accepted in any one part.
    pub max_xml_events: usize,
    pub max_xml_depth: usize,
    /// Maximum attributes accepted on one XML element.
    pub max_xml_attributes: usize,
    /// Maximum decoded bytes accepted in one XML attribute value.
    pub max_xml_attribute_bytes: usize,
    pub max_relationships: usize,
    pub max_sheets: usize,
    pub max_tables: usize,
    pub max_styles: usize,
    pub max_cells: u64,
    /// Maximum formula-bearing cells and calculated columns processed.
    pub max_formulas: u64,
    /// Maximum entries accepted in an OOXML shared string table.
    pub max_shared_strings: usize,
    pub max_string_bytes: usize,
}

impl Default for ConversionLimits {
    fn default() -> Self {
        Self {
            max_input_bytes: 64 * 1024 * 1024,
            max_output_bytes: 64 * 1024 * 1024,
            max_zip_entries: 4_096,
            max_zip_entry_uncompressed_bytes: 32 * 1024 * 1024,
            max_zip_total_uncompressed_bytes: 128 * 1024 * 1024,
            max_zip_compression_ratio: 200,
            max_xml_events: 2_000_000,
            max_xml_depth: 128,
            max_xml_attributes: 64,
            max_xml_attribute_bytes: 8 * 1024,
            max_relationships: 16_384,
            max_sheets: 1_024,
            max_tables: 4_096,
            max_styles: 16_384,
            max_cells: 2_000_000,
            max_formulas: 1_000_000,
            max_shared_strings: 250_000,
            max_string_bytes: 1_048_576,
        }
    }
}

impl ConversionLimits {
    pub(crate) fn check_input(self, bytes: usize) -> Result<(), ConvertError> {
        if u64::try_from(bytes).unwrap_or(u64::MAX) > self.max_input_bytes {
            return Err(ConvertError::new(
                ConvertErrorCode::ResourceLimit,
                format!(
                    "input is {bytes} bytes; limit is {} bytes",
                    self.max_input_bytes
                ),
            ));
        }
        Ok(())
    }
}
