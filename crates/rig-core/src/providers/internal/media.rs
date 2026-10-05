//! Raw attachment encoding for provider request conversion.

use base64::{Engine, prelude::BASE64_STANDARD};

use crate::message::{DocumentSourceKind, MessageError};

pub(crate) fn encode_raw_source(source: DocumentSourceKind) -> DocumentSourceKind {
    match source {
        DocumentSourceKind::Raw(bytes) => DocumentSourceKind::Base64(BASE64_STANDARD.encode(bytes)),
        source => source,
    }
}

pub(crate) fn decode_raw_text(bytes: Vec<u8>) -> Result<String, MessageError> {
    String::from_utf8(bytes).map_err(|error| {
        MessageError::ConversionError(format!("Invalid UTF-8 in document: {}", error.utf8_error()))
    })
}

#[cfg(test)]
mod tests;
