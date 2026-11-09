//! The lexer. ECMAScript lexing is context dependent (`/` starts a regular
//! expression or a division, `}` may continue a template), so the parser
//! drives it: [`Lexer::next_token`] lexes the next token for the operator
//! context, and the parser asks for a re-scan when it knows better.
//!
//! Positions are byte offsets into the UTF-8 source. Source that came from
//! a JavaScript string carries its lone surrogates in a side table (see
//! [`crate::source`]); literal values put them back.

use crate::error::{Result, SyntaxError};
use crate::source::{self, LoneSurrogate};
use crate::span::Span;
use std::borrow::Cow;

#[derive(Clone, Debug, PartialEq)]
pub enum TokenKind<'s> {
    Eof,
    /// IdentifierName, including reserved words. `escaped` is set when the
    /// name contained a Unicode escape (such a name can never be a keyword).
    Ident {
        name: Cow<'s, str>,
        escaped: bool,
    },
    PrivateName(Cow<'s, str>),
    Number {
        value: f64,
        kind: NumberKind,
    },
    BigInt(Box<str>),
    /// A string literal. `legacy_octal` records a `\0`-prefixed octal or
    /// `\8`/`\9` escape (an error in strict code), checked once the parser
    /// knows the directive prologue's outcome.
    String {
        value: Box<[u16]>,
        legacy_octal: bool,
    },
    /// `...${` or a whole template without substitutions. `cooked` is None
    /// when the template contains an invalid escape (allowed only in tagged
    /// templates).
    Template {
        cooked: Option<Box<[u16]>>,
        raw: Box<[u16]>,
        tail: bool,
    },
    /// The pattern is UTF-16 code units: it may hold lone surrogates.
    RegExp {
        pattern: Box<[u16]>,
        flags: Box<str>,
    },
    Punct(Punct),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NumberKind {
    Decimal,
    /// `0777`: an error in strict code.
    LegacyOctal,
    /// `089`: an error in strict code.
    LegacyDecimal,
    Hex,
    Octal,
    Binary,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Punct {
    LBrace,
    RBrace,
    LParen,
    RParen,
    LBracket,
    RBracket,
    Dot,
    Ellipsis,
    Semicolon,
    Comma,
    Lt,
    Gt,
    LtEq,
    GtEq,
    EqEq,
    NotEq,
    EqEqEq,
    NotEqEq,
    Plus,
    Minus,
    Star,
    Slash,
    Percent,
    StarStar,
    PlusPlus,
    MinusMinus,
    Shl,
    Shr,
    UShr,
    Amp,
    Pipe,
    Caret,
    Bang,
    Tilde,
    AmpAmp,
    PipePipe,
    QuestionQuestion,
    Question,
    QuestionDot,
    Colon,
    Eq,
    PlusEq,
    MinusEq,
    StarEq,
    SlashEq,
    PercentEq,
    StarStarEq,
    ShlEq,
    ShrEq,
    UShrEq,
    AmpEq,
    PipeEq,
    CaretEq,
    AmpAmpEq,
    PipePipeEq,
    QuestionQuestionEq,
    Arrow,
    Hash,
    At,
}

#[derive(Clone, Debug)]
pub struct Token<'s> {
    pub kind: TokenKind<'s>,
    pub span: Span,
    /// A line terminator appeared between the previous token and this one.
    pub newline_before: bool,
}

#[derive(Clone, Copy)]
pub struct LexerState {
    pos: usize,
    line_start_seen: bool,
}

pub struct Lexer<'s> {
    src: &'s str,
    bytes: &'s [u8],
    /// The lone surrogates standing as U+FFFD in `src` (empty for source
    /// that was a Rust `str`).
    lone: &'s [LoneSurrogate],
    pos: usize,
    /// Whether the only things so far on the current line are whitespace
    /// and comments (for the Annex B `-->` comment).
    at_line_start: bool,
    /// Script goal allows HTML-like comments; the module goal does not.
    pub module: bool,
}

pub fn is_line_terminator(c: char) -> bool {
    matches!(c, '\n' | '\r' | '\u{2028}' | '\u{2029}')
}

pub fn is_whitespace(c: char) -> bool {
    matches!(
        c,
        '\t' | '\u{000B}' | '\u{000C}' | ' ' | '\u{00A0}' | '\u{FEFF}' | '\u{1680}' | '\u{2000}'
            ..='\u{200A}' | '\u{202F}' | '\u{205F}' | '\u{3000}'
    )
}

pub fn is_id_start(c: char) -> bool {
    c == '$' || c == '_' || unicode_id_start::is_id_start(c)
}

pub fn is_id_continue(c: char) -> bool {
    c == '$' || c == '\u{200C}' || c == '\u{200D}' || unicode_id_start::is_id_continue(c)
}

impl<'s> Lexer<'s> {
    pub fn new(src: &'s str, module: bool) -> Lexer<'s> {
        Lexer::with_lone_surrogates(src, &[], module)
    }

    /// A lexer over source converted from UTF-16 by
    /// [`source::from_utf16`].
    pub fn with_lone_surrogates(
        src: &'s str,
        lone: &'s [LoneSurrogate],
        module: bool,
    ) -> Lexer<'s> {
        let mut lx = Lexer {
            src,
            bytes: src.as_bytes(),
            lone,
            pos: 0,
            at_line_start: true,
            module,
        };
        // Hashbang comment: only at the very start.
        if lx.bytes.starts_with(b"#!") {
            lx.skip_to_line_end();
        }
        lx
    }

    pub fn source(&self) -> &'s str {
        self.src
    }

    pub fn lone_surrogates(&self) -> &'s [LoneSurrogate] {
        self.lone
    }

    /// Append the code units of `c`, which starts at byte `at`: a U+FFFD
    /// listed in the lone-surrogate table is that surrogate.
    fn push_char(&self, out: &mut Vec<u16>, c: char, at: usize) {
        if c == '\u{FFFD}' {
            if let Some(u) = source::lone_at(self.lone, at) {
                out.push(u);
                return;
            }
        }
        let mut buf = [0u16; 2];
        out.extend_from_slice(c.encode_utf16(&mut buf));
    }

    pub fn state(&self) -> LexerState {
        LexerState {
            pos: self.pos,
            line_start_seen: self.at_line_start,
        }
    }

    pub fn restore(&mut self, s: LexerState) {
        self.pos = s.pos;
        self.at_line_start = s.line_start_seen;
    }

    pub fn pos(&self) -> u32 {
        self.pos as u32
    }

    fn peek(&self) -> Option<char> {
        self.src[self.pos..].chars().next()
    }

    fn peek_byte(&self) -> Option<u8> {
        self.bytes.get(self.pos).copied()
    }

    fn byte_at(&self, n: usize) -> Option<u8> {
        self.bytes.get(self.pos + n).copied()
    }

    fn bump(&mut self) -> Option<char> {
        let c = self.peek()?;
        self.pos += c.len_utf8();
        Some(c)
    }

    fn eat(&mut self, c: char) -> bool {
        if self.peek() == Some(c) {
            self.pos += c.len_utf8();
            true
        } else {
            false
        }
    }

    fn err<T>(&self, message: impl Into<String>, start: usize) -> Result<T> {
        Err(SyntaxError::new(
            message,
            Span::new(start as u32, self.pos.max(start) as u32),
        ))
    }

    fn skip_to_line_end(&mut self) {
        while let Some(c) = self.peek() {
            if is_line_terminator(c) {
                break;
            }
            self.pos += c.len_utf8();
        }
    }

    /// Skip whitespace and comments. Returns whether a line terminator was
    /// crossed.
    fn skip_trivia(&mut self) -> Result<bool> {
        let mut newline = false;
        loop {
            let Some(c) = self.peek() else { break };
            if is_line_terminator(c) {
                self.pos += c.len_utf8();
                newline = true;
                self.at_line_start = true;
            } else if is_whitespace(c) {
                self.pos += c.len_utf8();
            } else if c == '/' && self.byte_at(1) == Some(b'/') {
                self.skip_to_line_end();
            } else if c == '/' && self.byte_at(1) == Some(b'*') {
                let start = self.pos;
                self.pos += 2;
                loop {
                    match self.bump() {
                        None => return self.err("unterminated comment", start),
                        Some('*') if self.peek_byte() == Some(b'/') => {
                            self.pos += 1;
                            break;
                        }
                        Some(c) if is_line_terminator(c) => {
                            newline = true;
                            self.at_line_start = true;
                        }
                        _ => {}
                    }
                }
            } else if !self.module && c == '<' && self.src[self.pos..].starts_with("<!--") {
                // Annex B SingleLineHTMLOpenComment.
                self.skip_to_line_end();
            } else if !self.module
                && c == '-'
                && self.at_line_start
                && self.src[self.pos..].starts_with("-->")
            {
                // Annex B SingleLineHTMLCloseComment: only after a line
                // terminator (or at the start of the script), with only
                // whitespace and comments before it on the line.
                self.skip_to_line_end();
            } else {
                break;
            }
        }
        Ok(newline)
    }

    /// Lex the next token. `/` is lexed as division; the parser calls
    /// [`Lexer::rescan_regexp`] when it expects an expression.
    pub fn next_token(&mut self) -> Result<Token<'s>> {
        let newline_before = self.skip_trivia()?;
        let start = self.pos;
        let Some(c) = self.peek() else {
            return Ok(Token {
                kind: TokenKind::Eof,
                span: Span::empty(start as u32),
                newline_before,
            });
        };
        let kind = match c {
            '"' | '\'' => self.lex_string(c)?,
            '`' => {
                self.pos += 1;
                self.lex_template_part(start)?
            }
            '0'..='9' => self.lex_number()?,
            '.' if matches!(self.byte_at(1), Some(b'0'..=b'9')) => self.lex_number()?,
            '#' => {
                self.pos += 1;
                if self.peek().is_some_and(is_id_start) || self.peek() == Some('\\') {
                    let name = self.lex_identifier_name()?;
                    TokenKind::PrivateName(name.0)
                } else {
                    return self.err("invalid or unexpected token", start);
                }
            }
            '\\' => {
                let (name, escaped) = self.lex_identifier_name()?;
                TokenKind::Ident { name, escaped }
            }
            c if is_id_start(c) => {
                let (name, escaped) = self.lex_identifier_name()?;
                TokenKind::Ident { name, escaped }
            }
            _ => TokenKind::Punct(self.lex_punct(start)?),
        };
        self.at_line_start = false;
        Ok(Token {
            kind,
            span: Span::new(start as u32, self.pos as u32),
            newline_before,
        })
    }

    fn lex_punct(&mut self, start: usize) -> Result<Punct> {
        use Punct::*;
        let b = self.bytes;
        let p = self.pos;
        let at = |i: usize| b.get(p + i).copied();
        let (punct, len) = match at(0).unwrap() {
            b'{' => (LBrace, 1),
            b'}' => (RBrace, 1),
            b'(' => (LParen, 1),
            b')' => (RParen, 1),
            b'[' => (LBracket, 1),
            b']' => (RBracket, 1),
            b';' => (Semicolon, 1),
            b',' => (Comma, 1),
            b':' => (Colon, 1),
            b'~' => (Tilde, 1),
            b'@' => (At, 1),
            b'.' => {
                if at(1) == Some(b'.') && at(2) == Some(b'.') {
                    (Ellipsis, 3)
                } else {
                    (Dot, 1)
                }
            }
            b'<' => match (at(1), at(2)) {
                (Some(b'<'), Some(b'=')) => (ShlEq, 3),
                (Some(b'<'), _) => (Shl, 2),
                (Some(b'='), _) => (LtEq, 2),
                _ => (Lt, 1),
            },
            b'>' => match (at(1), at(2), at(3)) {
                (Some(b'>'), Some(b'>'), Some(b'=')) => (UShrEq, 4),
                (Some(b'>'), Some(b'>'), _) => (UShr, 3),
                (Some(b'>'), Some(b'='), _) => (ShrEq, 3),
                (Some(b'>'), _, _) => (Shr, 2),
                (Some(b'='), _, _) => (GtEq, 2),
                _ => (Gt, 1),
            },
            b'=' => match (at(1), at(2)) {
                (Some(b'='), Some(b'=')) => (EqEqEq, 3),
                (Some(b'='), _) => (EqEq, 2),
                (Some(b'>'), _) => (Arrow, 2),
                _ => (Eq, 1),
            },
            b'!' => match (at(1), at(2)) {
                (Some(b'='), Some(b'=')) => (NotEqEq, 3),
                (Some(b'='), _) => (NotEq, 2),
                _ => (Bang, 1),
            },
            b'+' => match at(1) {
                Some(b'+') => (PlusPlus, 2),
                Some(b'=') => (PlusEq, 2),
                _ => (Plus, 1),
            },
            b'-' => match at(1) {
                Some(b'-') => (MinusMinus, 2),
                Some(b'=') => (MinusEq, 2),
                _ => (Minus, 1),
            },
            b'*' => match (at(1), at(2)) {
                (Some(b'*'), Some(b'=')) => (StarStarEq, 3),
                (Some(b'*'), _) => (StarStar, 2),
                (Some(b'='), _) => (StarEq, 2),
                _ => (Star, 1),
            },
            b'/' => match at(1) {
                Some(b'=') => (SlashEq, 2),
                _ => (Slash, 1),
            },
            b'%' => match at(1) {
                Some(b'=') => (PercentEq, 2),
                _ => (Percent, 1),
            },
            b'&' => match (at(1), at(2)) {
                (Some(b'&'), Some(b'=')) => (AmpAmpEq, 3),
                (Some(b'&'), _) => (AmpAmp, 2),
                (Some(b'='), _) => (AmpEq, 2),
                _ => (Amp, 1),
            },
            b'|' => match (at(1), at(2)) {
                (Some(b'|'), Some(b'=')) => (PipePipeEq, 3),
                (Some(b'|'), _) => (PipePipe, 2),
                (Some(b'='), _) => (PipeEq, 2),
                _ => (Pipe, 1),
            },
            b'^' => match at(1) {
                Some(b'=') => (CaretEq, 2),
                _ => (Caret, 1),
            },
            b'?' => match (at(1), at(2)) {
                (Some(b'?'), Some(b'=')) => (QuestionQuestionEq, 3),
                (Some(b'?'), _) => (QuestionQuestion, 2),
                // `?.` followed by a digit is a conditional with a decimal.
                (Some(b'.'), Some(d)) if !d.is_ascii_digit() => (QuestionDot, 2),
                (Some(b'.'), None) => (QuestionDot, 2),
                _ => (Question, 1),
            },
            _ => {
                self.bump();
                return self.err("invalid or unexpected token", start);
            }
        };
        self.pos += len;
        Ok(punct)
    }

    /// Returns (name, had_escape). A name without escapes borrows the
    /// source; only an escaped name is built up in a `String`.
    fn lex_identifier_name(&mut self) -> Result<(Cow<'s, str>, bool)> {
        let start = self.pos;
        // Set (to the decoded name so far) once an escape is seen.
        let mut owned: Option<String> = None;
        let mut first = true;
        loop {
            // ASCII fast path: `$`, `_`, letters and (after the first
            // character) digits.
            if let Some(&b) = self.bytes.get(self.pos) {
                if b.is_ascii_alphabetic() || b == b'_' || b == b'$' || (!first && b.is_ascii_digit())
                {
                    self.pos += 1;
                    if let Some(name) = owned.as_mut() {
                        name.push(b as char);
                    }
                    first = false;
                    continue;
                }
                if b.is_ascii() && b != b'\\' {
                    break;
                }
            }
            let Some(c) = self.peek() else { break };
            let ch = if c == '\\' {
                let esc_start = self.pos;
                let src = self.src;
                owned.get_or_insert_with(|| src[start..esc_start].to_string());
                self.pos += 1;
                if !self.eat('u') {
                    return self.err("invalid Unicode escape sequence", esc_start);
                }
                let cp = self.lex_unicode_escape_body(esc_start)?;
                let Some(ch) = char::from_u32(cp) else {
                    return self.err("invalid Unicode escape sequence", esc_start);
                };
                let ok = if first {
                    is_id_start(ch)
                } else {
                    is_id_continue(ch)
                };
                if !ok {
                    return self.err("invalid identifier escape", esc_start);
                }
                ch
            } else {
                let ok = if first {
                    is_id_start(c)
                } else {
                    is_id_continue(c)
                };
                if !ok {
                    break;
                }
                self.pos += c.len_utf8();
                c
            };
            if let Some(name) = owned.as_mut() {
                name.push(ch);
            }
            first = false;
        }
        if self.pos == start {
            return self.err("unexpected character", start);
        }
        Ok(match owned {
            Some(name) => (Cow::Owned(name), true),
            None => (Cow::Borrowed(&self.src[start..self.pos]), false),
        })
    }

    /// After `\u`: `{HexDigits}` or four hex digits. Returns the code point.
    fn lex_unicode_escape_body(&mut self, start: usize) -> Result<u32> {
        if self.eat('{') {
            let mut value: u32 = 0;
            let mut digits = 0;
            while let Some(c) = self.peek() {
                if c == '}' {
                    break;
                }
                let Some(d) = c.to_digit(16) else {
                    return self.err("invalid Unicode escape sequence", start);
                };
                value = value.saturating_mul(16).saturating_add(d);
                if value > 0x10FFFF {
                    return self.err("Unicode escape out of range", start);
                }
                digits += 1;
                self.pos += 1;
            }
            if digits == 0 || !self.eat('}') {
                return self.err("invalid Unicode escape sequence", start);
            }
            Ok(value)
        } else {
            let mut value = 0;
            for _ in 0..4 {
                let Some(d) = self.peek().and_then(|c| c.to_digit(16)) else {
                    return self.err("invalid Unicode escape sequence", start);
                };
                value = value * 16 + d;
                self.pos += 1;
            }
            Ok(value)
        }
    }

    fn lex_number(&mut self) -> Result<TokenKind<'s>> {
        let start = self.pos;
        let mut kind = NumberKind::Decimal;
        let mut is_bigint = false;
        let value: f64;

        if self.peek_byte() == Some(b'0')
            && matches!(
                self.byte_at(1),
                Some(b'x' | b'X' | b'o' | b'O' | b'b' | b'B')
            )
        {
            let radix = match self.byte_at(1).unwrap().to_ascii_lowercase() {
                b'x' => 16,
                b'o' => 8,
                _ => 2,
            };
            kind = match radix {
                16 => NumberKind::Hex,
                8 => NumberKind::Octal,
                _ => NumberKind::Binary,
            };
            self.pos += 2;
            let digits = self.lex_digits(radix, start)?;
            if digits.is_empty() {
                return self.err("invalid number literal", start);
            }
            if self.eat('n') {
                is_bigint = true;
            }
            value = parse_radix(&digits, radix);
            if is_bigint {
                self.check_after_number(start)?;
                let dec = radix_to_decimal(&digits, radix);
                return Ok(TokenKind::BigInt(dec.into_boxed_str()));
            }
        } else if self.peek_byte() == Some(b'0') && matches!(self.byte_at(1), Some(b'0'..=b'9')) {
            // Legacy octal (0777) or legacy decimal (089).
            self.pos += 1;
            let mut digits = String::new();
            let mut octal = true;
            while let Some(c) = self.peek() {
                if c.is_ascii_digit() {
                    if c >= '8' {
                        octal = false;
                    }
                    digits.push(c);
                    self.pos += 1;
                } else if c == '_' {
                    return self.err(
                        "numeric separators are not allowed in legacy literals",
                        self.pos,
                    );
                } else {
                    break;
                }
            }
            if octal {
                kind = NumberKind::LegacyOctal;
                value = parse_radix(&digits, 8);
            } else {
                kind = NumberKind::LegacyDecimal;
                let mut text = digits;
                if self.eat('.') {
                    text.push('.');
                    text.push_str(&self.lex_digits(10, start)?);
                }
                if matches!(self.peek(), Some('e' | 'E')) {
                    text.push_str(&self.lex_exponent(start)?);
                }
                value = text.parse().unwrap_or(f64::NAN);
            }
            if self.peek() == Some('n') {
                return self.err("invalid BigInt literal", start);
            }
        } else {
            // The literal is scanned (and validated) in place; its text is
            // the source slice, minus any separators.
            let lit_start = self.pos;
            let mut saw_dot = false;
            let mut saw_exp = false;
            if self.peek_byte() != Some(b'.') {
                if self.peek_byte() == Some(b'0') && self.byte_at(1) == Some(b'_') {
                    return self.err(
                        "numeric separator not allowed after a leading zero",
                        self.pos + 1,
                    );
                }
                self.scan_digits(10, start, None)?;
            }
            if self.peek_byte() == Some(b'.') {
                saw_dot = true;
                self.pos += 1;
                if self.peek_byte() == Some(b'_') {
                    return self.err("numeric separator not allowed here", self.pos);
                }
                self.scan_digits(10, start, None)?;
            }
            if matches!(self.peek(), Some('e' | 'E')) {
                saw_exp = true;
                self.scan_exponent(start, None)?;
            }
            let src = self.src;
            let raw = &src[lit_start..self.pos];
            let text: Cow<'s, str> = if raw.contains('_') {
                Cow::Owned(raw.replace('_', ""))
            } else {
                Cow::Borrowed(raw)
            };
            if self.peek() == Some('n') {
                if saw_dot || saw_exp {
                    return self.err("invalid BigInt literal", start);
                }
                self.pos += 1;
                self.check_after_number(start)?;
                let trimmed = text.trim_start_matches('0');
                let dec = if trimmed.is_empty() { "0" } else { trimmed };
                return Ok(TokenKind::BigInt(dec.into()));
            }
            if text == "." {
                return self.err("invalid number literal", start);
            }
            value = if text.starts_with('.') {
                format!("0{text}").parse::<f64>()
            } else {
                text.parse::<f64>()
            }
            .unwrap_or(f64::NAN);
        }
        self.check_after_number(start)?;
        Ok(TokenKind::Number { value, kind })
    }

    /// Digits in `radix` with `_` separators (not leading, trailing or
    /// doubled). Returns the digits without separators.
    fn lex_digits(&mut self, radix: u32, start: usize) -> Result<String> {
        let mut out = String::new();
        self.scan_digits(radix, start, Some(&mut out))?;
        Ok(out)
    }

    /// Scan digits in `radix` with `_` separators (not leading, trailing
    /// or doubled), appending them without separators to `out` if given.
    fn scan_digits(&mut self, radix: u32, start: usize, mut out: Option<&mut String>) -> Result<()> {
        let mut last_sep = false;
        let mut any = false;
        loop {
            let Some(c) = self.peek() else { break };
            if c == '_' {
                if !any || last_sep {
                    return self.err("numeric separator not allowed here", self.pos);
                }
                last_sep = true;
                self.pos += 1;
                continue;
            }
            if c.is_digit(radix) {
                if let Some(out) = out.as_mut() {
                    out.push(c);
                }
                any = true;
                last_sep = false;
                self.pos += 1;
            } else {
                break;
            }
        }
        if last_sep {
            return self.err("numeric separator not allowed at the end", start);
        }
        Ok(())
    }

    fn lex_exponent(&mut self, start: usize) -> Result<String> {
        let mut text = String::new();
        self.scan_exponent(start, Some(&mut text))?;
        Ok(text)
    }

    /// Scan an exponent part (`e`, an optional sign, digits), appending it
    /// with a lower-case `e` and without separators to `out` if given.
    fn scan_exponent(&mut self, start: usize, mut out: Option<&mut String>) -> Result<()> {
        self.pos += 1;
        if let Some(out) = out.as_mut() {
            out.push('e');
        }
        if let Some(c @ ('+' | '-')) = self.peek() {
            if let Some(out) = out.as_mut() {
                out.push(c);
            }
            self.pos += 1;
        }
        let digits_start = self.pos;
        self.scan_digits(10, start, out)?;
        if self.pos == digits_start {
            return self.err("missing exponent digits", start);
        }
        Ok(())
    }

    fn check_after_number(&mut self, start: usize) -> Result<()> {
        // The SourceCharacter immediately following a NumericLiteral must
        // not be an IdentifierStart or DecimalDigit.
        if let Some(c) = self.peek() {
            if is_id_start(c) || c.is_ascii_digit() || c == '\\' {
                return self.err("identifier starts immediately after numeric literal", start);
            }
        }
        Ok(())
    }

    fn lex_string(&mut self, quote: char) -> Result<TokenKind<'s>> {
        let start = self.pos;
        self.pos += 1;
        // Fast path: an ASCII string without escapes or line breaks is
        // its source bytes, widened.
        let q = quote as u8;
        let body = &self.bytes[self.pos..];
        if let Some(len) = body
            .iter()
            .position(|&b| b == q || b == b'\\' || b == b'\n' || b == b'\r' || !b.is_ascii())
        {
            if body[len] == q {
                let value: Box<[u16]> = body[..len].iter().map(|&b| b as u16).collect();
                self.pos += len + 1;
                return Ok(TokenKind::String {
                    value,
                    legacy_octal: false,
                });
            }
        }
        let mut value: Vec<u16> = Vec::new();
        let mut legacy_octal = false;
        loop {
            let Some(c) = self.bump() else {
                return self.err("unterminated string literal", start);
            };
            if c == quote {
                break;
            }
            match c {
                '\\' => {
                    let esc_start = self.pos - 1;
                    match self.lex_escape(esc_start, false)? {
                        Escape::Units(units) => value.extend_from_slice(units.as_slice()),
                        Escape::LineContinuation => {}
                        Escape::LegacyOctal(unit) => {
                            legacy_octal = true;
                            value.push(unit);
                        }
                        Escape::Invalid => return self.err("invalid escape sequence", esc_start),
                    }
                }
                '\n' | '\r' => return self.err("unterminated string literal", start),
                c => self.push_char(&mut value, c, self.pos - c.len_utf8()),
            }
        }
        Ok(TokenKind::String {
            value: value.into_boxed_slice(),
            legacy_octal,
        })
    }

    /// After the backslash of an escape. `in_template` makes octal-ish
    /// escapes invalid instead of legacy.
    fn lex_escape(&mut self, esc_start: usize, in_template: bool) -> Result<Escape> {
        let Some(c) = self.bump() else {
            return self.err("unterminated escape", esc_start);
        };
        let unit = |u: u16| Escape::Units(Units::one(u));
        Ok(match c {
            'n' => unit(0x0A),
            't' => unit(0x09),
            'r' => unit(0x0D),
            'b' => unit(0x08),
            'v' => unit(0x0B),
            'f' => unit(0x0C),
            '\r' => {
                self.eat('\n');
                Escape::LineContinuation
            }
            '\n' | '\u{2028}' | '\u{2029}' => Escape::LineContinuation,
            '0' if !self.peek().is_some_and(|d| d.is_ascii_digit()) => unit(0),
            '0'..='7' => {
                if in_template {
                    return Ok(Escape::Invalid);
                }
                // Legacy octal: up to three digits, value <= 255.
                let mut v = c.to_digit(8).unwrap();
                if let Some(d) = self.peek().and_then(|d| d.to_digit(8)) {
                    v = v * 8 + d;
                    self.pos += 1;
                    if c <= '3' {
                        if let Some(d) = self.peek().and_then(|d| d.to_digit(8)) {
                            v = v * 8 + d;
                            self.pos += 1;
                        }
                    }
                }
                Escape::LegacyOctal(v as u16)
            }
            '8' | '9' => {
                if in_template {
                    return Ok(Escape::Invalid);
                }
                Escape::LegacyOctal(c as u16)
            }
            'x' => {
                let mut v = 0u32;
                for _ in 0..2 {
                    let Some(d) = self.peek().and_then(|d| d.to_digit(16)) else {
                        return Ok(Escape::Invalid);
                    };
                    v = v * 16 + d;
                    self.pos += 1;
                }
                unit(v as u16)
            }
            'u' => {
                let save = self.pos;
                match self.lex_unicode_escape_body(esc_start) {
                    Ok(cp) => {
                        Escape::Units(if cp <= 0xFFFF {
                            Units::one(cp as u16)
                        } else {
                            let c = cp - 0x10000;
                            Units {
                                buf: [0xD800 + (c >> 10) as u16, 0xDC00 + (c & 0x3FF) as u16],
                                len: 2,
                            }
                        })
                    }
                    Err(_) => {
                        self.pos = save;
                        Escape::Invalid
                    }
                }
            }
            '\u{FFFD}' if !self.lone.is_empty() => {
                // NonEscapeCharacter: a lone surrogate escapes to itself.
                let mut units = Vec::with_capacity(1);
                self.push_char(&mut units, '\u{FFFD}', self.pos - 3);
                Escape::Units(Units::one(units[0]))
            }
            c => {
                let mut buf = [0u16; 2];
                let len = c.encode_utf16(&mut buf).len() as u8;
                Escape::Units(Units { buf, len })
            }
        })
    }

    /// Lex a template span after the opening `` ` `` or the `}` that ends a
    /// substitution. Stops after `${` or the closing backtick.
    fn lex_template_part(&mut self, start: usize) -> Result<TokenKind<'s>> {
        let mut cooked: Option<Vec<u16>> = Some(Vec::new());
        let mut raw: Vec<u16> = Vec::new();
        loop {
            let Some(c) = self.peek() else {
                return self.err("unterminated template literal", start);
            };
            match c {
                '`' => {
                    self.pos += 1;
                    return Ok(TokenKind::Template {
                        cooked: cooked.map(Vec::into_boxed_slice),
                        raw: raw.into_boxed_slice(),
                        tail: true,
                    });
                }
                '$' if self.byte_at(1) == Some(b'{') => {
                    self.pos += 2;
                    return Ok(TokenKind::Template {
                        cooked: cooked.map(Vec::into_boxed_slice),
                        raw: raw.into_boxed_slice(),
                        tail: false,
                    });
                }
                '\\' => {
                    let esc_start = self.pos;
                    self.pos += 1;
                    match self.lex_escape(esc_start, true)? {
                        Escape::Units(u) => {
                            if let Some(ck) = cooked.as_mut() {
                                ck.extend_from_slice(u.as_slice());
                            }
                        }
                        Escape::LineContinuation => {}
                        Escape::LegacyOctal(_) | Escape::Invalid => {
                            cooked = None;
                            // Skip the rest of a malformed \u{ escape so the
                            // raw text stays in sync.
                        }
                    }
                    // Raw text: the source, with CRLF and CR normalised to LF.
                    let text = &self.src[esc_start..self.pos];
                    if text.contains('\r') {
                        let text = text.replace("\r\n", "\n").replace('\r', "\n");
                        raw.extend(text.encode_utf16());
                    } else {
                        source::push_units(&mut raw, text, esc_start, self.lone);
                    }
                }
                '\r' => {
                    self.pos += 1;
                    self.eat('\n');
                    raw.push(0x0A);
                    if let Some(ck) = cooked.as_mut() {
                        ck.push(0x0A);
                    }
                }
                c => {
                    let at = self.pos;
                    self.pos += c.len_utf8();
                    self.push_char(&mut raw, c, at);
                    if let Some(ck) = cooked.as_mut() {
                        self.push_char(ck, c, at);
                    }
                }
            }
        }
    }

    /// The parser saw `}` while inside a template substitution: rescan from
    /// just after the brace as a template continuation.
    pub fn rescan_template_continuation(&mut self, rbrace: &Token<'s>) -> Result<Token<'s>> {
        let start = rbrace.span.start as usize;
        self.pos = start + 1;
        let kind = self.lex_template_part(start)?;
        Ok(Token {
            kind,
            span: Span::new(start as u32, self.pos as u32),
            newline_before: rbrace.newline_before,
        })
    }

    /// The parser saw `/` or `/=` where an expression is expected: rescan
    /// as a regular expression literal.
    pub fn rescan_regexp(&mut self, slash: &Token<'s>) -> Result<Token<'s>> {
        let start = slash.span.start as usize;
        self.pos = start + 1;
        let mut in_class = false;
        loop {
            let Some(c) = self.bump() else {
                return self.err("unterminated regular expression literal", start);
            };
            if is_line_terminator(c) {
                return self.err("unterminated regular expression literal", start);
            }
            match c {
                '\\' => match self.bump() {
                    Some(c) if !is_line_terminator(c) => {}
                    _ => return self.err("unterminated regular expression literal", start),
                },
                '[' => in_class = true,
                ']' => in_class = false,
                '/' if !in_class => break,
                _ => {}
            }
        }
        let pattern = source::units_of(self.src, start + 1, self.pos - 1, self.lone);
        let flags_start = self.pos;
        while let Some(c) = self.peek() {
            if c == '\\' {
                return self.err("invalid regular expression flags", flags_start);
            }
            if is_id_continue(c) {
                self.pos += c.len_utf8();
            } else {
                break;
            }
        }
        let flags = &self.src[flags_start..self.pos];
        Ok(Token {
            kind: TokenKind::RegExp {
                pattern: pattern.into_boxed_slice(),
                flags: flags.into(),
            },
            span: Span::new(start as u32, self.pos as u32),
            newline_before: slash.newline_before,
        })
    }
}

/// The one or two UTF-16 code units an escape stands for.
struct Units {
    buf: [u16; 2],
    len: u8,
}

impl Units {
    fn one(u: u16) -> Units {
        Units { buf: [u, 0], len: 1 }
    }

    fn as_slice(&self) -> &[u16] {
        &self.buf[..self.len as usize]
    }
}

enum Escape {
    Units(Units),
    LineContinuation,
    LegacyOctal(u16),
    Invalid,
}

fn parse_radix(digits: &str, radix: u32) -> f64 {
    let mut v = 0f64;
    for c in digits.chars() {
        v = v * radix as f64 + c.to_digit(radix).unwrap() as f64;
    }
    v
}

/// Convert digits in `radix` to a decimal string (arbitrary precision).
pub fn radix_to_decimal(digits: &str, radix: u32) -> String {
    // Schoolbook: keep the value as little-endian base-1e9 limbs.
    let mut limbs: Vec<u32> = vec![0];
    for c in digits.chars() {
        let d = c.to_digit(radix).unwrap();
        let mut carry = d as u64;
        for limb in limbs.iter_mut() {
            let v = *limb as u64 * radix as u64 + carry;
            *limb = (v % 1_000_000_000) as u32;
            carry = v / 1_000_000_000;
        }
        while carry > 0 {
            limbs.push((carry % 1_000_000_000) as u32);
            carry /= 1_000_000_000;
        }
    }
    let mut out = String::new();
    for (i, limb) in limbs.iter().rev().enumerate() {
        if i == 0 {
            out.push_str(&limb.to_string());
        } else {
            out.push_str(&format!("{limb:09}"));
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lex_all(src: &str) -> Vec<TokenKind<'_>> {
        let mut lx = Lexer::new(src, false);
        let mut out = Vec::new();
        loop {
            let t = lx.next_token().unwrap();
            if t.kind == TokenKind::Eof {
                break;
            }
            out.push(t.kind);
        }
        out
    }

    #[test]
    fn punctuators_and_identifiers() {
        let toks = lex_all("a >>>= b ?. c ?.5 \\u0061wait");
        assert!(matches!(&toks[0], TokenKind::Ident { name, escaped: false } if &**name == "a"));
        assert_eq!(toks[1], TokenKind::Punct(Punct::UShrEq));
        assert_eq!(toks[3], TokenKind::Punct(Punct::QuestionDot));
        assert_eq!(toks[5], TokenKind::Punct(Punct::Question));
        assert!(matches!(&toks[6], TokenKind::Number { value, .. } if *value == 0.5));
        assert!(matches!(&toks[7], TokenKind::Ident { name, escaped: true } if &**name == "await"));
    }

    #[test]
    fn numbers() {
        let toks = lex_all("0x1F 0b1_0 1_000.5e3 017 089 .5 123n 0o7n");
        assert!(
            matches!(toks[0], TokenKind::Number { value, kind: NumberKind::Hex } if value == 31.0)
        );
        assert!(
            matches!(toks[1], TokenKind::Number { value, kind: NumberKind::Binary } if value == 2.0)
        );
        assert!(matches!(toks[2], TokenKind::Number { value, .. } if value == 1000500.0));
        assert!(
            matches!(toks[3], TokenKind::Number { value, kind: NumberKind::LegacyOctal } if value == 15.0)
        );
        assert!(
            matches!(toks[4], TokenKind::Number { value, kind: NumberKind::LegacyDecimal } if value == 89.0)
        );
        assert!(matches!(toks[5], TokenKind::Number { value, .. } if value == 0.5));
        assert!(matches!(&toks[6], TokenKind::BigInt(d) if &**d == "123"));
        assert!(matches!(&toks[7], TokenKind::BigInt(d) if &**d == "7"));
        assert!(Lexer::new("1__0", false).next_token().is_err());
        assert!(Lexer::new("1_", false).next_token().is_err());
        assert!(Lexer::new("3in", false).next_token().is_err());
        assert!(Lexer::new("1.5n", false).next_token().is_err());
        assert_eq!(radix_to_decimal("ff", 16), "255");
        assert_eq!(
            radix_to_decimal("10000000000000000000000", 16),
            "309485009821345068724781056"
        );
    }

    /// The lexer's fast paths (ASCII identifiers, separator-free decimals,
    /// escape-free strings) and its slow paths agree.
    #[test]
    fn fast_and_slow_paths_agree() {
        let toks = lex_all("abc a\\u0062c caf\u{e9} \u{e9}t\u{e9} a$_1 _ $");
        let names: Vec<(&str, bool)> = toks
            .iter()
            .map(|t| match t {
                TokenKind::Ident { name, escaped } => (&**name, *escaped),
                _ => panic!("not an identifier: {t:?}"),
            })
            .collect();
        assert_eq!(
            names,
            [
                ("abc", false),
                ("abc", true),
                ("caf\u{e9}", false),
                ("\u{e9}t\u{e9}", false),
                ("a$_1", false),
                ("_", false),
                ("$", false)
            ]
        );
        let toks = lex_all("5. 1e+5 1E-2 .5e1 1_0.0_1e1_0 0 0.0 123456789012345678901 1e400 0e0");
        let values: Vec<f64> = toks
            .iter()
            .map(|t| match t {
                TokenKind::Number { value, kind: NumberKind::Decimal } => *value,
                _ => panic!("not a decimal: {t:?}"),
            })
            .collect();
        assert_eq!(
            values,
            [5.0, 1e5, 1e-2, 5.0, 10.01e10, 0.0, 0.0, 123456789012345678901.0, f64::INFINITY, 0.0]
        );
        assert!(matches!(&lex_all("1_000n")[0], TokenKind::BigInt(d) if &**d == "1000"));
        assert!(matches!(&lex_all("0n")[0], TokenKind::BigInt(d) if &**d == "0"));
        for bad in ["1e", "1e+", "1._5", "1_.5", "1.5_", "1e_5", "0_1", "5.n", "1e5n"] {
            assert!(Lexer::new(bad, false).next_token().is_err(), "{bad} should not lex");
        }
        let units = |s: &str| s.encode_utf16().collect::<Vec<u16>>();
        let toks = lex_all("'plain' \"caf\u{e9}\" 'a\\'b' \"\\u{1F600}\\x41\" ''");
        let strings: Vec<Vec<u16>> = toks
            .iter()
            .map(|t| match t {
                TokenKind::String { value, .. } => value.to_vec(),
                _ => panic!("not a string: {t:?}"),
            })
            .collect();
        assert_eq!(
            strings,
            [units("plain"), units("caf\u{e9}"), units("a'b"), units("\u{1F600}A"), vec![]]
        );
        assert!(Lexer::new("'abc", false).next_token().is_err());
        assert!(Lexer::new("'ab\nc'", false).next_token().is_err());
    }

    #[test]
    fn strings_and_templates() {
        let toks = lex_all(r#"'a\u{1F600}b' "\x41\n" `x` "\07""#);
        assert!(
            matches!(&toks[0], TokenKind::String { value, legacy_octal: false } if value.len() == 4)
        );
        assert!(matches!(&toks[1], TokenKind::String { value, .. } if **value == [0x41, 0x0A]));
        assert!(matches!(&toks[2], TokenKind::Template { tail: true, raw, .. } if **raw == [0x78]));
        // after `${`, the parser lexes `1` then sees `}`; rescanning is the parser's job.
        assert!(matches!(
            &toks[3],
            TokenKind::String {
                legacy_octal: true,
                ..
            }
        ));
    }

    #[test]
    fn template_continuation_and_regexp_rescan() {
        let mut lx = Lexer::new("`a${x}b`", false);
        let t = lx.next_token().unwrap();
        assert!(matches!(t.kind, TokenKind::Template { tail: false, .. }));
        let _x = lx.next_token().unwrap();
        let rb = lx.next_token().unwrap();
        assert_eq!(rb.kind, TokenKind::Punct(Punct::RBrace));
        let cont = lx.rescan_template_continuation(&rb).unwrap();
        assert!(matches!(cont.kind, TokenKind::Template { tail: true, raw, .. } if *raw == [0x62]));

        let mut lx = Lexer::new("/[/]a\\/b/gi", false);
        let slash = lx.next_token().unwrap();
        let re = lx.rescan_regexp(&slash).unwrap();
        let units: Vec<u16> = "[/]a\\/b".encode_utf16().collect();
        assert!(
            matches!(re.kind, TokenKind::RegExp { pattern, flags } if *pattern == *units && &*flags == "gi")
        );
    }

    #[test]
    fn comments_and_newlines() {
        let mut lx = Lexer::new("a /* x\n */ b // c\n<!-- html\n--> also html\nc", false);
        let a = lx.next_token().unwrap();
        assert!(!a.newline_before);
        let b = lx.next_token().unwrap();
        assert!(b.newline_before);
        let c = lx.next_token().unwrap();
        assert!(matches!(c.kind, TokenKind::Ident { name, .. } if &*name == "c"));
        assert!(Lexer::new("#!/bin/sh\n1", false).next_token().unwrap().kind != TokenKind::Eof);
    }
}
