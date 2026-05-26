use std::fmt;

/// A source location in JavaScript source code.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct SourceLocation {
    pub line: u32,
    pub column: u32,
}

impl SourceLocation {
    pub fn new(line: u32, column: u32) -> Self {
        Self { line, column }
    }
}

impl fmt::Display for SourceLocation {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}:{}", self.line, self.column)
    }
}

/// Errors that occur during compilation (parsing → IR → bytecode).
#[derive(Debug)]
pub enum CompileError {
    /// Parser/syntax error with source location
    SyntaxError {
        message: String,
        location: SourceLocation,
    },
    /// Error during AST-to-IR lowering
    LowerError {
        message: String,
        location: Option<SourceLocation>,
    },
    /// Semantic error (e.g., const reassignment)
    SemanticError {
        message: String,
        location: Option<SourceLocation>,
    },
    /// Internal compiler error (SSA, regalloc, codegen invariants)
    InternalError(String),
    /// Feature not yet implemented
    NotImplemented(String),
}

impl CompileError {
    pub fn syntax(msg: impl Into<String>, line: u32, column: u32) -> Self {
        CompileError::SyntaxError {
            message: msg.into(),
            location: SourceLocation::new(line, column),
        }
    }

    pub fn lower(msg: impl Into<String>) -> Self {
        CompileError::LowerError {
            message: msg.into(),
            location: None,
        }
    }

    pub fn lower_at(msg: impl Into<String>, line: u32, column: u32) -> Self {
        CompileError::LowerError {
            message: msg.into(),
            location: Some(SourceLocation::new(line, column)),
        }
    }

    pub fn internal(msg: impl Into<String>) -> Self {
        CompileError::InternalError(msg.into())
    }

    pub fn not_implemented(msg: impl Into<String>) -> Self {
        CompileError::NotImplemented(msg.into())
    }
}

impl fmt::Display for CompileError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            CompileError::SyntaxError { message, location } => {
                write!(f, "SyntaxError at {}: {message}", location)
            }
            CompileError::LowerError {
                message,
                location: Some(loc),
            } => {
                write!(f, "LowerError at {loc}: {message}")
            }
            CompileError::LowerError {
                message,
                location: None,
            } => {
                write!(f, "LowerError: {message}")
            }
            CompileError::SemanticError {
                message,
                location: Some(loc),
            } => {
                write!(f, "SemanticError at {loc}: {message}")
            }
            CompileError::SemanticError {
                message,
                location: None,
            } => {
                write!(f, "SemanticError: {message}")
            }
            CompileError::InternalError(msg) => {
                write!(f, "InternalError: {msg}")
            }
            CompileError::NotImplemented(msg) => {
                write!(f, "NotImplemented: {msg}")
            }
        }
    }
}

impl std::error::Error for CompileError {}
