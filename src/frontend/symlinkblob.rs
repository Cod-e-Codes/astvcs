/// Symlink target stored as its own blob kind (content-addressed by target string).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SymlinkBlob {
    pub target: String,
}

impl SymlinkBlob {
    pub fn new(target: String) -> Self {
        Self { target }
    }
}
