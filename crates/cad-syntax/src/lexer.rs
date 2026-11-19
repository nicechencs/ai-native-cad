use crate::span::{LineIndex, Span};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TriviaKind {
    Whitespace,
    LineComment,
    BlockComment,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Trivia {
    pub kind: TriviaKind,
    pub text: String,
    pub span: Span,
}

#[derive(Debug, Clone, PartialEq)]
pub enum TokenKind {
    // Keywords
    Part,
    Assembly,
    Let,
    Return,
    Sketch,
    Const,
    True,
    False,

    // Identifiers & Literals
    Ident(String),
    Number {
        value: f64,
        raw_number: String,
        unit: Option<String>,
    },
    Str(String),

    // Multi-char operators
    PipeOp,     // |>
    Arrow,      // ->
    ColonColon, // ::
    EqEq,       // ==
    BangEq,     // !=
    LtEq,       // <=
    GtEq,       // >=
    AndAnd,     // &&
    OrOr,       // ||

    // Single-char punctuation & operators
    At,       // @
    LParen,   // (
    RParen,   // )
    LBrace,   // {
    RBrace,   // }
    LBracket, // [
    RBracket, // ]
    Comma,    // ,
    Colon,    // :
    Semi,     // ;
    Dot,      // .
    Eq,       // =
    Plus,     // +
    Minus,    // -
    Star,     // *
    Slash,    // /
    Percent,  // %
    Bang,     // !
    Lt,       // <
    Gt,       // >

    Eof,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Token {
    pub kind: TokenKind,
    pub lexeme: String,
    pub span: Span,
    pub leading_trivia: Vec<Trivia>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LexError {
    pub message: String,
    pub span: Span,
}

pub fn tokenize(source: &str) -> (Vec<Token>, Vec<LexError>) {
    let index = LineIndex::new(source);
    let bytes = source.as_bytes();
    let len = bytes.len();
    let mut i = 0usize;
    let mut tokens = Vec::new();
    let mut errors = Vec::new();
    let mut pending_trivia = Vec::new();

    while i < len {
        // Trivia: whitespace and comments
        if bytes[i].is_ascii_whitespace() {
            let start = i;
            while i < len && bytes[i].is_ascii_whitespace() {
                i += 1;
            }
            pending_trivia.push(Trivia {
                kind: TriviaKind::Whitespace,
                text: source[start..i].to_string(),
                span: index.span(source, start, i),
            });
            continue;
        }

        if i + 1 < len && bytes[i] == b'/' && bytes[i + 1] == b'/' {
            let start = i;
            i += 2;
            while i < len && bytes[i] != b'\n' {
                i += 1;
            }
            pending_trivia.push(Trivia {
                kind: TriviaKind::LineComment,
                text: source[start..i].to_string(),
                span: index.span(source, start, i),
            });
            continue;
        }

        if i + 1 < len && bytes[i] == b'/' && bytes[i + 1] == b'*' {
            let start = i;
            i += 2;
            let mut closed = false;
            while i + 1 < len {
                if bytes[i] == b'*' && bytes[i + 1] == b'/' {
                    i += 2;
                    closed = true;
                    break;
                }
                i += 1;
            }
            if !closed {
                i = len;
                errors.push(LexError {
                    message: "unterminated block comment".to_string(),
                    span: index.span(source, start, i),
                });
            }
            pending_trivia.push(Trivia {
                kind: TriviaKind::BlockComment,
                text: source[start..i].to_string(),
                span: index.span(source, start, i),
            });
            continue;
        }

        let start = i;
        let b = bytes[i];

        // String literal
        if b == b'"' {
            i += 1;
            let mut val = String::new();
            let mut terminated = false;
            while i < len {
                let ch = source[i..].chars().next().unwrap();
                if ch == '"' {
                    i += 1;
                    terminated = true;
                    break;
                } else if ch == '\\' {
                    i += 1;
                    if i < len {
                        let esc = source[i..].chars().next().unwrap();
                        i += esc.len_utf8();
                        match esc {
                            'n' => val.push('\n'),
                            'r' => val.push('\r'),
                            't' => val.push('\t'),
                            '\\' => val.push('\\'),
                            '"' => val.push('"'),
                            other => val.push(other),
                        }
                    }
                } else {
                    val.push(ch);
                    i += ch.len_utf8();
                }
            }
            if !terminated {
                errors.push(LexError {
                    message: "unterminated string literal".to_string(),
                    span: index.span(source, start, i),
                });
            }
            tokens.push(Token {
                kind: TokenKind::Str(val),
                lexeme: source[start..i].to_string(),
                span: index.span(source, start, i),
                leading_trivia: std::mem::take(&mut pending_trivia),
            });
            continue;
        }

        // Number literal with optional attached unit suffix (e.g., 42.0mm, 30.0deg, 1500.0N, 1e-3mm)
        if b.is_ascii_digit() {
            while i < len && (bytes[i].is_ascii_digit() || bytes[i] == b'_') {
                i += 1;
            }
            if i < len && bytes[i] == b'.' && i + 1 < len && bytes[i + 1].is_ascii_digit() {
                i += 1;
                while i < len && (bytes[i].is_ascii_digit() || bytes[i] == b'_') {
                    i += 1;
                }
            }
            // Scientific notation (e.g. 1e-4) only if followed by digit or +/- digit
            if i < len && (bytes[i] == b'e' || bytes[i] == b'E') {
                let exp_pos = i;
                let mut j = i + 1;
                if j < len && (bytes[j] == b'+' || bytes[j] == b'-') {
                    j += 1;
                }
                if j < len && bytes[j].is_ascii_digit() {
                    while j < len && (bytes[j].is_ascii_digit() || bytes[j] == b'_') {
                        j += 1;
                    }
                    i = j;
                } else {
                    i = exp_pos;
                }
            }
            let num_end = i;
            let raw_number = source[start..num_end].replace('_', "");
            let value = match raw_number.parse::<f64>() {
                Ok(v) => v,
                Err(_) => {
                    errors.push(LexError {
                        message: format!("invalid numeric literal `{}`", &source[start..num_end]),
                        span: index.span(source, start, num_end),
                    });
                    0.0
                }
            };

            let unit = if i < len && (bytes[i].is_ascii_alphabetic() || bytes[i] == b'_') {
                let u_start = i;
                while i < len && (bytes[i].is_ascii_alphanumeric() || bytes[i] == b'_') {
                    i += 1;
                }
                Some(source[u_start..i].to_string())
            } else {
                None
            };

            tokens.push(Token {
                kind: TokenKind::Number {
                    value,
                    raw_number: source[start..num_end].to_string(),
                    unit,
                },
                lexeme: source[start..i].to_string(),
                span: index.span(source, start, i),
                leading_trivia: std::mem::take(&mut pending_trivia),
            });
            continue;
        }

        // Identifier or keyword
        if b.is_ascii_alphabetic() || b == b'_' {
            i += 1;
            while i < len && (bytes[i].is_ascii_alphanumeric() || bytes[i] == b'_') {
                i += 1;
            }
            let text = &source[start..i];
            let kind = match text {
                "part" => TokenKind::Part,
                "assembly" => TokenKind::Assembly,
                "let" => TokenKind::Let,
                "return" => TokenKind::Return,
                "sketch" => TokenKind::Sketch,
                "const" => TokenKind::Const,
                "true" => TokenKind::True,
                "false" => TokenKind::False,
                _ => TokenKind::Ident(text.to_string()),
            };
            tokens.push(Token {
                kind,
                lexeme: text.to_string(),
                span: index.span(source, start, i),
                leading_trivia: std::mem::take(&mut pending_trivia),
            });
            continue;
        }

        // Two-character operators
        if i + 1 < len {
            let two = &bytes[i..i + 2];
            let kind_opt = match two {
                b"|>" => Some(TokenKind::PipeOp),
                b"->" => Some(TokenKind::Arrow),
                b"::" => Some(TokenKind::ColonColon),
                b"==" => Some(TokenKind::EqEq),
                b"!=" => Some(TokenKind::BangEq),
                b"<=" => Some(TokenKind::LtEq),
                b">=" => Some(TokenKind::GtEq),
                b"&&" => Some(TokenKind::AndAnd),
                b"||" => Some(TokenKind::OrOr),
                _ => None,
            };
            if let Some(kind) = kind_opt {
                i += 2;
                tokens.push(Token {
                    kind,
                    lexeme: source[start..i].to_string(),
                    span: index.span(source, start, i),
                    leading_trivia: std::mem::take(&mut pending_trivia),
                });
                continue;
            }
        }

        // Single-character punctuation/operators
        let single_opt = match b {
            b'@' => Some(TokenKind::At),
            b'(' => Some(TokenKind::LParen),
            b')' => Some(TokenKind::RParen),
            b'{' => Some(TokenKind::LBrace),
            b'}' => Some(TokenKind::RBrace),
            b'[' => Some(TokenKind::LBracket),
            b']' => Some(TokenKind::RBracket),
            b',' => Some(TokenKind::Comma),
            b':' => Some(TokenKind::Colon),
            b';' => Some(TokenKind::Semi),
            b'.' => Some(TokenKind::Dot),
            b'=' => Some(TokenKind::Eq),
            b'+' => Some(TokenKind::Plus),
            b'-' => Some(TokenKind::Minus),
            b'*' => Some(TokenKind::Star),
            b'/' => Some(TokenKind::Slash),
            b'%' => Some(TokenKind::Percent),
            b'!' => Some(TokenKind::Bang),
            b'<' => Some(TokenKind::Lt),
            b'>' => Some(TokenKind::Gt),
            _ => None,
        };

        if let Some(kind) = single_opt {
            i += 1;
            tokens.push(Token {
                kind,
                lexeme: source[start..i].to_string(),
                span: index.span(source, start, i),
                leading_trivia: std::mem::take(&mut pending_trivia),
            });
        } else {
            let ch = source[i..].chars().next().unwrap();
            i += ch.len_utf8();
            errors.push(LexError {
                message: format!("unexpected character `{ch}`"),
                span: index.span(source, start, i),
            });
        }
    }

    tokens.push(Token {
        kind: TokenKind::Eof,
        lexeme: String::new(),
        span: index.span(source, len, len),
        leading_trivia: pending_trivia,
    });

    (tokens, errors)
}
