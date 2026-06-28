/// Raw byte payload for non-UTF-8 or NUL-containing files.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BinaryBlob {
    pub bytes: Vec<u8>,
}

impl BinaryBlob {
    pub fn new(bytes: Vec<u8>) -> Self {
        Self { bytes }
    }

    pub fn from_path(path: &std::path::Path) -> Result<Self, String> {
        std::fs::read(path)
            .map(Self::new)
            .map_err(|e| e.to_string())
    }
}

/// Classify on-disk bytes as binary or UTF-8 text suitable for AST/text parsing.
pub fn is_binary_payload(bytes: &[u8]) -> bool {
    bytes.contains(&0) || std::str::from_utf8(bytes).is_err()
}

/// Load working-tree file content: binary blobs are stored verbatim; UTF-8 paths
/// follow the existing AST-or-text parse path.
pub fn load_working_content(path: &str, bytes: Vec<u8>) -> crate::frontend::FileContent {
    if is_binary_payload(&bytes) {
        crate::trace::notice(format!("{path}: stored as binary blob"));
        return crate::frontend::FileContent::Binary(BinaryBlob::new(bytes));
    }
    let text = String::from_utf8(bytes).expect("validated UTF-8 above");
    crate::frontend::parse_text_or_blob(path, &text)
}
