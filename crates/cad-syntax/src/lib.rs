pub mod ast;
pub mod lexer;
pub mod parser;
pub mod span;

pub use ast::*;
pub use lexer::{LexError, Token, TokenKind, Trivia, TriviaKind, tokenize};
pub use parser::{Parser, SyntaxError};
pub use span::{LineIndex, Span};

#[derive(Debug, Clone)]
pub struct ParseResult {
    pub file: SourceFile,
    pub tokens: Vec<Token>,
    pub lex_errors: Vec<LexError>,
    pub syntax_errors: Vec<SyntaxError>,
}

impl ParseResult {
    pub fn is_ok(&self) -> bool {
        self.lex_errors.is_empty() && self.syntax_errors.is_empty()
    }
}

pub fn parse(source: &str) -> ParseResult {
    let (tokens, lex_errors) = tokenize(source);
    let mut parser = Parser::new(&tokens);
    let file = parser.parse_source_file();
    let syntax_errors = parser.errors;
    ParseResult {
        file,
        tokens,
        lex_errors,
        syntax_errors,
    }
}

/// Losslessly rewrites a numeric literal at `span` while preserving its attached unit suffix
/// and all surrounding whitespace/comments (CST invariant for Tier 1 writeback).
pub fn rewrite_quantity_preserving_unit(
    source: &str,
    span: Span,
    new_number: f64,
) -> Result<String, String> {
    if span.start_byte > span.end_byte || span.end_byte > source.len() {
        return Err("span byte range out of bounds".to_string());
    }
    let slice = &source[span.start_byte..span.end_byte];
    let unit_start = slice
        .find(|c: char| c.is_ascii_alphabetic() || c == '_')
        .unwrap_or(slice.len());
    let unit = &slice[unit_start..];
    let formatted = if (new_number - new_number.round()).abs() < 1e-9 {
        format!("{new_number:.1}{unit}")
    } else {
        format!("{new_number:.4}", new_number = new_number)
            .trim_end_matches('0')
            .trim_end_matches('.')
            .to_string()
            + unit
    };
    let mut out = String::with_capacity(source.len() + 8);
    out.push_str(&source[..span.start_byte]);
    out.push_str(&formatted);
    out.push_str(&source[span.end_byte..]);
    Ok(out)
}
