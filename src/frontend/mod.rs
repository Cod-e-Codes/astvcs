mod binaryblob;
mod languages;
mod textblob;
mod treesitter;

pub use binaryblob::{BinaryBlob, is_binary_payload, load_working_content};
pub use languages::{
    SourceLanguage, is_text_only_path, supported_extensions, supported_special_paths,
};
pub use textblob::{TextBlob, parse_text_or_blob};
pub use treesitter::{parse_language, parse_rust, parse_source};

use crate::graph::AstGraph;
use base64::{Engine as _, engine::general_purpose::STANDARD};

/// Parsed representation of a source file.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum FileContent {
    Ast(AstGraph),
    Text(TextBlob),
    Binary(BinaryBlob),
}

impl FileContent {
    pub fn is_ast(&self) -> bool {
        matches!(self, Self::Ast(_))
    }

    pub fn is_binary(&self) -> bool {
        matches!(self, Self::Binary(_))
    }

    pub fn display_kind(&self) -> &'static str {
        match self {
            Self::Ast(_) => "ast",
            Self::Text(_) => "text",
            Self::Binary(_) => "binary",
        }
    }

    pub fn semantic_eq(&self, other: &Self) -> bool {
        match (self, other) {
            (Self::Ast(a), Self::Ast(b)) => a.to_snapshot() == b.to_snapshot(),
            (Self::Text(a), Self::Text(b)) => a.content == b.content,
            (Self::Binary(a), Self::Binary(b)) => a.bytes == b.bytes,
            _ => false,
        }
    }
}

impl serde::Serialize for FileContent {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        use serde::ser::SerializeStruct;
        match self {
            Self::Ast(graph) => {
                let mut s = serializer.serialize_struct("FileContent", 2)?;
                s.serialize_field("kind", "ast")?;
                s.serialize_field("graph", &graph.to_snapshot())?;
                s.end()
            }
            Self::Text(blob) => {
                let mut s = serializer.serialize_struct("FileContent", 2)?;
                s.serialize_field("kind", "text")?;
                s.serialize_field("content", &blob.content)?;
                s.end()
            }
            Self::Binary(blob) => {
                let mut s = serializer.serialize_struct("FileContent", 2)?;
                s.serialize_field("kind", "binary")?;
                s.serialize_field("bytes", &STANDARD.encode(&blob.bytes))?;
                s.end()
            }
        }
    }
}

impl<'de> serde::Deserialize<'de> for FileContent {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        #[derive(serde::Deserialize)]
        struct Raw {
            kind: String,
            graph: Option<crate::graph::AstGraphSnapshot>,
            content: Option<String>,
            bytes: Option<String>,
        }
        let raw = Raw::deserialize(deserializer)?;
        match raw.kind.as_str() {
            "ast" => {
                let graph = raw
                    .graph
                    .ok_or_else(|| serde::de::Error::missing_field("graph"))?;
                Ok(Self::Ast(AstGraph::from_snapshot(graph)))
            }
            "text" => {
                let content = raw
                    .content
                    .ok_or_else(|| serde::de::Error::missing_field("content"))?;
                Ok(Self::Text(TextBlob::new(content)))
            }
            "binary" => {
                let encoded = raw
                    .bytes
                    .ok_or_else(|| serde::de::Error::missing_field("bytes"))?;
                let decoded = STANDARD
                    .decode(encoded.as_bytes())
                    .map_err(serde::de::Error::custom)?;
                Ok(Self::Binary(BinaryBlob::new(decoded)))
            }
            other => Err(serde::de::Error::unknown_variant(
                other,
                &["ast", "text", "binary"],
            )),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn binary_roundtrip_serde() {
        let bytes = vec![0x89, 0x50, 0x4E, 0x47, 0, 0];
        let content = FileContent::Binary(BinaryBlob::new(bytes.clone()));
        let json = serde_json::to_vec(&content).unwrap();
        let loaded: FileContent = serde_json::from_slice(&json).unwrap();
        assert_eq!(content, loaded);
    }

    #[test]
    fn is_binary_payload_detects_nul_and_invalid_utf8() {
        assert!(is_binary_payload(&[0x89, 0x50, 0, 0x47]));
        assert!(is_binary_payload(&[0xFF, 0xFE]));
        assert!(!is_binary_payload(b"hello\n"));
    }
}
