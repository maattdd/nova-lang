use crate::token::Span;
use thiserror::Error;

#[allow(dead_code)]
#[derive(Error, Debug)]
pub enum CompileError {
    #[error("Lex error")]
    LexError { message: String, span: Span },

    #[error("Parse error")]
    ParseError { message: String, span: Span },

    #[error("Type error")]
    TypeError { message: String, span: Span },

    #[error("Macro error")]
    MacroError { message: String, span: Span },

    #[error("I/O error: {0}")]
    IoError(#[from] std::io::Error),

    #[error("{0}")]
    Generic(String),
}

impl CompileError {
    pub fn lex(message: impl Into<String>, span: Span) -> Self {
        CompileError::LexError { message: message.into(), span }
    }

    pub fn parse(message: impl Into<String>, span: Span) -> Self {
        CompileError::ParseError { message: message.into(), span }
    }

    pub fn type_err(message: impl Into<String>, span: Span) -> Self {
        CompileError::TypeError { message: message.into(), span }
    }

    pub fn macro_err(message: impl Into<String>, span: Span) -> Self {
        CompileError::MacroError { message: message.into(), span }
    }

    /// Pretty-print with ariadne
    pub fn display_with_source(&self, source: &str, file_path: &str) -> String {
        use ariadne::{Color, Label, Report, ReportKind, Source};

        let (kind, message, span) = match self {
            CompileError::LexError { message, span } => (ReportKind::Error, message.as_str(), span),
            CompileError::ParseError { message, span } => (ReportKind::Error, message.as_str(), span),
            CompileError::TypeError { message, span } => (ReportKind::Error, message.as_str(), span),
            CompileError::MacroError { message, span } => (ReportKind::Error, message.as_str(), span),
            CompileError::IoError(e) => return format!("I/O error: {}", e),
            CompileError::Generic(msg) => return msg.clone(),
        };

        let src = Source::from(source);
        let mut out = Vec::new();
        Report::build(kind, file_path, span.col.max(1) as usize)
            .with_message(message)
            .with_label(
                Label::new((file_path, span.start..span.end))
                    .with_message(message)
                    .with_color(Color::Red),
            )
            .finish()
            .write((file_path, src), &mut out)
            .unwrap();
        String::from_utf8_lossy(&out).to_string()
    }
}
