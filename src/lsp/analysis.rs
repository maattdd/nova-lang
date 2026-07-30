//! Document state for LSP analysis.

/// Represents an open document in the editor.
#[derive(Clone, Debug)]
pub struct Document {
    /// Full source text
    pub source: String,
}

impl Document {
    pub fn new(source: String) -> Self {
        Self { source }
    }
}
