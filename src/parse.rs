//! The parser for the textual form printed by `Module`'s `Display`.
//!
//! The parser is iterative (the grammar has no nesting beyond one level of braces),
//! linear in the input, and bounded: the input size is capped, and entity ids are
//! capped relative to the input size, so a short input cannot make it allocate a
//! huge table. It reads syntax only; [`Module::validate`](crate::Module::validate)
//! checks meaning. Ids in the text are kept exactly, so a printed module parses
//! back to the same handles.

use alloc::boxed::Box;
use alloc::collections::BTreeMap;
use alloc::string::{String, ToString};
use alloc::vec::Vec;
use core::fmt;

use crate::entity::{
    AggId, Block, FuncId, GlobalId, Inst, NONE, SigId, StackSlot, Value, ValueRange,
};
use crate::error::ModuleError;
use crate::function::{BlockNode, Function, MAX_ENTITIES, SlotData, ValueDef, ValueNode};
use crate::inst::{
    Attrs, BlockArg, BlockCall, Call, CallTarget, InstData, MemFlags, MemOrder, RmwOp, SwitchCase,
    Terminator,
};
use crate::module::{Global, Init, Linkage, Module, Reloc, SymbolRef};
use crate::ops::{BinaryOp, CmpOp, ConvOp, DivZero, FloatToInt, Overflow, Policy, Shift, UnaryOp};
use crate::types::{Aggregate, CallConv, MemType, PointerWidth, Signature, Type};

/// Budgets for [`parse_with`].
///
/// # Examples
///
/// ```
/// use ir_lang::{ParseErrorKind, ParseLimits};
///
/// let tiny = ParseLimits { max_bytes: 8, ..ParseLimits::default() };
/// let err = ir_lang::parse_with("module \"m\" ptr64\n", &tiny).unwrap_err();
/// assert_eq!(err.kind(), &ParseErrorKind::InputTooLarge { limit: 8 });
/// ```
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct ParseLimits {
    /// The largest input accepted, in bytes (default 256 MiB; at most `u32::MAX`).
    pub max_bytes: usize,
    /// The largest entity id (value, block, ...) accepted (default 2^26). Ids are
    /// also capped at `64 * input length + 4096`, so a short input cannot make the
    /// parser allocate a large table.
    pub max_id: u32,
}

impl Default for ParseLimits {
    fn default() -> Self {
        ParseLimits {
            max_bytes: 256 << 20,
            max_id: 1 << 26,
        }
    }
}

/// What went wrong in a [`ParseError`].
#[derive(Clone, PartialEq, Eq, Debug)]
#[non_exhaustive]
pub enum ParseErrorKind {
    /// The input exceeds [`ParseLimits::max_bytes`].
    InputTooLarge {
        /// The limit.
        limit: usize,
    },
    /// A character that starts no token.
    UnexpectedChar {
        /// The character.
        found: char,
    },
    /// A string literal is not closed.
    UnterminatedString,
    /// A string literal has an invalid escape.
    BadEscape,
    /// A different token was expected.
    Expected {
        /// What was expected.
        expected: &'static str,
        /// What was found.
        found: String,
    },
    /// An unknown instruction or terminator name.
    UnknownOpcode {
        /// The name.
        name: String,
    },
    /// An unknown type name.
    UnknownType {
        /// The name.
        name: String,
    },
    /// A symbol that no `func` or `global` declares.
    UnknownSymbol {
        /// The name.
        name: String,
    },
    /// A malformed or out-of-range number.
    BadNumber {
        /// The text.
        text: String,
    },
    /// An entity id above the limits.
    IdTooLarge {
        /// The id.
        id: u64,
    },
    /// Aggregates, signatures, or stack slots must be numbered 0, 1, 2, ... in order.
    OutOfOrder {
        /// What is out of order.
        what: &'static str,
    },
    /// Something is defined twice.
    Duplicate {
        /// What.
        what: String,
    },
    /// Something is used but never defined.
    Undefined {
        /// What.
        what: String,
    },
    /// The results of one instruction must have consecutive ids.
    NonConsecutiveResults,
    /// An instruction needs a typed result that is missing.
    MissingResult,
    /// The module refused a declaration.
    Module(ModuleError),
}

impl fmt::Display for ParseErrorKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        use ParseErrorKind as K;
        match self {
            K::InputTooLarge { limit } => write!(f, "input exceeds {limit} bytes"),
            K::UnexpectedChar { found } => write!(f, "unexpected character {found:?}"),
            K::UnterminatedString => f.write_str("unterminated string"),
            K::BadEscape => f.write_str("invalid escape in string"),
            K::Expected { expected, found } => write!(f, "expected {expected}, found {found}"),
            K::UnknownOpcode { name } => write!(f, "unknown instruction `{name}`"),
            K::UnknownType { name } => write!(f, "unknown type `{name}`"),
            K::UnknownSymbol { name } => write!(f, "undeclared symbol `@{name}`"),
            K::BadNumber { text } => write!(f, "invalid number `{text}`"),
            K::IdTooLarge { id } => write!(f, "id {id} exceeds the parser's limits"),
            K::OutOfOrder { what } => write!(f, "{what} must be numbered in order from 0"),
            K::Duplicate { what } => write!(f, "{what} is defined twice"),
            K::Undefined { what } => write!(f, "{what} is used but never defined"),
            K::NonConsecutiveResults => {
                f.write_str("the results of an instruction must have consecutive ids")
            }
            K::MissingResult => f.write_str("this instruction needs a typed result"),
            K::Module(e) => write!(f, "{e}"),
        }
    }
}

/// Why text could not be parsed, and where (1-based line and column, in
/// characters).
///
/// # Examples
///
/// ```
/// use ir_lang::ParseErrorKind;
///
/// let err = ir_lang::parse("module \"m\" ptr64\nwidget\n").unwrap_err();
/// assert_eq!((err.line(), err.column()), (2, 1));
/// assert!(matches!(err.kind(), ParseErrorKind::Expected { .. }));
/// ```
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct ParseError {
    line: u32,
    column: u32,
    kind: ParseErrorKind,
}

impl ParseError {
    /// The 1-based line.
    ///
    /// # Examples
    ///
    /// ```
    /// let err = ir_lang::parse("module \"m\" ptr64\n\n  oops\n").unwrap_err();
    /// assert_eq!(err.line(), 3);
    /// ```
    #[must_use]
    pub fn line(&self) -> u32 {
        self.line
    }

    /// The 1-based column, in characters.
    ///
    /// # Examples
    ///
    /// ```
    /// let err = ir_lang::parse("module \"m\" ptr64\n\n  oops\n").unwrap_err();
    /// assert_eq!(err.column(), 3);
    /// ```
    #[must_use]
    pub fn column(&self) -> u32 {
        self.column
    }

    /// What went wrong.
    ///
    /// # Examples
    ///
    /// ```
    /// use ir_lang::ParseErrorKind;
    ///
    /// let err = ir_lang::parse("module \"m\" ptr64\n\n  oops\n").unwrap_err();
    /// assert!(matches!(err.kind(), ParseErrorKind::Expected { .. }));
    /// ```
    #[must_use]
    pub fn kind(&self) -> &ParseErrorKind {
        &self.kind
    }
}

impl fmt::Display for ParseError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}:{}: {}", self.line, self.column, self.kind)
    }
}

impl core::error::Error for ParseError {}

/// Parses the textual form of a module with the default [`ParseLimits`].
///
/// The result is not validated; call [`Module::validate`](crate::Module::validate).
/// `parse(&m.to_string())` reproduces `m` with the same handles.
///
/// # Errors
///
/// A [`ParseError`] at the first problem.
///
/// # Examples
///
/// ```
/// let text = "module \"m\" ptr64\nsig s0 = (i32) -> (i32) fast\n\nfunc @id s0 export {\nb0(v0: i32):\n    ret v0\n}\n";
/// let m = ir_lang::parse(text)?;
/// m.validate()?;
/// assert_eq!(m.to_string(), text);
/// # Ok::<(), Box<dyn std::error::Error>>(())
/// ```
pub fn parse(text: &str) -> Result<Module, ParseError> {
    parse_with(text, &ParseLimits::default())
}

/// Parses the textual form of a module with explicit budgets.
///
/// # Errors
///
/// A [`ParseError`] at the first problem, including
/// [`ParseErrorKind::InputTooLarge`] and [`ParseErrorKind::IdTooLarge`] when a
/// budget is exceeded.
///
/// See [`ParseLimits`] for an example.
pub fn parse_with(text: &str, limits: &ParseLimits) -> Result<Module, ParseError> {
    let max = limits.max_bytes.min(u32::MAX as usize);
    if text.len() > max {
        return Err(ParseError {
            line: 1,
            column: 1,
            kind: ParseErrorKind::InputTooLarge {
                limit: limits.max_bytes,
            },
        });
    }
    let toks = lex(text)?;
    let relative = (text.len() as u64).saturating_mul(64).saturating_add(4096);
    let id_cap = u64::from(limits.max_id)
        .min(relative)
        .min(u64::from(MAX_ENTITIES - 1)) as u32;
    let mut p = Parser {
        src: text,
        toks,
        pos: 0,
        id_cap,
        funcs: BTreeMap::new(),
        globals: BTreeMap::new(),
        results: Vec::new(),
    };
    p.module()
}

// ------------------------------------------------------------------------ lexer

#[derive(Clone, Debug, PartialEq)]
enum Tok<'a> {
    Ident(&'a str),
    Num(&'a str),
    Str(String),
    Sym(String),
    Punct(u8),
    Arrow,
    Eof,
}

#[derive(Clone, Debug)]
struct Token<'a> {
    tok: Tok<'a>,
    start: u32,
}

fn error_at(src: &str, offset: usize, kind: ParseErrorKind) -> ParseError {
    let before = src.get(..offset).unwrap_or(src);
    let line = before.bytes().filter(|&b| b == b'\n').count() as u32 + 1;
    let line_start = before.rfind('\n').map_or(0, |i| i + 1);
    let column = before.get(line_start..).map_or(0, |s| s.chars().count()) as u32 + 1;
    ParseError { line, column, kind }
}

fn is_word_start(b: u8) -> bool {
    b.is_ascii_alphabetic() || b == b'_'
}

fn is_word(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b == b'_' || b == b'.' || b == b'$'
}

fn lex_string(src: &str, open: usize) -> Result<(String, usize), ParseError> {
    let bytes = src.as_bytes();
    let mut out = String::new();
    let mut i = open + 1;
    loop {
        let Some(rest) = src.get(i..) else {
            return Err(error_at(src, open, ParseErrorKind::UnterminatedString));
        };
        let Some(c) = rest.chars().next() else {
            return Err(error_at(src, open, ParseErrorKind::UnterminatedString));
        };
        match c {
            '"' => return Ok((out, i + 1)),
            '\\' => {
                let esc = bytes.get(i + 1).copied();
                match esc {
                    Some(b'\\') => out.push('\\'),
                    Some(b'"') => out.push('"'),
                    Some(b'n') => out.push('\n'),
                    Some(b't') => out.push('\t'),
                    Some(b'r') => out.push('\r'),
                    Some(b'u') => {
                        if bytes.get(i + 2) != Some(&b'{') {
                            return Err(error_at(src, i, ParseErrorKind::BadEscape));
                        }
                        let close = src
                            .get(i + 3..)
                            .and_then(|s| s.find('}'))
                            .ok_or_else(|| error_at(src, i, ParseErrorKind::BadEscape))?;
                        let hex = src.get(i + 3..i + 3 + close).unwrap_or("");
                        let ch = (hex.len() <= 6)
                            .then(|| u32::from_str_radix(hex, 16).ok())
                            .flatten()
                            .and_then(char::from_u32)
                            .ok_or_else(|| error_at(src, i, ParseErrorKind::BadEscape))?;
                        out.push(ch);
                        i += 3 + close + 1;
                        continue;
                    }
                    _ => return Err(error_at(src, i, ParseErrorKind::BadEscape)),
                }
                i += 2;
            }
            c => {
                out.push(c);
                i += c.len_utf8();
            }
        }
    }
}

fn lex(src: &str) -> Result<Vec<Token<'_>>, ParseError> {
    let bytes = src.as_bytes();
    let mut toks = Vec::new();
    let mut i = 0usize;
    while let Some(&b) = bytes.get(i) {
        let start = i as u32;
        match b {
            b' ' | b'\t' | b'\r' | b'\n' => i += 1,
            b';' => {
                while bytes.get(i).is_some_and(|&c| c != b'\n') {
                    i += 1;
                }
            }
            b'"' => {
                let (s, end) = lex_string(src, i)?;
                toks.push(Token {
                    tok: Tok::Str(s),
                    start,
                });
                i = end;
            }
            b'@' => {
                if bytes.get(i + 1) == Some(&b'"') {
                    let (s, end) = lex_string(src, i + 1)?;
                    toks.push(Token {
                        tok: Tok::Sym(s),
                        start,
                    });
                    i = end;
                } else {
                    let mut j = i + 1;
                    while bytes.get(j).is_some_and(|&c| is_word(c)) {
                        j += 1;
                    }
                    let name = src.get(i + 1..j).unwrap_or("");
                    if name.is_empty() {
                        return Err(error_at(
                            src,
                            i,
                            ParseErrorKind::UnexpectedChar { found: '@' },
                        ));
                    }
                    toks.push(Token {
                        tok: Tok::Sym(name.to_string()),
                        start,
                    });
                    i = j;
                }
            }
            b'-' if bytes.get(i + 1) == Some(&b'>') => {
                toks.push(Token {
                    tok: Tok::Arrow,
                    start,
                });
                i += 2;
            }
            b'-' | b'+' if bytes.get(i + 1).is_some_and(u8::is_ascii_digit) => {
                let end = number_end(bytes, i + 1);
                toks.push(Token {
                    tok: Tok::Num(src.get(i..end).unwrap_or("")),
                    start,
                });
                i = end;
            }
            b'0'..=b'9' => {
                let end = number_end(bytes, i);
                toks.push(Token {
                    tok: Tok::Num(src.get(i..end).unwrap_or("")),
                    start,
                });
                i = end;
            }
            b if is_word_start(b) => {
                let mut j = i + 1;
                while bytes.get(j).is_some_and(|&c| is_word(c)) {
                    j += 1;
                }
                toks.push(Token {
                    tok: Tok::Ident(src.get(i..j).unwrap_or("")),
                    start,
                });
                i = j;
            }
            b'(' | b')' | b'{' | b'}' | b'[' | b']' | b'<' | b'>' | b',' | b':' | b'=' | b'-' => {
                toks.push(Token {
                    tok: Tok::Punct(b),
                    start,
                });
                i += 1;
            }
            _ => {
                let found = src.get(i..).and_then(|s| s.chars().next()).unwrap_or('?');
                return Err(error_at(src, i, ParseErrorKind::UnexpectedChar { found }));
            }
        }
    }
    toks.push(Token {
        tok: Tok::Eof,
        start: src.len() as u32,
    });
    Ok(toks)
}

/// The end of a number starting at `i`: digits, letters, `_` and `.`, and a sign
/// directly after a decimal exponent marker.
fn number_end(bytes: &[u8], mut i: usize) -> usize {
    let hex = bytes.get(i) == Some(&b'0') && matches!(bytes.get(i + 1), Some(b'x' | b'X'));
    while let Some(&c) = bytes.get(i) {
        let word = c.is_ascii_alphanumeric() || c == b'_' || c == b'.';
        let exponent_sign = (c == b'+' || c == b'-')
            && !hex
            && matches!(bytes.get(i.wrapping_sub(1)), Some(b'e' | b'E'));
        if !(word || exponent_sign) {
            break;
        }
        i += 1;
    }
    i
}

// ----------------------------------------------------------------------- parser

struct Parser<'a> {
    src: &'a str,
    toks: Vec<Token<'a>>,
    pos: usize,
    id_cap: u32,
    funcs: BTreeMap<String, FuncId>,
    globals: BTreeMap<String, GlobalId>,
    /// Reused per instruction: (value id, type, offset) of each result.
    results: Vec<(u32, Type, u32)>,
}

type PResult<T> = Result<T, ParseError>;

fn describe(tok: &Tok<'_>) -> String {
    match tok {
        Tok::Ident(s) | Tok::Num(s) => format_tok(s),
        Tok::Str(_) => "a string".to_string(),
        Tok::Sym(s) => {
            let mut out = String::from("`@");
            out.push_str(s);
            out.push('`');
            out
        }
        Tok::Punct(c) => {
            let mut out = String::from("`");
            out.push(*c as char);
            out.push('`');
            out
        }
        Tok::Arrow => "`->`".to_string(),
        Tok::Eof => "end of input".to_string(),
    }
}

fn format_tok(s: &str) -> String {
    let mut out = String::from("`");
    out.push_str(s);
    out.push('`');
    out
}

/// Splits `prefix` + decimal digits.
fn entity_id(word: &str, prefix: &str) -> Option<u64> {
    let digits = word.strip_prefix(prefix)?;
    if digits.is_empty() || !digits.bytes().all(|b| b.is_ascii_digit()) || digits.len() > 12 {
        return None;
    }
    digits.parse().ok()
}

fn is_entity(word: &str, prefix: &str) -> bool {
    entity_id(word, prefix).is_some()
}

/// The pieces of a function body under construction.
struct Body {
    func: Function,
    /// Which values have been defined.
    defined: Vec<bool>,
    /// Which blocks have been defined.
    block_defined: Vec<bool>,
    value_refs: Vec<(u32, u32)>,
    block_refs: Vec<(u32, u32)>,
    first_block: Option<Block>,
}

impl<'a> Parser<'a> {
    fn err(&self, kind: ParseErrorKind) -> ParseError {
        let offset = self
            .toks
            .get(self.pos)
            .map_or(self.src.len(), |t| t.start as usize);
        error_at(self.src, offset, kind)
    }

    fn err_at(&self, offset: u32, kind: ParseErrorKind) -> ParseError {
        error_at(self.src, offset as usize, kind)
    }

    fn peek(&self) -> &Tok<'a> {
        self.toks.get(self.pos).map_or(&Tok::Eof, |t| &t.tok)
    }

    fn peek_at(&self, ahead: usize) -> &Tok<'a> {
        self.toks
            .get(self.pos + ahead)
            .map_or(&Tok::Eof, |t| &t.tok)
    }

    fn offset(&self) -> u32 {
        self.toks
            .get(self.pos)
            .map_or(self.src.len() as u32, |t| t.start)
    }

    fn bump(&mut self) {
        if self.pos < self.toks.len() {
            self.pos += 1;
        }
    }

    fn expected(&self, what: &'static str) -> ParseError {
        self.err(ParseErrorKind::Expected {
            expected: what,
            found: describe(self.peek()),
        })
    }

    fn eat_punct(&mut self, c: u8) -> bool {
        if *self.peek() == Tok::Punct(c) {
            self.bump();
            true
        } else {
            false
        }
    }

    fn punct(&mut self, c: u8, what: &'static str) -> PResult<()> {
        if self.eat_punct(c) {
            Ok(())
        } else {
            Err(self.expected(what))
        }
    }

    fn eat_word(&mut self, word: &str) -> bool {
        if *self.peek() == Tok::Ident(word) {
            self.bump();
            true
        } else {
            false
        }
    }

    fn word(&mut self, word: &'static str) -> PResult<()> {
        if self.eat_word(word) {
            Ok(())
        } else {
            Err(self.expected(word))
        }
    }

    fn ident(&mut self, what: &'static str) -> PResult<&'a str> {
        match *self.peek() {
            Tok::Ident(s) => {
                self.bump();
                Ok(s)
            }
            _ => Err(self.expected(what)),
        }
    }

    fn number(&mut self, what: &'static str) -> PResult<&'a str> {
        match *self.peek() {
            Tok::Num(s) => {
                self.bump();
                Ok(s)
            }
            _ => Err(self.expected(what)),
        }
    }

    fn u64(&mut self, what: &'static str) -> PResult<u64> {
        let at = self.offset();
        let text = self.number(what)?;
        parse_u64(text).ok_or_else(|| {
            self.err_at(
                at,
                ParseErrorKind::BadNumber {
                    text: text.to_string(),
                },
            )
        })
    }

    fn u32(&mut self, what: &'static str) -> PResult<u32> {
        let at = self.offset();
        let v = self.u64(what)?;
        u32::try_from(v).map_err(|_| {
            self.err_at(
                at,
                ParseErrorKind::BadNumber {
                    text: v.to_string(),
                },
            )
        })
    }

    /// An entity id like `v12`, bounded by the id cap.
    fn entity(&mut self, prefix: &str, what: &'static str) -> PResult<u32> {
        let at = self.offset();
        let word = self.ident(what)?;
        let id = entity_id(word, prefix).ok_or_else(|| {
            self.err_at(
                at,
                ParseErrorKind::Expected {
                    expected: what,
                    found: format_tok(word),
                },
            )
        })?;
        if id > u64::from(self.id_cap) {
            return Err(self.err_at(at, ParseErrorKind::IdTooLarge { id }));
        }
        Ok(id as u32)
    }

    fn ty(&mut self) -> PResult<Type> {
        let at = self.offset();
        let name = self.ident("a type")?;
        Type::from_name(name).ok_or_else(|| {
            self.err_at(
                at,
                ParseErrorKind::UnknownType {
                    name: name.to_string(),
                },
            )
        })
    }

    fn mem_type(&mut self) -> PResult<MemType> {
        if let Tok::Ident(w) = *self.peek() {
            if is_entity(w, "t") {
                let id = self.entity("t", "an aggregate")?;
                return Ok(MemType::Agg(AggId::from_u32(id)));
            }
        }
        Ok(MemType::Scalar(self.ty()?))
    }

    fn type_list(&mut self) -> PResult<Vec<Type>> {
        self.punct(b'(', "`(`")?;
        let mut out = Vec::new();
        if !self.eat_punct(b')') {
            loop {
                out.push(self.ty()?);
                if self.eat_punct(b')') {
                    break;
                }
                self.punct(b',', "`,` or `)`")?;
            }
        }
        Ok(out)
    }

    fn symbol(&mut self) -> PResult<(String, u32)> {
        let at = self.offset();
        match self.peek().clone() {
            Tok::Sym(s) => {
                self.bump();
                Ok((s, at))
            }
            _ => Err(self.expected("a symbol")),
        }
    }

    fn func_ref(&mut self) -> PResult<FuncId> {
        let (name, at) = self.symbol()?;
        self.funcs
            .get(&name)
            .copied()
            .ok_or_else(|| self.err_at(at, ParseErrorKind::UnknownSymbol { name }))
    }

    fn global_ref(&mut self) -> PResult<GlobalId> {
        let (name, at) = self.symbol()?;
        self.globals
            .get(&name)
            .copied()
            .ok_or_else(|| self.err_at(at, ParseErrorKind::UnknownSymbol { name }))
    }

    fn attrs(&mut self) -> PResult<Attrs> {
        let mut attrs = Attrs::NONE;
        if self.eat_punct(b'[') {
            loop {
                let at = self.offset();
                let name = self.ident("an attribute")?;
                attrs |= Attrs::from_name(name).ok_or_else(|| {
                    self.err_at(
                        at,
                        ParseErrorKind::Expected {
                            expected: "an attribute",
                            found: format_tok(name),
                        },
                    )
                })?;
                if self.eat_punct(b']') {
                    break;
                }
                self.punct(b',', "`,` or `]`")?;
            }
        }
        Ok(attrs)
    }

    fn named<T>(&mut self, what: &'static str, lookup: impl Fn(&str) -> Option<T>) -> PResult<T> {
        let at = self.offset();
        let name = self.ident(what)?;
        lookup(name).ok_or_else(|| {
            self.err_at(
                at,
                ParseErrorKind::Expected {
                    expected: what,
                    found: format_tok(name),
                },
            )
        })
    }

    // ------------------------------------------------------------- module level

    /// Collects the function and global names in order, so bodies and relocations
    /// can name symbols declared later.
    fn prescan(&mut self) -> PResult<()> {
        let mut depth = 0u32;
        let mut i = 0usize;
        while let Some(t) = self.toks.get(i) {
            match &t.tok {
                Tok::Punct(b'{') => depth += 1,
                Tok::Punct(b'}') => depth = depth.saturating_sub(1),
                Tok::Ident(kw @ ("func" | "global")) if depth == 0 => {
                    if let Some(Token {
                        tok: Tok::Sym(name),
                        start,
                    }) = self.toks.get(i + 1)
                    {
                        let dup = self.funcs.contains_key(name) || self.globals.contains_key(name);
                        if dup {
                            return Err(self.err_at(
                                *start,
                                ParseErrorKind::Module(ModuleError::DuplicateName {
                                    name: name.clone(),
                                }),
                            ));
                        }
                        if *kw == "func" {
                            let id = FuncId::from_u32(self.funcs.len() as u32);
                            let _ = self.funcs.insert(name.clone(), id);
                        } else {
                            let id = GlobalId::from_u32(self.globals.len() as u32);
                            let _ = self.globals.insert(name.clone(), id);
                        }
                    }
                }
                _ => {}
            }
            i += 1;
        }
        Ok(())
    }

    fn module(&mut self) -> PResult<Module> {
        self.prescan()?;
        self.word("module")?;
        let name = match self.peek().clone() {
            Tok::Str(s) => {
                self.bump();
                s
            }
            _ => return Err(self.expected("the module name (a string)")),
        };
        let ptr = match self.ident("`ptr64` or `ptr32`")? {
            "ptr64" => PointerWidth::P64,
            "ptr32" => PointerWidth::P32,
            _ => {
                self.pos -= 1;
                return Err(self.expected("`ptr64` or `ptr32`"));
            }
        };
        let mut m = Module::with_pointer_width(&name, ptr);
        loop {
            let at = self.offset();
            match *self.peek() {
                Tok::Eof => break,
                Tok::Ident("type") => self.aggregate_decl(&mut m)?,
                Tok::Ident("sig") => self.sig_decl(&mut m)?,
                Tok::Ident("global") => self.global_decl(&mut m)?,
                Tok::Ident("func") => self.func_decl(&mut m)?,
                _ => {
                    return Err(self.err_at(
                        at,
                        ParseErrorKind::Expected {
                            expected: "`type`, `sig`, `global`, or `func`",
                            found: describe(self.peek()),
                        },
                    ));
                }
            }
        }
        Ok(m)
    }

    fn aggregate_decl(&mut self, m: &mut Module) -> PResult<()> {
        self.bump();
        let at = self.offset();
        let id = self.entity("t", "an aggregate id")?;
        if id as usize != m.aggregate_count() {
            return Err(self.err_at(at, ParseErrorKind::OutOfOrder { what: "aggregates" }));
        }
        self.punct(b'=', "`=`")?;
        let agg = if self.eat_punct(b'[') {
            let elem = self.mem_type()?;
            self.word("x")?;
            let len = self.u64("the array length")?;
            self.punct(b']', "`]`")?;
            Aggregate::Array { elem, len }
        } else {
            let packed = self.eat_word("packed");
            self.word("struct")?;
            self.punct(b'{', "`{`")?;
            let mut fields = Vec::new();
            if !self.eat_punct(b'}') {
                loop {
                    fields.push(self.mem_type()?);
                    if self.eat_punct(b'}') {
                        break;
                    }
                    self.punct(b',', "`,` or `}`")?;
                }
            }
            Aggregate::Struct { fields, packed }
        };
        let _ = m
            .declare_aggregate(agg)
            .map_err(|e| self.err_at(at, ParseErrorKind::Module(e)))?;
        Ok(())
    }

    fn sig_decl(&mut self, m: &mut Module) -> PResult<()> {
        self.bump();
        let at = self.offset();
        let id = self.entity("s", "a signature id")?;
        if id as usize != m.signature_count() {
            return Err(self.err_at(at, ParseErrorKind::OutOfOrder { what: "signatures" }));
        }
        self.punct(b'=', "`=`")?;
        let params = self.type_list()?;
        if *self.peek() != Tok::Arrow {
            return Err(self.expected("`->`"));
        }
        self.bump();
        let returns = self.type_list()?;
        let conv = self.named("a calling convention", CallConv::from_name)?;
        let sig = Signature {
            params,
            returns,
            conv,
        };
        let got = m
            .intern_signature(&sig)
            .map_err(|e| self.err_at(at, ParseErrorKind::Module(e)))?;
        if got.as_u32() != id {
            return Err(self.err_at(
                at,
                ParseErrorKind::Duplicate {
                    what: "signature".to_string(),
                },
            ));
        }
        Ok(())
    }

    fn global_decl(&mut self, m: &mut Module) -> PResult<()> {
        self.bump();
        let (name, at) = self.symbol()?;
        let linkage = self.named("a linkage", Linkage::from_name)?;
        let mutable = match self.ident("`const` or `mutable`")? {
            "const" => false,
            "mutable" => true,
            _ => {
                self.pos -= 1;
                return Err(self.expected("`const` or `mutable`"));
            }
        };
        self.word("size")?;
        let size = self.u64("the size")?;
        self.word("align")?;
        let align = self.u32("the alignment")?;
        let init = if self.eat_word("zeroed") {
            Init::Zeroed
        } else if self.eat_word("bytes") {
            let hat = self.offset();
            let hex = match self.peek().clone() {
                Tok::Str(s) => {
                    self.bump();
                    s
                }
                _ => return Err(self.expected("a hex string")),
            };
            Init::Bytes(
                parse_hex(&hex)
                    .ok_or_else(|| self.err_at(hat, ParseErrorKind::BadNumber { text: hex }))?,
            )
        } else {
            return Err(self.expected("`zeroed` or `bytes`"));
        };
        let mut relocs = Vec::new();
        while self.eat_word("reloc") {
            let offset = self.u64("the relocation offset")?;
            let (target_name, tat) = self.symbol()?;
            let target = if let Some(&f) = self.funcs.get(&target_name) {
                SymbolRef::Func(f)
            } else if let Some(&g) = self.globals.get(&target_name) {
                SymbolRef::Global(g)
            } else {
                return Err(self.err_at(tat, ParseErrorKind::UnknownSymbol { name: target_name }));
            };
            let nat = self.offset();
            let text = self.number("the addend")?;
            let addend = parse_i64(text).ok_or_else(|| {
                self.err_at(
                    nat,
                    ParseErrorKind::BadNumber {
                        text: text.to_string(),
                    },
                )
            })?;
            relocs.push(Reloc {
                offset,
                target,
                addend,
            });
        }
        let global = Global {
            linkage,
            size,
            align,
            mutable,
            init,
            relocs,
        };
        let _ = m
            .declare_global(&name, global)
            .map_err(|e| self.err_at(at, ParseErrorKind::Module(e)))?;
        Ok(())
    }

    fn func_decl(&mut self, m: &mut Module) -> PResult<()> {
        self.bump();
        let (name, at) = self.symbol()?;
        let sat = self.offset();
        let sig_id = self.entity("s", "a signature id")?;
        let sig = m
            .signature(SigId::from_u32(sig_id))
            .cloned()
            .ok_or_else(|| {
                self.err_at(
                    sat,
                    ParseErrorKind::Undefined {
                        what: SigId::from_u32(sig_id).to_string(),
                    },
                )
            })?;
        let linkage = self.named("a linkage", Linkage::from_name)?;
        let attrs = self.attrs()?;
        let id = m
            .declare_function(&name, &sig, linkage)
            .map_err(|e| self.err_at(at, ParseErrorKind::Module(e)))?;
        m.set_attrs(id, attrs)
            .map_err(|e| self.err_at(at, ParseErrorKind::Module(e)))?;
        if *self.peek() == Tok::Punct(b'{') {
            let body = self.body()?;
            m.set_body(id, body);
        }
        Ok(())
    }

    // ---------------------------------------------------------------- bodies

    fn body(&mut self) -> PResult<Function> {
        self.punct(b'{', "`{`")?;
        let mut body = Body {
            func: Function::empty(),
            defined: Vec::new(),
            block_defined: Vec::new(),
            value_refs: Vec::new(),
            block_refs: Vec::new(),
            first_block: None,
        };
        // Stack slots.
        while let Tok::Ident(w) = *self.peek() {
            if !is_entity(w, "ss") {
                break;
            }
            let at = self.offset();
            let id = self.entity("ss", "a stack slot")?;
            if id as usize != body.func.slots.len() {
                return Err(self.err_at(
                    at,
                    ParseErrorKind::OutOfOrder {
                        what: "stack slots",
                    },
                ));
            }
            self.punct(b'=', "`=`")?;
            self.word("slot")?;
            let size = self.u32("the slot size")?;
            self.word("align")?;
            let align = self.u32("the slot alignment")?;
            body.func.slots.push(SlotData { size, align });
        }
        // Blocks.
        loop {
            match *self.peek() {
                Tok::Punct(b'}') => {
                    self.bump();
                    break;
                }
                Tok::Ident(w) if is_entity(w, "b") => self.block(&mut body)?,
                _ => return Err(self.expected("a block or `}`")),
            }
        }
        self.finish_body(body)
    }

    fn finish_body(&self, mut body: Body) -> PResult<Function> {
        for &(id, at) in &body.value_refs {
            if !body.defined.get(id as usize).copied().unwrap_or(false) {
                return Err(self.err_at(
                    at,
                    ParseErrorKind::Undefined {
                        what: Value::from_u32(id).to_string(),
                    },
                ));
            }
        }
        for &(id, at) in &body.block_refs {
            if !body
                .block_defined
                .get(id as usize)
                .copied()
                .unwrap_or(false)
            {
                return Err(self.err_at(
                    at,
                    ParseErrorKind::Undefined {
                        what: Block::from_u32(id).to_string(),
                    },
                ));
            }
        }
        body.func.entry = body.first_block.unwrap_or(Block::from_u32(0));
        Ok(body.func)
    }

    fn define_value(
        &mut self,
        body: &mut Body,
        id: u32,
        ty: Type,
        def: ValueDef,
        at: u32,
    ) -> PResult<Value> {
        let index = id as usize;
        if body.func.values.len() <= index {
            body.func.values.resize(
                index + 1,
                ValueNode {
                    ty: Type::Bool,
                    def: ValueDef::Detached,
                    alias: NONE,
                },
            );
            body.defined.resize(index + 1, false);
        }
        if core::mem::replace(&mut body.defined[index], true) {
            return Err(self.err_at(
                at,
                ParseErrorKind::Duplicate {
                    what: Value::from_u32(id).to_string(),
                },
            ));
        }
        body.func.values[index] = ValueNode {
            ty,
            def,
            alias: NONE,
        };
        Ok(Value::from_u32(id))
    }

    fn block(&mut self, body: &mut Body) -> PResult<()> {
        let at = self.offset();
        let id = self.entity("b", "a block")?;
        let index = id as usize;
        if body.func.blocks.len() <= index {
            while body.func.blocks.len() <= index {
                let mut dead = BlockNode::new(Vec::new());
                dead.live = false;
                body.func.blocks.push(dead);
            }
            body.block_defined.resize(index + 1, false);
        }
        if core::mem::replace(&mut body.block_defined[index], true) {
            return Err(self.err_at(
                at,
                ParseErrorKind::Duplicate {
                    what: Block::from_u32(id).to_string(),
                },
            ));
        }
        let block = Block::from_u32(id);
        body.func.blocks[index].live = true;
        if body.first_block.is_none() {
            body.first_block = Some(block);
        }
        let mut params = Vec::new();
        if self.eat_punct(b'(') {
            loop {
                let vat = self.offset();
                let v = self.entity("v", "a value")?;
                self.punct(b':', "`:`")?;
                let ty = self.ty()?;
                let def = ValueDef::Param {
                    block,
                    index: params.len() as u32,
                };
                params.push(self.define_value(body, v, ty, def, vat)?);
                if self.eat_punct(b')') {
                    break;
                }
                self.punct(b',', "`,` or `)`")?;
            }
        }
        body.func.blocks[index].params = params;
        self.punct(b':', "`:`")?;
        // Statements until the next block or the end of the body. The terminator,
        // if any, is the last statement of its block.
        loop {
            let terminated = body.func.blocks[index].term.is_some();
            match *self.peek() {
                Tok::Punct(b'}') => return Ok(()),
                Tok::Ident(w) if is_entity(w, "b") => return Ok(()),
                _ if terminated => return Err(self.expected("a block or `}` after the terminator")),
                Tok::Ident(w) if is_entity(w, "v") && *self.peek_at(1) == Tok::Punct(b':') => {
                    self.instruction(body, block)?;
                }
                Tok::Ident(w) => match self.try_terminator(body, w)? {
                    Some(term) => body.func.blocks[index].term = Some(term),
                    None => self.instruction(body, block)?,
                },
                _ => return Err(self.expected("an instruction")),
            }
        }
    }

    fn value(&mut self, body: &mut Body) -> PResult<Value> {
        let at = self.offset();
        let id = self.entity("v", "a value")?;
        body.value_refs.push((id, at));
        Ok(Value::from_u32(id))
    }

    fn values_in_parens(&mut self, body: &mut Body) -> PResult<Vec<Value>> {
        self.punct(b'(', "`(`")?;
        let mut out = Vec::new();
        if !self.eat_punct(b')') {
            loop {
                out.push(self.value(body)?);
                if self.eat_punct(b')') {
                    break;
                }
                self.punct(b',', "`,` or `)`")?;
            }
        }
        Ok(out)
    }

    fn block_ref(&mut self, body: &mut Body) -> PResult<Block> {
        let at = self.offset();
        let id = self.entity("b", "a block")?;
        body.block_refs.push((id, at));
        Ok(Block::from_u32(id))
    }

    fn dest(&mut self, body: &mut Body) -> PResult<BlockCall> {
        let block = self.block_ref(body)?;
        let mut args = Vec::new();
        if self.eat_punct(b'(') {
            loop {
                let at = self.offset();
                let word = self.ident("a block argument")?;
                let arg = if word == "exn" {
                    BlockArg::Exception
                } else if word == "err" {
                    BlockArg::ErrorCode
                } else if let Some(i) = entity_id(word, "res") {
                    BlockArg::Result(
                        u32::try_from(i)
                            .map_err(|_| self.err_at(at, ParseErrorKind::IdTooLarge { id: i }))?,
                    )
                } else {
                    self.pos -= 1;
                    BlockArg::Value(self.value(body)?)
                };
                args.push(arg);
                if self.eat_punct(b')') {
                    break;
                }
                self.punct(b',', "`,` or `)`")?;
            }
        }
        Ok(BlockCall { block, args })
    }

    fn policy(&mut self) -> PResult<Policy> {
        let mut p = Policy::NONE;
        if !self.eat_punct(b'<') {
            return Ok(p);
        }
        loop {
            let at = self.offset();
            let field = self.ident("a policy field")?;
            self.punct(b'=', "`=`")?;
            let vat = self.offset();
            let value = self.ident("a policy value")?;
            let bad = |s: &Self| {
                s.err_at(
                    vat,
                    ParseErrorKind::Expected {
                        expected: "a policy value",
                        found: format_tok(value),
                    },
                )
            };
            match field {
                "overflow" => {
                    p.overflow = Some(Overflow::from_name(value).ok_or_else(|| bad(self))?)
                }
                "div_zero" => {
                    p.div_zero = Some(DivZero::from_name(value).ok_or_else(|| bad(self))?)
                }
                "shift" => p.shift = Some(Shift::from_name(value).ok_or_else(|| bad(self))?),
                "float_to_int" => {
                    p.float_to_int = Some(FloatToInt::from_name(value).ok_or_else(|| bad(self))?);
                }
                _ => {
                    return Err(self.err_at(
                        at,
                        ParseErrorKind::Expected {
                            expected: "a policy field",
                            found: format_tok(field),
                        },
                    ));
                }
            }
            if self.eat_punct(b'>') {
                return Ok(p);
            }
            self.punct(b',', "`,` or `>`")?;
        }
    }

    fn flags(&mut self) -> PResult<MemFlags> {
        self.word("align")?;
        let align = self.u32("the alignment")?;
        let volatile = self.eat_word("volatile");
        Ok(MemFlags { align, volatile })
    }

    fn order(&mut self) -> PResult<MemOrder> {
        self.named("a memory ordering", MemOrder::from_name)
    }

    fn call(&mut self, body: &mut Body, indirect: bool) -> PResult<Call> {
        let target = if indirect {
            let sig = SigId::from_u32(self.entity("s", "a signature id")?);
            let callee = self.value(body)?;
            CallTarget::Indirect { sig, callee }
        } else {
            CallTarget::Direct(self.func_ref()?)
        };
        let args = self.values_in_parens(body)?;
        let gc = if self.eat_word("gc") {
            self.values_in_parens(body)?
        } else {
            Vec::new()
        };
        let attrs = self.attrs()?;
        Ok(Call {
            target,
            args,
            gc,
            attrs,
        })
    }

    fn two(&mut self, body: &mut Body) -> PResult<[Value; 2]> {
        let a = self.value(body)?;
        self.punct(b',', "`,`")?;
        let b = self.value(body)?;
        Ok([a, b])
    }

    fn three(&mut self, body: &mut Body) -> PResult<[Value; 3]> {
        let [a, b] = self.two(body)?;
        self.punct(b',', "`,`")?;
        let c = self.value(body)?;
        Ok([a, b, c])
    }

    /// Parses an operation after its opcode word was consumed. `result` is the
    /// first result type (for `const`, `load`, conversions); `in_check` asks a
    /// conversion for an explicit `as <type>`.
    fn op(
        &mut self,
        body: &mut Body,
        name: &'a str,
        result: Option<Type>,
        in_check: bool,
        at: u32,
    ) -> PResult<InstData> {
        let need_result =
            |s: &Self| result.ok_or_else(|| s.err_at(at, ParseErrorKind::MissingResult));
        if let Some(op) = UnaryOp::from_name(name) {
            let policy = self.policy()?;
            let arg = self.value(body)?;
            return Ok(InstData::Unary { op, policy, arg });
        }
        if let Some(op) = BinaryOp::from_name(name) {
            let policy = self.policy()?;
            let args = self.two(body)?;
            return Ok(InstData::Binary { op, policy, args });
        }
        if let Some(op) = CmpOp::from_name(name) {
            let args = self.two(body)?;
            return Ok(InstData::Compare { op, args });
        }
        if let Some(op) = ConvOp::from_name(name) {
            let policy = self.policy()?;
            let arg = self.value(body)?;
            let to = if in_check {
                self.word("as")?;
                self.ty()?
            } else {
                need_result(self)?
            };
            return Ok(InstData::Convert {
                op,
                policy,
                to,
                arg,
            });
        }
        Ok(match name {
            "const" => {
                let ty = need_result(self)?;
                let bits = self.const_bits(ty)?;
                InstData::Const { ty, bits }
            }
            "fma" => InstData::Fma {
                args: self.three(body)?,
            },
            "select" => {
                let [cond, a, b] = self.three(body)?;
                InstData::Select { cond, args: [a, b] }
            }
            "stack_addr" => InstData::StackAddr {
                slot: StackSlot::from_u32(self.entity("ss", "a stack slot")?),
            },
            "global_addr" => InstData::GlobalAddr {
                global: self.global_ref()?,
            },
            "func_addr" => InstData::FuncAddr {
                func: self.func_ref()?,
            },
            "field_addr" => {
                let base = self.value(body)?;
                self.punct(b',', "`,`")?;
                let agg = AggId::from_u32(self.entity("t", "an aggregate")?);
                self.punct(b',', "`,`")?;
                let field = self.u32("a field index")?;
                InstData::FieldAddr { base, agg, field }
            }
            "elem_addr" => {
                let base = self.value(body)?;
                self.punct(b',', "`,`")?;
                let elem = self.mem_type()?;
                self.punct(b',', "`,`")?;
                let index = self.value(body)?;
                InstData::ElemAddr { base, elem, index }
            }
            "ptr_offset" => {
                let [base, offset] = self.two(body)?;
                InstData::PtrOffset { base, offset }
            }
            "load" => {
                let ty = need_result(self)?;
                let addr = self.value(body)?;
                InstData::Load {
                    ty,
                    addr,
                    flags: self.flags()?,
                }
            }
            "store" => {
                let [value, addr] = self.two(body)?;
                InstData::Store {
                    value,
                    addr,
                    flags: self.flags()?,
                }
            }
            "atomic_load" => {
                let ty = need_result(self)?;
                let addr = self.value(body)?;
                InstData::AtomicLoad {
                    ty,
                    addr,
                    order: self.order()?,
                }
            }
            "atomic_store" => {
                let [value, addr] = self.two(body)?;
                InstData::AtomicStore {
                    value,
                    addr,
                    order: self.order()?,
                }
            }
            "atomic_rmw" => {
                let op = self.named("an atomic operation", RmwOp::from_name)?;
                let [addr, value] = self.two(body)?;
                InstData::AtomicRmw {
                    op,
                    addr,
                    value,
                    order: self.order()?,
                }
            }
            "cmpxchg" => {
                let [addr, expected, new] = self.three(body)?;
                let success = self.order()?;
                let failure = self.order()?;
                InstData::CmpXchg {
                    addr,
                    expected,
                    new,
                    success,
                    failure,
                }
            }
            "fence" => InstData::Fence {
                order: self.order()?,
            },
            "memcpy" | "memmove" => {
                let [dst, src, len] = self.three(body)?;
                let flags = self.flags()?;
                if name == "memcpy" {
                    InstData::MemCopy {
                        dst,
                        src,
                        len,
                        flags,
                    }
                } else {
                    InstData::MemMove {
                        dst,
                        src,
                        len,
                        flags,
                    }
                }
            }
            "memset" => {
                let [dst, byte, len] = self.three(body)?;
                InstData::MemSet {
                    dst,
                    byte,
                    len,
                    flags: self.flags()?,
                }
            }
            "call" => InstData::Call(Box::new(self.call(body, false)?)),
            "call_indirect" => InstData::Call(Box::new(self.call(body, true)?)),
            "safepoint" => {
                self.word("gc")?;
                InstData::Safepoint {
                    gc: self.values_in_parens(body)?.into_boxed_slice(),
                }
            }
            _ => {
                return Err(self.err_at(
                    at,
                    ParseErrorKind::UnknownOpcode {
                        name: name.to_string(),
                    },
                ));
            }
        })
    }

    fn const_bits(&mut self, ty: Type) -> PResult<u64> {
        let at = self.offset();
        let bad = |s: &Self, text: &str| {
            s.err_at(
                at,
                ParseErrorKind::BadNumber {
                    text: text.to_string(),
                },
            )
        };
        match self.peek().clone() {
            Tok::Ident("true") => {
                self.bump();
                Ok(1)
            }
            Tok::Ident("false") | Tok::Ident("null") => {
                self.bump();
                Ok(0)
            }
            Tok::Ident("inf") => {
                self.bump();
                float_bits(ty, f64::INFINITY).ok_or_else(|| bad(self, "inf"))
            }
            Tok::Punct(b'-') if *self.peek_at(1) == Tok::Ident("inf") => {
                self.bump();
                self.bump();
                float_bits(ty, f64::NEG_INFINITY).ok_or_else(|| bad(self, "-inf"))
            }
            Tok::Ident("nan") => {
                self.bump();
                self.punct(b':', "`:`")?;
                self.u64("the NaN bits")
            }
            Tok::Num(text) => {
                self.bump();
                if text.starts_with("0x") || text.starts_with("0X") {
                    return parse_u64(text).ok_or_else(|| bad(self, text));
                }
                match ty {
                    Type::F64 => text
                        .parse::<f64>()
                        .map(f64::to_bits)
                        .map_err(|_| bad(self, text)),
                    Type::F32 => text
                        .parse::<f32>()
                        .map(|x| u64::from(x.to_bits()))
                        .map_err(|_| bad(self, text)),
                    t if t.is_int() => int_bits(t, text).ok_or_else(|| bad(self, text)),
                    _ => parse_u64(text).ok_or_else(|| bad(self, text)),
                }
            }
            _ => Err(self.expected("a constant")),
        }
    }

    fn instruction(&mut self, body: &mut Body, block: Block) -> PResult<()> {
        let at = self.offset();
        let mut results = core::mem::take(&mut self.results);
        results.clear();
        if let Tok::Ident(w) = *self.peek() {
            if is_entity(w, "v") {
                loop {
                    let vat = self.offset();
                    let id = self.entity("v", "a value")?;
                    self.punct(b':', "`:`")?;
                    let ty = self.ty()?;
                    results.push((id, ty, vat));
                    if self.eat_punct(b'=') {
                        break;
                    }
                    self.punct(b',', "`,` or `=`")?;
                }
            }
        }
        for pair in results.windows(2) {
            if pair[1].0 != pair[0].0.wrapping_add(1) {
                return Err(self.err_at(pair[1].2, ParseErrorKind::NonConsecutiveResults));
            }
        }
        let oat = self.offset();
        let name = self.ident("an instruction")?;
        let data = self.op(body, name, results.first().map(|r| r.1), false, oat)?;
        let inst: Inst = body.func.push_inst(data).ok_or_else(|| {
            self.err_at(
                at,
                ParseErrorKind::IdTooLarge {
                    id: u64::from(MAX_ENTITIES),
                },
            )
        })?;
        for (k, &(id, ty, vat)) in results.iter().enumerate() {
            let def = ValueDef::Result {
                inst,
                index: k as u32,
            };
            let _ = self.define_value(body, id, ty, def, vat)?;
        }
        if let Some(node) = body.func.insts.get_mut(inst.index()) {
            node.results = ValueRange {
                start: results.first().map_or(0, |r| r.0),
                len: results.len() as u32,
            };
        }
        body.func.link_at_end(inst, block);
        self.results = results;
        Ok(())
    }

    /// Parses a terminator if `word` names one.
    fn try_terminator(&mut self, body: &mut Body, word: &'a str) -> PResult<Option<Terminator>> {
        let term = match word {
            "jump" => {
                self.bump();
                Terminator::Jump(self.dest(body)?)
            }
            "br" => {
                self.bump();
                let cond = self.value(body)?;
                self.punct(b',', "`,`")?;
                let then_dest = self.dest(body)?;
                self.punct(b',', "`,`")?;
                let else_dest = self.dest(body)?;
                Terminator::Branch {
                    cond,
                    then_dest,
                    else_dest,
                }
            }
            "switch" => {
                self.bump();
                let value = self.value(body)?;
                self.word("default")?;
                let default = self.dest(body)?;
                self.punct(b'[', "`[`")?;
                let mut cases = Vec::new();
                if !self.eat_punct(b']') {
                    loop {
                        let v = self.u64("a case value")?;
                        self.punct(b':', "`:`")?;
                        cases.push(SwitchCase {
                            value: v,
                            dest: self.dest(body)?,
                        });
                        if self.eat_punct(b']') {
                            break;
                        }
                        self.punct(b',', "`,` or `]`")?;
                    }
                }
                Terminator::Switch {
                    value,
                    cases,
                    default,
                }
            }
            "ret" => {
                self.bump();
                let mut values = Vec::new();
                if matches!(*self.peek(), Tok::Ident(w) if is_entity(w, "v")) {
                    loop {
                        values.push(self.value(body)?);
                        if !self.eat_punct(b',') {
                            break;
                        }
                    }
                }
                Terminator::Return { values }
            }
            "unreachable" => {
                self.bump();
                Terminator::Unreachable
            }
            "trap" => {
                self.bump();
                Terminator::Trap {
                    code: self.u32("a trap code")?,
                }
            }
            "tail_call" | "tail_call_indirect" => {
                self.bump();
                Terminator::TailCall(Box::new(self.call(body, word == "tail_call_indirect")?))
            }
            "invoke" | "invoke_indirect" => {
                self.bump();
                let call = Box::new(self.call(body, word == "invoke_indirect")?);
                self.word("to")?;
                let normal = self.dest(body)?;
                self.word("unwind")?;
                let unwind = self.dest(body)?;
                Terminator::Invoke {
                    call,
                    normal,
                    unwind,
                }
            }
            "check" => {
                self.bump();
                let at = self.offset();
                let name = self.ident("an operation")?;
                let op = Box::new(self.op(body, name, None, true, at)?);
                self.word("to")?;
                let normal = self.dest(body)?;
                self.word("error")?;
                let error = self.dest(body)?;
                Terminator::Check { op, normal, error }
            }
            "resume" => {
                self.bump();
                Terminator::Resume {
                    payload: self.value(body)?,
                }
            }
            _ => return Ok(None),
        };
        Ok(Some(term))
    }
}

fn float_bits(ty: Type, x: f64) -> Option<u64> {
    match ty {
        Type::F64 => Some(x.to_bits()),
        Type::F32 => Some(u64::from((x as f32).to_bits())),
        _ => None,
    }
}

fn parse_u64(text: &str) -> Option<u64> {
    let t = text.strip_prefix('+').unwrap_or(text);
    if let Some(hex) = t.strip_prefix("0x").or_else(|| t.strip_prefix("0X")) {
        u64::from_str_radix(hex, 16).ok()
    } else {
        t.parse().ok()
    }
}

fn parse_i64(text: &str) -> Option<i64> {
    text.strip_prefix('+').unwrap_or(text).parse().ok()
}

/// A decimal integer constant in the range of `ty`, as bits masked to its width.
fn int_bits(ty: Type, text: &str) -> Option<u64> {
    let width = ty.int_bits()?;
    let v: i128 = text.strip_prefix('+').unwrap_or(text).parse().ok()?;
    let (lo, hi) = if ty.is_signed_int() {
        (-(1i128 << (width - 1)), (1i128 << (width - 1)) - 1)
    } else {
        (0, (1i128 << width) - 1)
    };
    if v < lo || v > hi {
        return None;
    }
    let mask: u128 = if width == 64 {
        u128::from(u64::MAX)
    } else {
        (1u128 << width) - 1
    };
    Some(((v as u128) & mask) as u64)
}

fn parse_hex(s: &str) -> Option<Vec<u8>> {
    if s.len() % 2 != 0 {
        return None;
    }
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(s.len() / 2);
    for pair in bytes.chunks(2) {
        let hi = (*pair.first()? as char).to_digit(16)?;
        let lo = (*pair.get(1)? as char).to_digit(16)?;
        out.push((hi * 16 + lo) as u8);
    }
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_lexer_numbers_and_signs() {
        let toks = lex("1e-5 -3 +4 0x1e -> a.b @x @\"y z\"").unwrap_or_default();
        let kinds: Vec<Tok<'_>> = toks.into_iter().map(|t| t.tok).collect();
        assert_eq!(
            kinds,
            [
                Tok::Num("1e-5"),
                Tok::Num("-3"),
                Tok::Num("+4"),
                Tok::Num("0x1e"),
                Tok::Arrow,
                Tok::Ident("a.b"),
                Tok::Sym("x".to_string()),
                Tok::Sym("y z".to_string()),
                Tok::Eof
            ]
        );
    }

    #[test]
    fn test_int_bits_range() {
        assert_eq!(int_bits(Type::I8, "-1"), Some(0xff));
        assert_eq!(int_bits(Type::I8, "-129"), None);
        assert_eq!(int_bits(Type::U64, "18446744073709551615"), Some(u64::MAX));
        assert_eq!(int_bits(Type::U8, "-1"), None);
    }

    #[test]
    fn test_hex_init() {
        assert_eq!(parse_hex("00ff10"), Some(alloc::vec![0, 255, 16]));
        assert_eq!(parse_hex("0"), None);
        assert_eq!(parse_hex("zz"), None);
    }

    #[test]
    fn test_error_position_counts_characters() {
        let e = error_at("ab\ncé x", 7, ParseErrorKind::MissingResult);
        assert_eq!((e.line, e.column), (2, 4));
    }

    #[test]
    fn test_unterminated_string_is_an_error() {
        assert!(matches!(
            lex("\"abc").map(|_| ()),
            Err(ParseError {
                kind: ParseErrorKind::UnterminatedString,
                ..
            })
        ));
        assert!(lex("\"a\\q\"").is_err());
        assert!(lex("\"\\u{110000}\"").is_err());
    }
}
