// Copyright (c) 2026 tre4surehunter9
//! Parser: tokens to a syntax tree (POSIX shell grammar, simplified).
//!
//!   list     := and_or ((';' | '&' | newline) and_or)*
//!   and_or   := pipeline (('&&' | '||') pipeline)*
//!   pipeline := ['!'] command ('|' command)*
//!   command  := simple | compound redirect* | NAME '(' ')' compound
//!   compound := { list } | ( list ) | if | while | until | for | case

use alloc::format;
use alloc::string::{String, ToString};
use alloc::vec::Vec;

use crate::lex::{is_name, lex, LexError, Op, Part, Redir, RedirKind, Tok, Word};

/// The shell's aliases (`alias ll='ls -l'`), used when commands are parsed.
/// (The shell runs one thread.)
pub struct Aliases(core::cell::UnsafeCell<alloc::collections::BTreeMap<String, String>>);
unsafe impl Sync for Aliases {}
pub static ALIASES: Aliases = Aliases(core::cell::UnsafeCell::new(alloc::collections::BTreeMap::new()));

impl Aliases {
    pub fn with<R>(&self, f: impl FnOnce(&mut alloc::collections::BTreeMap<String, String>) -> R) -> R {
        // SAFETY: one thread, and `f` can't reach ALIASES again.
        unsafe { f(&mut *self.0.get()) }
    }
    fn get(&self, name: &str) -> Option<String> {
        self.with(|a| a.get(name).cloned())
    }
}

#[derive(Debug, Clone, Default)]
pub struct List(pub Vec<(AndOr, bool)>); // (command, run in background)

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Connector {
    And,
    Or,
}

#[derive(Debug, Clone)]
pub struct AndOr {
    pub first: Pipeline,
    pub rest: Vec<(Connector, Pipeline)>,
}

#[derive(Debug, Clone)]
pub struct Pipeline {
    pub negate: bool,
    pub cmds: Vec<Cmd>,
}

#[derive(Debug, Clone, Default)]
pub struct Simple {
    pub assigns: Vec<(String, Word)>,
    pub words: Vec<Word>,
    pub redirs: Vec<Redir>,
}

#[derive(Debug, Clone)]
pub enum Compound {
    Group(List),
    Subshell(List),
    If { branches: Vec<(List, List)>, otherwise: Option<List> },
    Loop { cond: List, body: List, until: bool },
    For { var: String, words: Option<Vec<Word>>, body: List },
    Case { word: Word, arms: Vec<(Vec<Word>, List)> },
}

#[derive(Debug, Clone)]
pub enum Cmd {
    Simple(Simple),
    Compound(Compound, Vec<Redir>),
    FuncDef(String, Compound),
}

#[derive(Debug, Clone, PartialEq)]
pub enum ParseError {
    /// The input ended in the middle of a command (e.g. `if true; then`):
    /// an interactive shell asks for another line.
    Incomplete,
    Syntax(String),
}

const RESERVED: &[&str] = &[
    "if", "then", "elif", "else", "fi", "while", "until", "do", "done", "for", "in", "case", "esac", "{", "}", "!",
];

pub fn parse(toks: Vec<Tok>) -> Result<List, ParseError> {
    let mut p = Parser { toks, pos: 0, alias_guards: Vec::new(), alias_next: None };
    let list = p.list(&[])?;
    p.skip_newlines();
    match p.peek() {
        None => Ok(list),
        Some(t) => Err(ParseError::Syntax(format!("syntax error near {}", describe(t)))),
    }
}

fn describe(t: &Tok) -> String {
    match t {
        Tok::Word(w) => format!("'{}'", plain_text(w).unwrap_or_else(|| "word".to_string())),
        Tok::Op(op) => format!(
            "'{}'",
            match op {
                Op::Semi => ";",
                Op::DSemi => ";;",
                Op::Amp => "&",
                Op::AndAnd => "&&",
                Op::OrOr => "||",
                Op::Pipe => "|",
                Op::LParen => "(",
                Op::RParen => ")",
            }
        ),
        Tok::Redir(_) => "redirection".to_string(),
        Tok::Newline => "newline".to_string(),
    }
}

/// The text of a word made only of unquoted literal text.
pub fn plain_text(w: &Word) -> Option<String> {
    match w.as_slice() {
        [Part::Lit(s)] => Some(s.clone()),
        _ => None,
    }
}

struct Parser {
    toks: Vec<Tok>,
    pos: usize,
    /// Aliases being expanded: the name and where its text ends (an alias
    /// isn't expanded again inside its own text: alias ls='ls -F').
    alias_guards: Vec<(String, usize)>,
    /// Where the next word may be an alias too (after an alias whose text
    /// ends with a blank).
    alias_next: Option<usize>,
}

impl Parser {
    /// If the word here is an alias, replace it with the alias's tokens.
    fn expand_alias(&mut self) -> Result<bool, ParseError> {
        let Some(Tok::Word(w)) = self.peek() else { return Ok(false) };
        let Some(name) = plain_text(w) else { return Ok(false) };
        if RESERVED.contains(&name.as_str()) || self.peek_at(1) == Some(&Tok::Op(Op::LParen)) {
            return Ok(false); // (and not `name() { ...; }`)
        }
        if self.alias_guards.iter().any(|(n, end)| *n == name && *end > self.pos) {
            return Ok(false);
        }
        let Some(value) = ALIASES.get(&name) else { return Ok(false) };
        let toks = match lex(&value) {
            Ok(t) => t,
            Err(LexError::Incomplete) => {
                return Err(ParseError::Syntax(format!("alias {}: unfinished quote in its text", name)))
            }
            Err(LexError::Syntax(e)) => return Err(ParseError::Syntax(format!("alias {}: {}", name, e))),
        };
        let n = toks.len();
        let pos = self.pos;
        self.toks.splice(pos..pos + 1, toks);
        for (_, end) in self.alias_guards.iter_mut() {
            if *end > pos {
                *end = *end + n - 1;
            }
        }
        self.alias_guards.push((name, pos + n));
        if value.ends_with(|c: char| c == ' ' || c == '\t') {
            self.alias_next = Some(pos + n);
        }
        Ok(true)
    }

    fn peek(&self) -> Option<&Tok> {
        self.toks.get(self.pos)
    }

    fn peek_at(&self, n: usize) -> Option<&Tok> {
        self.toks.get(self.pos + n)
    }

    fn next(&mut self) -> Option<Tok> {
        let t = self.toks.get(self.pos).cloned();
        self.pos += 1;
        t
    }

    fn skip_newlines(&mut self) {
        while self.peek() == Some(&Tok::Newline) {
            self.pos += 1;
        }
    }

    /// The reserved word at the current position, if any.
    fn reserved(&self) -> Option<&'static str> {
        match self.peek() {
            Some(Tok::Word(w)) => {
                let text = plain_text(w)?;
                RESERVED.iter().copied().find(|r| *r == text)
            }
            _ => None,
        }
    }

    fn is_op(&self, op: Op) -> bool {
        self.peek() == Some(&Tok::Op(op))
    }

    fn expect_reserved(&mut self, word: &str) -> Result<(), ParseError> {
        self.skip_newlines();
        match self.reserved() {
            Some(r) if r == word => {
                self.pos += 1;
                Ok(())
            }
            _ => match self.peek() {
                None => Err(ParseError::Incomplete),
                Some(t) => Err(ParseError::Syntax(format!("expected '{}' near {}", word, describe(t)))),
            },
        }
    }

    /// A list, ending before EOF, ')', ';;', or one of the `stop` words.
    fn list(&mut self, stop: &[&str]) -> Result<List, ParseError> {
        let mut items = Vec::new();
        loop {
            self.skip_newlines();
            match self.peek() {
                None | Some(Tok::Op(Op::RParen)) | Some(Tok::Op(Op::DSemi)) => break,
                _ => {}
            }
            if let Some(r) = self.reserved() {
                if stop.contains(&r) {
                    break;
                }
            }
            let ao = self.and_or()?;
            match self.peek() {
                Some(Tok::Op(Op::Semi)) | Some(Tok::Newline) => {
                    self.pos += 1;
                    items.push((ao, false));
                }
                Some(Tok::Op(Op::Amp)) => {
                    self.pos += 1;
                    items.push((ao, true));
                }
                _ => {
                    items.push((ao, false));
                    break;
                }
            }
        }
        Ok(List(items))
    }

    /// A list that must contain at least one command (e.g. after `then`).
    fn nonempty_list(&mut self, stop: &[&str], what: &str) -> Result<List, ParseError> {
        let list = self.list(stop)?;
        if list.0.is_empty() {
            return match self.peek() {
                None => Err(ParseError::Incomplete),
                Some(t) => Err(ParseError::Syntax(format!("syntax error: empty {} near {}", what, describe(t)))),
            };
        }
        Ok(list)
    }

    fn and_or(&mut self) -> Result<AndOr, ParseError> {
        let first = self.pipeline()?;
        let mut rest = Vec::new();
        loop {
            let conn = match self.peek() {
                Some(Tok::Op(Op::AndAnd)) => Connector::And,
                Some(Tok::Op(Op::OrOr)) => Connector::Or,
                _ => break,
            };
            self.pos += 1;
            self.skip_newlines();
            if self.peek().is_none() {
                return Err(ParseError::Incomplete);
            }
            rest.push((conn, self.pipeline()?));
        }
        Ok(AndOr { first, rest })
    }

    fn pipeline(&mut self) -> Result<Pipeline, ParseError> {
        let negate = self.reserved() == Some("!");
        if negate {
            self.pos += 1;
        }
        let mut cmds = vec_of(self.command()?);
        while self.is_op(Op::Pipe) {
            self.pos += 1;
            self.skip_newlines();
            if self.peek().is_none() {
                return Err(ParseError::Incomplete);
            }
            cmds.push(self.command()?);
        }
        Ok(Pipeline { negate, cmds })
    }

    fn command(&mut self) -> Result<Cmd, ParseError> {
        while self.expand_alias()? {}
        if let Some(r) = self.reserved() {
            let compound = match r {
                "{" | "if" | "while" | "until" | "for" | "case" => self.compound()?,
                other => return Err(ParseError::Syntax(format!("syntax error near unexpected '{}'", other))),
            };
            let redirs = self.redirects()?;
            return Ok(Cmd::Compound(compound, redirs));
        }
        if self.is_op(Op::LParen) {
            let compound = self.compound()?;
            let redirs = self.redirects()?;
            return Ok(Cmd::Compound(compound, redirs));
        }
        // NAME ( ) compound-command
        if let (Some(Tok::Word(w)), Some(Tok::Op(Op::LParen)), Some(Tok::Op(Op::RParen))) =
            (self.peek(), self.peek_at(1), self.peek_at(2))
        {
            if let Some(name) = plain_text(w).filter(|n| is_name(n)) {
                self.pos += 3;
                self.skip_newlines();
                if self.peek().is_none() {
                    return Err(ParseError::Incomplete);
                }
                let body = self.compound()?;
                return Ok(Cmd::FuncDef(name, body));
            }
        }
        self.simple()
    }

    fn compound(&mut self) -> Result<Compound, ParseError> {
        if self.is_op(Op::LParen) {
            self.pos += 1;
            let list = self.nonempty_list(&[], "subshell")?;
            self.skip_newlines();
            return match self.next() {
                Some(Tok::Op(Op::RParen)) => Ok(Compound::Subshell(list)),
                None => Err(ParseError::Incomplete),
                Some(t) => Err(ParseError::Syntax(format!("expected ')' near {}", describe(&t)))),
            };
        }
        match self.reserved() {
            Some("{") => {
                self.pos += 1;
                let list = self.nonempty_list(&["}"], "{ }")?;
                self.expect_reserved("}")?;
                Ok(Compound::Group(list))
            }
            Some("if") => {
                self.pos += 1;
                let mut branches = Vec::new();
                let cond = self.nonempty_list(&["then"], "if condition")?;
                self.expect_reserved("then")?;
                let body = self.nonempty_list(&["elif", "else", "fi"], "then")?;
                branches.push((cond, body));
                let mut otherwise = None;
                loop {
                    self.skip_newlines();
                    match self.reserved() {
                        Some("elif") => {
                            self.pos += 1;
                            let cond = self.nonempty_list(&["then"], "elif condition")?;
                            self.expect_reserved("then")?;
                            let body = self.nonempty_list(&["elif", "else", "fi"], "then")?;
                            branches.push((cond, body));
                        }
                        Some("else") => {
                            self.pos += 1;
                            otherwise = Some(self.nonempty_list(&["fi"], "else")?);
                            self.expect_reserved("fi")?;
                            break;
                        }
                        _ => {
                            self.expect_reserved("fi")?;
                            break;
                        }
                    }
                }
                Ok(Compound::If { branches, otherwise })
            }
            Some(kw @ ("while" | "until")) => {
                self.pos += 1;
                let cond = self.nonempty_list(&["do"], "loop condition")?;
                self.expect_reserved("do")?;
                let body = self.nonempty_list(&["done"], "loop body")?;
                self.expect_reserved("done")?;
                Ok(Compound::Loop { cond, body, until: kw == "until" })
            }
            Some("for") => {
                self.pos += 1;
                let var = match self.next() {
                    Some(Tok::Word(w)) => match plain_text(&w).filter(|n| is_name(n)) {
                        Some(n) => n,
                        None => return Err(ParseError::Syntax("for: bad variable name".into())),
                    },
                    None => return Err(ParseError::Incomplete),
                    Some(t) => return Err(ParseError::Syntax(format!("for: expected a name near {}", describe(&t)))),
                };
                self.skip_newlines();
                let mut words = None;
                if self.reserved() == Some("in") {
                    self.pos += 1;
                    let mut list = Vec::new();
                    while let Some(Tok::Word(w)) = self.peek() {
                        list.push(w.clone());
                        self.pos += 1;
                    }
                    words = Some(list);
                }
                match self.peek() {
                    Some(Tok::Op(Op::Semi)) | Some(Tok::Newline) => self.pos += 1,
                    _ => {}
                }
                self.expect_reserved("do")?;
                let body = self.nonempty_list(&["done"], "loop body")?;
                self.expect_reserved("done")?;
                Ok(Compound::For { var, words, body })
            }
            Some("case") => {
                self.pos += 1;
                let word = match self.next() {
                    Some(Tok::Word(w)) => w,
                    None => return Err(ParseError::Incomplete),
                    Some(t) => return Err(ParseError::Syntax(format!("case: expected a word near {}", describe(&t)))),
                };
                self.expect_reserved("in")?;
                let mut arms = Vec::new();
                loop {
                    self.skip_newlines();
                    if self.reserved() == Some("esac") {
                        self.pos += 1;
                        break;
                    }
                    if self.is_op(Op::LParen) {
                        self.pos += 1;
                    }
                    let mut patterns = Vec::new();
                    loop {
                        match self.next() {
                            Some(Tok::Word(w)) => patterns.push(w),
                            None => return Err(ParseError::Incomplete),
                            Some(t) => return Err(ParseError::Syntax(format!("case: bad pattern near {}", describe(&t)))),
                        }
                        match self.next() {
                            Some(Tok::Op(Op::Pipe)) => continue,
                            Some(Tok::Op(Op::RParen)) => break,
                            None => return Err(ParseError::Incomplete),
                            Some(t) => return Err(ParseError::Syntax(format!("case: expected ')' near {}", describe(&t)))),
                        }
                    }
                    let body = self.list(&["esac"])?;
                    arms.push((patterns, body));
                    self.skip_newlines();
                    if self.is_op(Op::DSemi) {
                        self.pos += 1;
                    } else if self.reserved() != Some("esac") {
                        return match self.peek() {
                            None => Err(ParseError::Incomplete),
                            Some(t) => Err(ParseError::Syntax(format!("case: expected ';;' near {}", describe(t)))),
                        };
                    }
                }
                Ok(Compound::Case { word, arms })
            }
            _ => Err(ParseError::Syntax("expected a command".into())),
        }
    }

    fn redirect(&mut self, mut r: Redir) -> Result<Redir, ParseError> {
        if !matches!(r.kind, RedirKind::ToFd(_) | RedirKind::HereDoc { .. }) {
            match self.next() {
                Some(Tok::Word(w)) => r.target = w,
                None => return Err(ParseError::Syntax("syntax error: missing file name after redirection".into())),
                Some(t) => return Err(ParseError::Syntax(format!("syntax error: bad redirection near {}", describe(&t)))),
            }
        }
        Ok(r)
    }

    fn redirects(&mut self) -> Result<Vec<Redir>, ParseError> {
        let mut out = Vec::new();
        while let Some(Tok::Redir(r)) = self.peek().cloned() {
            self.pos += 1;
            out.push(self.redirect(r)?);
        }
        Ok(out)
    }

    fn simple(&mut self) -> Result<Cmd, ParseError> {
        let mut cmd = Simple::default();
        loop {
            // An alias: the command name (also after NAME=value), or the
            // word after an alias ending with a blank.
            if matches!(self.peek(), Some(Tok::Word(_))) {
                let first = cmd.words.is_empty()
                    && !matches!(self.peek(), Some(Tok::Word(w)) if as_assignment(w).is_some());
                if first || self.alias_next == Some(self.pos) {
                    if self.alias_next == Some(self.pos) {
                        self.alias_next = None;
                    }
                    if self.expand_alias()? {
                        continue;
                    }
                }
            }
            match self.peek().cloned() {
                Some(Tok::Word(w)) => {
                    self.pos += 1;
                    if cmd.words.is_empty() {
                        if let Some(assign) = as_assignment(&w) {
                            cmd.assigns.push(assign);
                            continue;
                        }
                    }
                    cmd.words.push(w);
                }
                Some(Tok::Redir(r)) => {
                    self.pos += 1;
                    let r = self.redirect(r)?;
                    cmd.redirs.push(r);
                }
                _ => break,
            }
        }
        if cmd.words.is_empty() && cmd.assigns.is_empty() && cmd.redirs.is_empty() {
            return match self.peek() {
                None => Err(ParseError::Incomplete),
                Some(t) => Err(ParseError::Syntax(format!("syntax error near {}", describe(t)))),
            };
        }
        Ok(Cmd::Simple(cmd))
    }
}

fn vec_of<T>(x: T) -> Vec<T> {
    let mut v = Vec::new();
    v.push(x);
    v
}

/// A word that is a NAME=value assignment.
fn as_assignment(word: &Word) -> Option<(String, Word)> {
    let Some(Part::Lit(first)) = word.first() else { return None };
    let eq = first.find('=')?;
    let name = &first[..eq];
    if !is_name(name) {
        return None;
    }
    let mut value = Vec::new();
    if eq + 1 < first.len() {
        value.push(Part::Lit(first[eq + 1..].to_string()));
    }
    value.extend(word[1..].iter().cloned());
    Some((name.to_string(), value))
}
