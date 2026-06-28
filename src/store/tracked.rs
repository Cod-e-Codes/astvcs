use crate::frontend::FileContent;
use crate::store::blobs::BlobStore;
use crate::store::manifest::FileMode;

/// Working-tree or committed file: content blob plus mode metadata.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TrackedFile {
    pub content: FileContent,
    pub mode: FileMode,
}

impl TrackedFile {
    pub fn regular(content: FileContent) -> Self {
        Self {
            mode: FileMode::Regular,
            content,
        }
    }

    pub fn new(content: FileContent, mode: FileMode) -> Self {
        Self { content, mode }
    }

    pub fn content_ref(&self) -> &FileContent {
        &self.content
    }

    pub fn semantic_eq(&self, other: &TrackedFile) -> bool {
        self.mode == other.mode && self.content.semantic_eq(&other.content)
    }

    pub fn content_hash(&self) -> Result<String, String> {
        BlobStore::hash_content(&self.content)
    }
}

pub fn tracked_eq(a: &TrackedFile, b: &TrackedFile) -> bool {
    match (a.content_hash(), b.content_hash()) {
        (Ok(ha), Ok(hb)) => ha == hb && a.mode == b.mode,
        _ => false,
    }
}
