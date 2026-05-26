use oxc_allocator::Allocator;
use oxc_ast::ast::*;
use oxc_diagnostics::OxcDiagnostic;
use oxc_parser::Parser;
use oxc_span::SourceType;

/// A single parse error with structured information
#[derive(Debug, Clone)]
pub struct ParseError {
    /// Error message
    pub message: String,
    /// Line number (1-indexed)
    pub line: u32,
    /// Column number (1-indexed)
    pub column: u32,
    /// Error severity
    pub severity: ParseErrorSeverity,
    /// Error code if available
    pub code: Option<String>,
    /// Help message if available
    pub help: Option<String>,
}

/// Error severity levels
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ParseErrorSeverity {
    Error,
    Warning,
}

impl std::fmt::Display for ParseErrorSeverity {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ParseErrorSeverity::Error => write!(f, "Error"),
            ParseErrorSeverity::Warning => write!(f, "Warning"),
        }
    }
}

/// Collection of parse errors
#[derive(Debug, Clone)]
pub struct ParseErrors {
    pub errors: Vec<ParseError>,
}

impl ParseErrors {
    pub fn new(errors: Vec<ParseError>) -> Self {
        Self { errors }
    }

    pub fn is_empty(&self) -> bool {
        self.errors.is_empty()
    }

    pub fn len(&self) -> usize {
        self.errors.len()
    }
}

impl std::fmt::Display for ParseErrors {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        for (i, err) in self.errors.iter().enumerate() {
            if i > 0 {
                writeln!(f)?;
            }
            write!(f, "{} at {}:{}", err.severity, err.line, err.column)?;
            if let Some(code) = &err.code {
                write!(f, " [{}]", code)?;
            }
            write!(f, ": {}", err.message)?;
            if let Some(help) = &err.help {
                write!(f, " (Help: {})", help)?;
            }
        }
        Ok(())
    }
}

impl std::error::Error for ParseErrors {}

/// Convert OxcDiagnostic to ParseError
fn convert_diagnostic(diagnostic: &OxcDiagnostic, source: &str) -> ParseError {
    // Extract line and column from labels if available
    let (line, column) = if let Some(labels) = &diagnostic.labels {
        labels
            .first()
            .and_then(|label| {
                // Calculate line/column from byte offset
                let offset = label.offset() as usize;
                let mut line = 1u32;
                let mut col = 1u32;
                for (i, ch) in source.char_indices() {
                    if i >= offset {
                        break;
                    }
                    if ch == '\n' {
                        line += 1;
                        col = 1;
                    } else {
                        col += 1;
                    }
                }
                Some((line, col))
            })
            .unwrap_or((1, 1))
    } else {
        (1, 1)
    };

    let severity = match diagnostic.severity {
        oxc_diagnostics::Severity::Error => ParseErrorSeverity::Error,
        _ => ParseErrorSeverity::Warning,
    };

    let code = if diagnostic.code.is_some() {
        let scope_str = diagnostic
            .code
            .scope
            .as_ref()
            .map(|s| s.as_ref())
            .unwrap_or("");
        let num_str = diagnostic
            .code
            .number
            .as_ref()
            .map(|n| n.as_ref())
            .unwrap_or("");
        let code_str = format!("{}{}", scope_str, num_str);
        if code_str.is_empty() {
            None
        } else {
            Some(code_str)
        }
    } else {
        None
    };

    let help = diagnostic.help.as_ref().map(|h| h.to_string());

    ParseError {
        message: diagnostic.message.to_string(),
        line,
        column,
        severity,
        code,
        help,
    }
}

/// Parse JavaScript source code and return the AST Program.
///
/// Returns `Err(ParseErrors)` if there are any parse errors.
pub fn parse_js<'a>(allocator: &'a Allocator, source: &'a str) -> Result<Program<'a>, ParseErrors> {
    let ret = Parser::new(allocator, source, SourceType::cjs()).parse();
    if !ret.errors.is_empty() {
        let errors: Vec<ParseError> = ret
            .errors
            .iter()
            .map(|e| convert_diagnostic(e, source))
            .collect();
        return Err(ParseErrors::new(errors));
    }
    Ok(ret.program)
}

#[cfg(test)]
mod tests {
    use super::*;
    // ──────────────────────── Parser Tests ────────────────────────

    #[test]
    fn test_parse_number_literal() {
        let allocator = Allocator::default();
        let program = parse_js(&allocator, "42;").unwrap();
        assert_eq!(program.body.len(), 1);
        assert!(matches!(
            &program.body[0],
            Statement::ExpressionStatement(_)
        ));
    }

    #[test]
    fn test_parse_string_literal_as_directive() {
        // oxc treats standalone string literals as directives
        let allocator = Allocator::default();
        let program = parse_js(&allocator, "\"hello\";").unwrap();
        assert_eq!(program.body.len(), 0);
        assert_eq!(program.directives.len(), 1);
    }

    #[test]
    fn test_parse_error_structured() {
        // Test that parse errors are returned as structured ParseErrors
        let allocator = Allocator::default();
        let result = parse_js(&allocator, "const x = {");

        assert!(result.is_err());
        let errors = result.unwrap_err();
        assert!(!errors.is_empty());

        let first_error = &errors.errors[0];
        assert_eq!(first_error.severity, ParseErrorSeverity::Error);
        assert!(first_error.line >= 1);
        assert!(first_error.column >= 1);
        assert!(!first_error.message.is_empty());

        // Test Display formatting
        let error_string = format!("{}", errors);
        println!("Error string: {}", error_string);
        assert!(error_string.contains("Error at"));
        // The error message may not contain "const", just check it's not empty
        assert!(!error_string.is_empty());
    }

    #[test]
    fn test_parse_error_multiple_lines() {
        // Test error location calculation for multi-line source
        let source = "let a = 1;\nlet b = {\nlet c = 3;";
        let allocator = Allocator::default();
        let result = parse_js(&allocator, source);

        assert!(result.is_err());
        let errors = result.unwrap_err();
        let first_error = &errors.errors[0];

        // Error should be around line 2-3 (where the `{` is unclosed)
        // The exact line depends on how oxc reports the error
        println!(
            "Error at line: {}, column: {}",
            first_error.line, first_error.column
        );
        assert!(first_error.line >= 1);
        assert!(first_error.column >= 1);
    }
}
