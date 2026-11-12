// Copyright (c) 2026 tre4surehunter9
//! Lexer: shell text to tokens.
//!
//! Quotes and escapes are resolved here, but expansions ($VAR, $(cmd),
//! $((expr)), ~) are kept as parts of the word and done when the command
//! runs. Each part remembers whether it was quoted, because only unquoted
//! text is split into fields and matched against file names (globbing).

use alloc::boxed::Box;
use alloc::string::{String, ToString};
use alloc::vec::Vec;

/// A piece of a word.
#[derive(Debug, Clone, PartialEq)]
pub enum Part {
    /// Unquoted literal text (may contain glob characters).
    Lit(String),
    /// Quoted literal text.
    Quoted(String),
    /// $NAME, ${NAME}, ${NAME:-word}, ${#NAME}, $?, $1, "$@", ...
    Param { name: String, op: ParamOp, quoted: bool },
    /// $(commands) or `commands`
    CmdSub { src: String, quoted: bool },
    /// $((expression))
    Arith { src: String, quoted: bool },
    /// ~ at the start of an unquoted word: $HOME
    Home,
}

#[derive(Debug, Clone, PartialEq)]
pub enum ParamOp {
    Plain,
    /// ${#NAME}
    Length,
    /// ${NAME:-word} (colon: also when set but empty) and friends.
    Default { colon: bool, word: Box<Word> },
    Assign { colon: bool, word: Box<Word> },
    Alternative { colon: bool, word: Box<Word> },
    Error { colon: bool, word: Box<Word> },
}

pub type Word = Vec<Part>;

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum RedirKind {
    Out,       // >
    Append,    // >>
    In,        // <
    ToFd(i32), // >&N  <&N
    /// <<WORD or <<-WORD (`strip`: leading tabs removed): the target is
    /// the document's text, filled in by the lexer from the lines after
    /// the command.
    HereDoc { strip: bool },
}

#[derive(Debug, Clone, PartialEq)]
pub struct Redir {
    pub fd: i32,
    pub kind: RedirKind,
    /// The target file (empty for ToFd); filled in by the parser.
    pub target: Word,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Op {
    Semi,   // ;
    DSemi,  // ;;
    Amp,    // &
    AndAnd, // &&
    OrOr,   // ||
    Pipe,   // |
    LParen, // (
    RParen, // )
}

#[derive(Debug, Clone, PartialEq)]
pub enum Tok {
    Word(Word),
    Op(Op),
    /// A redirection operator; its target is the next word.
    Redir(Redir),
    Newline,
}

#[derive(Debug, Clone, PartialEq)]
pub enum LexError {
    /// The text ended inside a quote or $( ... ): more input may finish it.
    Incomplete,
    Syntax(String),
}

pub fn is_name_char(c: char) -> bool {
    c.is_ascii_alphanumeric() || c == '_'
}

pub fn is_name(s: &str) -> bool {
    !s.is_empty() && !s.starts_with(|c: char| c.is_ascii_digit()) && s.chars().all(is_name_char)
}

struct Lexer {
    chars: Vec<char>,
    i: usize,
}

/// A here-document whose text comes after the next newline: its token's
/// index, the delimiter, whether the delimiter was quoted (then the text is
/// taken literally), and whether to strip leading tabs.
struct PendingHereDoc {
    tok: usize,
    delim: String,
    quoted: bool,
    strip: bool,
}

impl Lexer {
    fn peek(&self) -> Option<char> {
        self.chars.get(self.i).copied()
    }

    fn at(&self, offset: usize) -> Option<char> {
        self.chars.get(self.i + offset).copied()
    }

    /// Index of the ')' matching an already-open '(' starting at `start`,
    /// skipping quoted text.
    fn find_close_paren(&self, start: usize) -> Result<usize, LexError> {
        let mut depth = 1;
        let mut j = start;
        while j < self.chars.len() {
            match self.chars[j] {
                '\\' => j += 1,
                '\'' => {
                    j += 1;
                    while j < self.chars.len() && self.chars[j] != '\'' {
                        j += 1;
                    }
                }
                '"' => {
                    j += 1;
                    while j < self.chars.len() && self.chars[j] != '"' {
                        if self.chars[j] == '\\' {
                            j += 1;
                        }
                        j += 1;
                    }
                }
                '(' => depth += 1,
                ')' => {
                    depth -= 1;
                    if depth == 0 {
                        return Ok(j);
                    }
                }
                _ => {}
            }
            j += 1;
        }
        Err(LexError::Incomplete)
    }

    /// Parse a $ expansion at self.i (which is '$'). Returns None if the
    /// '$' is just a literal dollar sign.
    fn dollar(&mut self, quoted: bool) -> Result<Option<Part>, LexError> {
        match self.at(1) {
            Some('(') if self.at(2) == Some('(') => {
                // $(( expr ))
                let start = self.i + 3;
                let mut depth = 0;
                let mut j = start;
                loop {
                    match self.chars.get(j) {
                        None => return Err(LexError::Incomplete),
                        Some('(') => depth += 1,
                        Some(')') if depth == 0 => {
                            if self.chars.get(j + 1) != Some(&')') {
                                return Err(LexError::Syntax("expected '))'".into()));
                            }
                            break;
                        }
                        Some(')') => depth -= 1,
                        _ => {}
                    }
                    j += 1;
                }
                let src: String = self.chars[start..j].iter().collect();
                self.i = j + 2;
                Ok(Some(Part::Arith { src, quoted }))
            }
            Some('(') => {
                let start = self.i + 2;
                let end = self.find_close_paren(start)?;
                let src: String = self.chars[start..end].iter().collect();
                self.i = end + 1;
                Ok(Some(Part::CmdSub { src, quoted }))
            }
            Some('{') => {
                let start = self.i + 2;
                let end = start
                    + self.chars[start..].iter().position(|&c| c == '}').ok_or(LexError::Incomplete)?;
                let inner: String = self.chars[start..end].iter().collect();
                self.i = end + 1;
                Ok(Some(parse_braced(&inner, quoted)?))
            }
            Some(c) if matches!(c, '?' | '$' | '#' | '@' | '*' | '!' | '-') || c.is_ascii_digit() => {
                self.i += 2;
                Ok(Some(Part::Param { name: c.to_string(), op: ParamOp::Plain, quoted }))
            }
            Some(c) if is_name_char(c) => {
                let start = self.i + 1;
                let mut j = start;
                while j < self.chars.len() && is_name_char(self.chars[j]) {
                    j += 1;
                }
                let name: String = self.chars[start..j].iter().collect();
                self.i = j;
                Ok(Some(Part::Param { name, op: ParamOp::Plain, quoted }))
            }
            _ => Ok(None),
        }
    }

    /// `commands` (old-style command substitution)
    fn backtick(&mut self, quoted: bool) -> Result<Part, LexError> {
        let mut src = String::new();
        let mut j = self.i + 1;
        loop {
            match self.chars.get(j) {
                None => return Err(LexError::Incomplete),
                Some('`') => break,
                Some('\\') if matches!(self.chars.get(j + 1), Some('`' | '\\' | '$')) => {
                    src.push(self.chars[j + 1]);
                    j += 2;
                }
                Some(&c) => {
                    src.push(c);
                    j += 1;
                }
            }
        }
        self.i = j + 1;
        Ok(Part::CmdSub { src, quoted })
    }
}

/// The inside of ${...}.
fn parse_braced(inner: &str, quoted: bool) -> Result<Part, LexError> {
    let bad = || LexError::Syntax(alloc::format!("${{{}}}: bad substitution", inner));
    if let Some(name) = inner.strip_prefix('#') {
        if !name.is_empty() {
            if !(is_name(name) || name.chars().all(|c| c.is_ascii_digit())) {
                return Err(bad());
            }
            return Ok(Part::Param { name: name.to_string(), op: ParamOp::Length, quoted });
        }
    }
    let name_len = if inner.starts_with(|c: char| matches!(c, '?' | '$' | '#' | '@' | '*' | '!')) {
        1
    } else {
        inner.chars().take_while(|&c| is_name_char(c)).count()
    };
    if name_len == 0 {
        return Err(bad());
    }
    let (name, rest) = inner.split_at(name_len);
    if rest.is_empty() {
        return Ok(Part::Param { name: name.to_string(), op: ParamOp::Plain, quoted });
    }
    let (colon, rest) = match rest.strip_prefix(':') {
        Some(r) => (true, r),
        None => (false, rest),
    };
    let mut chars = rest.chars();
    let op_char = chars.next().ok_or_else(bad)?;
    let word = Box::new(lex_word_text(chars.as_str())?);
    let op = match op_char {
        '-' => ParamOp::Default { colon, word },
        '=' => ParamOp::Assign { colon, word },
        '+' => ParamOp::Alternative { colon, word },
        '?' => ParamOp::Error { colon, word },
        _ => return Err(bad()),
    };
    Ok(Part::Param { name: name.to_string(), op, quoted })
}

/// Lex text that is a single word with no delimiters (the "word" inside
/// ${NAME:-word}); spaces are kept as literal text.
pub fn lex_word_text(text: &str) -> Result<Word, LexError> {
    let mut lx = Lexer { chars: text.chars().collect(), i: 0 };
    let mut word = Vec::new();
    while lx.peek().is_some() {
        lex_word_piece(&mut lx, &mut word, true)?;
    }
    Ok(word)
}

fn push_text(word: &mut Word, quoted: bool, c: char) {
    match (word.last_mut(), quoted) {
        (Some(Part::Lit(s)), false) | (Some(Part::Quoted(s)), true) => s.push(c),
        _ => word.push(if quoted { Part::Quoted(c.to_string()) } else { Part::Lit(c.to_string()) }),
    }
}

/// Lex one piece of a word at the current position (a quoted string, an
/// expansion, an escaped character or a plain character). `spaces_literal`
/// is for ${x:-two words}, where spaces don't end the word.
fn lex_word_piece(lx: &mut Lexer, word: &mut Word, spaces_literal: bool) -> Result<(), LexError> {
    let c = lx.peek().unwrap();
    match c {
        '\'' => {
            let start = lx.i + 1;
            let end = start + lx.chars[start..].iter().position(|&c| c == '\'').ok_or(LexError::Incomplete)?;
            let text: String = lx.chars[start..end].iter().collect();
            word.push(Part::Quoted(text)); // even '' makes an (empty) word
            lx.i = end + 1;
        }
        '"' => {
            lx.i += 1;
            word.push(Part::Quoted(String::new()));
            loop {
                match lx.peek() {
                    None => return Err(LexError::Incomplete),
                    Some('"') => {
                        lx.i += 1;
                        break;
                    }
                    Some('\\') => match lx.at(1) {
                        Some('\n') => lx.i += 2, // line continuation
                        Some(n @ ('"' | '\\' | '$' | '`')) => {
                            push_text(word, true, n);
                            lx.i += 2;
                        }
                        _ => {
                            push_text(word, true, '\\');
                            lx.i += 1;
                        }
                    },
                    Some('$') => match lx.dollar(true)? {
                        Some(part) => word.push(part),
                        None => {
                            push_text(word, true, '$');
                            lx.i += 1;
                        }
                    },
                    Some('`') => {
                        let part = lx.backtick(true)?;
                        word.push(part);
                    }
                    Some(c) => {
                        push_text(word, true, c);
                        lx.i += 1;
                    }
                }
            }
        }
        '\\' => match lx.at(1) {
            Some('\n') => lx.i += 2, // line continuation
            Some(n) => {
                push_text(word, true, n);
                lx.i += 2;
            }
            None => {
                push_text(word, false, '\\');
                lx.i += 1;
            }
        },
        '$' => match lx.dollar(false)? {
            Some(part) => word.push(part),
            None => {
                push_text(word, false, '$');
                lx.i += 1;
            }
        },
        '`' => {
            let part = lx.backtick(false)?;
            word.push(part);
        }
        '~' if word.is_empty()
            && matches!(lx.at(1), None | Some('/' | ' ' | '\t' | '\n' | ';' | '&' | '|' | '<' | '>' | ')' | ':')) =>
        {
            word.push(Part::Home);
            lx.i += 1;
        }
        ' ' | '\t' if spaces_literal => {
            push_text(word, false, c);
            lx.i += 1;
        }
        c => {
            push_text(word, false, c);
            lx.i += 1;
        }
    }
    Ok(())
}

fn is_delimiter(c: char) -> bool {
    matches!(c, ' ' | '\t' | '\r' | '\n' | ';' | '&' | '|' | '(' | ')' | '<' | '>')
}

pub fn lex(src: &str) -> Result<Vec<Tok>, LexError> {
    let mut lx = Lexer { chars: src.chars().collect(), i: 0 };
    let mut toks = Vec::new();
    let mut heredocs: Vec<PendingHereDoc> = Vec::new();

    while let Some(c) = lx.peek() {
        match c {
            ' ' | '\t' | '\r' => lx.i += 1,
            '\\' if lx.at(1) == Some('\n') => lx.i += 2,
            '\n' => {
                toks.push(Tok::Newline);
                lx.i += 1;
                // The here-documents of this line follow it, in order.
                for h in heredocs.drain(..) {
                    let body = read_heredoc(&mut lx, &h)?;
                    if let Some(Tok::Redir(r)) = toks.get_mut(h.tok) {
                        r.target = body;
                    }
                }
            }
            '<' if lx.at(1) == Some('<') => {
                let (tok, h) = lex_heredoc_op(&mut lx, toks.len(), 0)?;
                heredocs.push(h);
                toks.push(tok);
            }
            '#' => {
                while lx.peek().is_some_and(|c| c != '\n') {
                    lx.i += 1;
                }
            }
            ';' => {
                if lx.at(1) == Some(';') {
                    toks.push(Tok::Op(Op::DSemi));
                    lx.i += 2;
                } else {
                    toks.push(Tok::Op(Op::Semi));
                    lx.i += 1;
                }
            }
            '&' | '|' => {
                let double = lx.at(1) == Some(c);
                toks.push(Tok::Op(match (c, double) {
                    ('&', true) => Op::AndAnd,
                    ('&', false) => Op::Amp,
                    ('|', true) => Op::OrOr,
                    _ => Op::Pipe,
                }));
                lx.i += if double { 2 } else { 1 };
            }
            '(' => {
                toks.push(Tok::Op(Op::LParen));
                lx.i += 1;
            }
            ')' => {
                toks.push(Tok::Op(Op::RParen));
                lx.i += 1;
            }
            '<' | '>' => toks.push(lex_redir(&mut lx, if c == '>' { 1 } else { 0 })?),
            _ => {
                let mut word: Word = Vec::new();
                while let Some(c) = lx.peek() {
                    if is_delimiter(c) {
                        break;
                    }
                    lex_word_piece(&mut lx, &mut word, false)?;
                }
                // "2>file": a word of only digits right before > or < is the fd.
                if let ([Part::Lit(digits)], Some('<' | '>')) = (word.as_slice(), lx.peek()) {
                    if digits.chars().all(|d| d.is_ascii_digit()) {
                        let fd = digits.parse().map_err(|_| LexError::Syntax("bad file descriptor".into()))?;
                        if lx.peek() == Some('<') && lx.at(1) == Some('<') {
                            let (tok, h) = lex_heredoc_op(&mut lx, toks.len(), fd)?; // 3<<EOF
                            heredocs.push(h);
                            toks.push(tok);
                            continue;
                        }
                        toks.push(lex_redir(&mut lx, fd)?);
                        continue;
                    }
                }
                toks.push(Tok::Word(word));
            }
        }
    }
    if !heredocs.is_empty() {
        // The text ended before a here-document's lines.
        return Err(LexError::Incomplete);
    }
    Ok(toks)
}

/// << or <<- and its delimiter word, at lx.i, for fd `fd`; the token goes
/// at index `tok` and its text is read after the line ends.
fn lex_heredoc_op(lx: &mut Lexer, tok: usize, fd: i32) -> Result<(Tok, PendingHereDoc), LexError> {
    lx.i += 2;
    let strip = lx.peek() == Some('-');
    if strip {
        lx.i += 1;
    }
    while matches!(lx.peek(), Some(' ' | '\t')) {
        lx.i += 1;
    }
    // The delimiter: a word; any quoting in it means the text is taken
    // literally.
    let start = lx.i;
    let mut word: Word = Vec::new();
    while let Some(c) = lx.peek() {
        if is_delimiter(c) {
            break;
        }
        lex_word_piece(lx, &mut word, false)?;
    }
    if word.is_empty() {
        return Err(LexError::Syntax("syntax error: missing word after <<".into()));
    }
    let quoted = lx.chars[start..lx.i].iter().any(|&c| matches!(c, '\'' | '"' | '\\'));
    let delim: String = if word.iter().all(|p| matches!(p, Part::Lit(_) | Part::Quoted(_))) {
        word.iter().map(|p| match p { Part::Lit(s) | Part::Quoted(s) => s.as_str(), _ => "" }).collect()
    } else {
        lx.chars[start..lx.i].iter().collect() // $ in a delimiter: taken as written
    };
    let h = PendingHereDoc { tok, delim, quoted, strip };
    Ok((Tok::Redir(Redir { fd, kind: RedirKind::HereDoc { strip }, target: Vec::new() }), h))
}

/// The lines of a here-document, at lx.i (just after a newline), up to its
/// delimiter line, as a word: literal if the delimiter was quoted, else
/// with $ expansions, `commands` and \ before $ ` \ or a newline.
fn read_heredoc(lx: &mut Lexer, h: &PendingHereDoc) -> Result<Word, LexError> {
    let mut text = String::new();
    loop {
        if lx.i >= lx.chars.len() {
            return Err(LexError::Incomplete);
        }
        let end = lx.chars[lx.i..].iter().position(|&c| c == '\n').map(|p| lx.i + p);
        let Some(end) = end else {
            return Err(LexError::Incomplete); // (the delimiter line needs its newline)
        };
        let mut line: String = lx.chars[lx.i..end].iter().collect();
        lx.i = end + 1;
        if h.strip {
            line = line.trim_start_matches('\t').to_string();
        }
        if line == h.delim {
            break;
        }
        text.push_str(&line);
        text.push('\n');
    }
    if h.quoted {
        return Ok(alloc::vec![Part::Quoted(text)]);
    }
    let mut body = Lexer { chars: text.chars().collect(), i: 0 };
    let mut word: Word = alloc::vec![Part::Quoted(String::new())];
    while let Some(c) = body.peek() {
        match c {
            '\\' => match body.at(1) {
                Some('\n') => body.i += 2,
                Some(n @ ('$' | '`' | '\\')) => {
                    push_text(&mut word, true, n);
                    body.i += 2;
                }
                _ => {
                    push_text(&mut word, true, '\\');
                    body.i += 1;
                }
            },
            '$' => match body.dollar(true)? {
                Some(part) => word.push(part),
                None => {
                    push_text(&mut word, true, '$');
                    body.i += 1;
                }
            },
            '`' => {
                let part = body.backtick(true)?;
                word.push(part);
            }
            c => {
                push_text(&mut word, true, c);
                body.i += 1;
            }
        }
    }
    Ok(word)
}

/// A redirection operator at lx.i (< or >), applying to `fd`.
fn lex_redir(lx: &mut Lexer, fd: i32) -> Result<Tok, LexError> {
    let c = lx.peek().unwrap();
    lx.i += 1;
    let kind = if lx.peek() == Some('&') {
        lx.i += 1;
        let start = lx.i;
        while lx.peek().is_some_and(|c| c.is_ascii_digit()) {
            lx.i += 1;
        }
        let digits: String = lx.chars[start..lx.i].iter().collect();
        let target = digits.parse().map_err(|_| LexError::Syntax(alloc::format!("bad file descriptor after {}&", c)))?;
        RedirKind::ToFd(target)
    } else if c == '<' {
        RedirKind::In
    } else if lx.peek() == Some('>') {
        lx.i += 1;
        RedirKind::Append
    } else {
        RedirKind::Out
    };
    Ok(Tok::Redir(Redir { fd, kind, target: Vec::new() }))
}
