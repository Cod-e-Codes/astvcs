mod languages;
mod textblob;
mod treesitter;

pub use languages::{SourceLanguage, is_text_only_path, supported_extensions};
pub use textblob::{TextBlob, parse_text_or_blob};
pub use treesitter::{parse_language, parse_rust, parse_source};

use crate::graph::AstGraph;

/// Parsed representation of a source file.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum FileContent {
    Ast(AstGraph),
    Text(TextBlob),
}

impl FileContent {
    pub fn is_ast(&self) -> bool {
        matches!(self, Self::Ast(_))
    }

    pub fn display_kind(&self) -> &'static str {
        match self {
            Self::Ast(_) => "ast",
            Self::Text(_) => "text",
        }
    }

    pub fn semantic_eq(&self, other: &Self) -> bool {
        match (self, other) {
            (Self::Ast(a), Self::Ast(b)) => a.to_snapshot() == b.to_snapshot(),
            (Self::Text(a), Self::Text(b)) => a.content == b.content,
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
            other => Err(serde::de::Error::unknown_variant(other, &["ast", "text"])),
        }
    }
}
