// Copyright (c) 2026 tre4surehunter9
//! Word expansion: parameters, command substitution, arithmetic, field
//! splitting and globbing (file name patterns).

use alloc::format;
use alloc::string::{String, ToString};
use alloc::vec::Vec;
use userlib::*;

use crate::lex::{ParamOp, Part, Word};
use crate::shell::Shell;

/// A character of an expanded word, and whether it was quoted (quoted
/// characters are never glob patterns and never split).
type Ch = (char, bool);

impl Shell {
    /// Expand a word to a single string: no field splitting, no globbing
    /// (assignments, redirection targets, case words, here-strings).
    pub fn expand_str(&mut self, word: &Word) -> String {
        let mut out = String::new();
        for part in word {
            match part {
                Part::Lit(s) | Part::Quoted(s) => out.push_str(s),
                Part::Home => out.push_str(&self.get("HOME")),
                Part::Param { name, op, .. } => {
                    if name == "@" || name == "*" {
                        out.push_str(&self.params[1..].join(" "));
                    } else {
                        out.push_str(&self.param(name, op));
                    }
                }
                Part::CmdSub { src, .. } => out.push_str(&self.command_substitution(src)),
                Part::Arith { src, .. } => out.push_str(&self.arithmetic(src)),
            }
        }
        out
    }

    /// Expand a word to fields: unquoted expansions are split on whitespace,
    /// and fields with unquoted *, ? or [ are matched against file names.
    pub fn expand_fields(&mut self, word: &Word) -> Vec<String> {
        let mut fields: Vec<Vec<Ch>> = Vec::new();
        let mut cur: Vec<Ch> = Vec::new();
        // Whether `cur` is a field even if empty ("" or '' makes one).
        let mut have = false;

        for part in word {
            match part {
                Part::Lit(s) => {
                    cur.extend(s.chars().map(|c| (c, false)));
                    have = true;
                }
                Part::Quoted(s) => {
                    cur.extend(s.chars().map(|c| (c, true)));
                    have = true;
                }
                Part::Home => {
                    let home = self.get("HOME");
                    cur.extend(home.chars().map(|c| (c, true)));
                    have = true;
                }
                Part::Param { name, quoted: true, .. } if name == "@" => {
                    // "$@": one field per parameter
                    let params: Vec<String> = self.params[1..].to_vec();
                    for (i, p) in params.iter().enumerate() {
                        if i > 0 {
                            fields.push(core::mem::take(&mut cur));
                        }
                        cur.extend(p.chars().map(|c| (c, true)));
                        have = true;
                    }
                }
                Part::Param { name, op, quoted } => {
                    let value =
                        if name == "@" || name == "*" { self.params[1..].join(" ") } else { self.param(name, op) };
                    self.add_value(&value, *quoted, &mut fields, &mut cur, &mut have);
                }
                Part::CmdSub { src, quoted } => {
                    let value = self.command_substitution(src);
                    self.add_value(&value, *quoted, &mut fields, &mut cur, &mut have);
                }
                Part::Arith { src, quoted } => {
                    let value = self.arithmetic(src);
                    self.add_value(&value, *quoted, &mut fields, &mut cur, &mut have);
                }
            }
        }
        if have || !cur.is_empty() {
            fields.push(cur);
        }

        let mut out = Vec::new();
        for field in fields {
            if field.iter().any(|&(c, q)| !q && matches!(c, '*' | '?' | '[')) {
                let matches = glob(&field);
                if !matches.is_empty() {
                    out.extend(matches);
                    continue;
                }
            }
            out.push(field.iter().map(|&(c, _)| c).collect());
        }
        out
    }

    /// Add an expansion's value: quoted, it's plain text; unquoted, it's
    /// split into fields on whitespace.
    fn add_value(&self, value: &str, quoted: bool, fields: &mut Vec<Vec<Ch>>, cur: &mut Vec<Ch>, have: &mut bool) {
        if quoted {
            cur.extend(value.chars().map(|c| (c, true)));
            *have = true;
            return;
        }
        let mut chars = value.chars().peekable();
        let starts_with_space = value.starts_with(|c: char| c.is_ascii_whitespace());
        if starts_with_space && (*have || !cur.is_empty()) {
            fields.push(core::mem::take(cur));
            *have = false;
        }
        let mut in_space = false;
        while let Some(c) = chars.next() {
            if c.is_ascii_whitespace() {
                in_space = true;
                continue;
            }
            if in_space && (*have || !cur.is_empty()) {
                fields.push(core::mem::take(cur));
                *have = false;
            }
            in_space = false;
            cur.push((c, false));
        }
        if in_space && (*have || !cur.is_empty()) {
            fields.push(core::mem::take(cur));
            *have = false;
        }
    }

    /// $NAME and the ${NAME:-word} family.
    fn param(&mut self, name: &str, op: &ParamOp) -> String {
        let set = self.is_set(name);
        let value = self.get(name);
        match op {
            ParamOp::Plain => value,
            ParamOp::Length => value.chars().count().to_string(),
            ParamOp::Default { colon, word } => {
                if !set || (*colon && value.is_empty()) {
                    self.expand_str(word)
                } else {
                    value
                }
            }
            ParamOp::Assign { colon, word } => {
                if !set || (*colon && value.is_empty()) {
                    let v = self.expand_str(word);
                    self.set(name, v.clone());
                    v
                } else {
                    value
                }
            }
            ParamOp::Alternative { colon, word } => {
                if set && !(*colon && value.is_empty()) {
                    self.expand_str(word)
                } else {
                    String::new()
                }
            }
            ParamOp::Error { colon, word } => {
                if !set || (*colon && value.is_empty()) {
                    let msg = self.expand_str(word);
                    eprintln!("sh: {}: {}", name, if msg.is_empty() { "parameter not set" } else { msg.as_str() });
                    if !self.interactive {
                        self.exit_requested = Some(1);
                    }
                    self.expansion_failed = true;
                }
                value
            }
        }
    }

    /// $(commands): run them in a subshell and capture standard output
    /// (without trailing newlines).
    pub fn command_substitution(&mut self, src: &str) -> String {
        let (r, w) = match pipe2(O_CLOEXEC) {
            Ok(fds) => fds,
            Err(e) => {
                eprintln!("sh: pipe: {}", strerror(-e));
                return String::new();
            }
        };
        let pid = fork();
        if pid < 0 {
            eprintln!("sh: fork: {}", strerror(-pid));
            close(r);
            close(w);
            return String::new();
        }
        if pid == 0 {
            close(r);
            dup2(w, STDOUT);
            close(w);
            self.become_subshell();
            self.run_text(src);
            exit(self.exit_status());
        }
        close(w);
        let data = userlib::io::read_to_end(r).unwrap_or_default();
        close(r);
        let mut status = 0;
        waitpid(pid as i32, &mut status, 0);
        self.status = crate::shell_status(status);
        let mut text = String::from_utf8_lossy(&data).into_owned();
        while text.ends_with('\n') {
            text.pop();
        }
        text
    }

    /// $((expression)): integer arithmetic.
    pub fn arithmetic(&mut self, src: &str) -> String {
        // Parameters inside are expanded first ($x, $(cmd)).
        let expanded = match crate::lex::lex_word_text(src) {
            Ok(word) => self.expand_str(&word),
            Err(_) => src.to_string(),
        };
        let mut a = Arith { s: expanded.as_bytes(), i: 0, shell: self };
        let result = a.expr();
        match result {
            Ok(v) if a.done() => v.to_string(),
            Ok(_) => {
                eprintln!("sh: arithmetic syntax error: {}", expanded.trim());
                self.expansion_failed = true;
                "0".to_string()
            }
            Err(e) => {
                eprintln!("sh: {}: {}", expanded.trim(), e);
                self.expansion_failed = true;
                "0".to_string()
            }
        }
    }
}

// ---------------------------------------------------------------- globbing

/// Does `name` match the glob `pattern`? Quoted pattern characters only
/// match themselves. Supports * ? [abc] [a-z] [!abc].
pub fn glob_match(pattern: &[Ch], name: &[char]) -> bool {
    match pattern.first() {
        None => name.is_empty(),
        Some(&('*', false)) => (0..=name.len()).any(|i| glob_match(&pattern[1..], &name[i..])),
        Some(&('?', false)) => !name.is_empty() && glob_match(&pattern[1..], &name[1..]),
        Some(&('[', false)) => {
            let Some(&c) = name.first() else { return false };
            match bracket(&pattern[1..], c) {
                Some((matched, len)) => matched && glob_match(&pattern[1 + len..], &name[1..]),
                // no closing ']': a literal '['
                None => c == '[' && glob_match(&pattern[1..], &name[1..]),
            }
        }
        Some(&(p, _)) => name.first() == Some(&p) && glob_match(&pattern[1..], &name[1..]),
    }
}

/// Match `c` against a bracket expression (after the '['). Returns
/// (matched, characters used including the ']').
fn bracket(p: &[Ch], c: char) -> Option<(bool, usize)> {
    let mut i = 0;
    let negate = matches!(p.first(), Some(&('!', false)) | Some(&('^', false)));
    if negate {
        i += 1;
    }
    let mut matched = false;
    let mut first = true;
    while i < p.len() {
        let (pc, _) = p[i];
        if pc == ']' && !first {
            return Some((matched != negate, i + 1));
        }
        first = false;
        if i + 2 < p.len() && p[i + 1].0 == '-' && p[i + 2].0 != ']' {
            if pc <= c && c <= p[i + 2].0 {
                matched = true;
            }
            i += 3;
        } else {
            if pc == c {
                matched = true;
            }
            i += 1;
        }
    }
    None
}

/// Expand a glob pattern to the matching paths, sorted.
fn glob(pattern: &[Ch]) -> Vec<String> {
    let absolute = pattern.first().is_some_and(|&(c, _)| c == '/');
    let components: Vec<&[Ch]> = pattern.split(|&(c, _)| c == '/').filter(|c| !c.is_empty()).collect();
    let mut paths: Vec<String> = Vec::new();
    paths.push(if absolute { "/".to_string() } else { String::new() });

    for (i, comp) in components.iter().enumerate() {
        let last = i + 1 == components.len();
        let has_glob = comp.iter().any(|&(c, q)| !q && matches!(c, '*' | '?' | '['));
        let mut next = Vec::new();
        for base in &paths {
            if has_glob {
                let dir = if base.is_empty() { ".".to_string() } else { base.clone() };
                let fd = open(&dir, O_RDONLY | O_DIRECTORY);
                if fd < 0 {
                    continue;
                }
                let mut names = Vec::new();
                let _ = for_each_entry(fd as i32, |e| {
                    if e.name == "." || e.name == ".." {
                        return;
                    }
                    // Hidden files only match a pattern starting with '.'
                    if e.name.starts_with('.') && comp.first().map(|c| c.0) != Some('.') {
                        return;
                    }
                    if !last && e.d_type != DT_DIR {
                        return;
                    }
                    let name: Vec<char> = e.name.chars().collect();
                    if glob_match(comp, &name) {
                        names.push(e.name.to_string());
                    }
                });
                close(fd as i32);
                names.sort();
                for n in names {
                    next.push(join(base, &n));
                }
            } else {
                let name: String = comp.iter().map(|&(c, _)| c).collect();
                let path = join(base, &name);
                if stat(&path).is_ok() {
                    next.push(path);
                }
            }
        }
        paths = next;
        if paths.is_empty() {
            break;
        }
    }
    paths
}

fn join(base: &str, name: &str) -> String {
    if base.is_empty() {
        name.to_string()
    } else if base.ends_with('/') {
        format!("{}{}", base, name)
    } else {
        format!("{}/{}", base, name)
    }
}

// ---------------------------------------------------------------- arithmetic

/// A small recursive-descent evaluator for $((...)): integers, variables,
/// + - * / %, comparisons, && || !, unary minus, parentheses, and
/// assignments (x = 1, x += 2).
struct Arith<'a> {
    s: &'a [u8],
    i: usize,
    shell: &'a mut Shell,
}

type R = Result<i64, &'static str>;

impl Arith<'_> {
    fn ws(&mut self) {
        while self.i < self.s.len() && self.s[self.i].is_ascii_whitespace() {
            self.i += 1;
        }
    }

    fn done(&mut self) -> bool {
        self.ws();
        self.i == self.s.len()
    }

    fn eat(&mut self, op: &str) -> bool {
        self.ws();
        if self.s[self.i..].starts_with(op.as_bytes()) {
            // don't take "<" from "<=" etc.
            let next = self.s.get(self.i + op.len()).copied();
            if (op == "<" || op == ">" || op == "!" || op == "=") && next == Some(b'=') {
                return false;
            }
            if (op == "&" && next == Some(b'&')) || (op == "|" && next == Some(b'|')) {
                return false;
            }
            self.i += op.len();
            return true;
        }
        false
    }

    fn expr(&mut self) -> R {
        // assignment: NAME = expr, NAME += expr, ...
        self.ws();
        let start = self.i;
        let mut j = self.i;
        while j < self.s.len() && (self.s[j].is_ascii_alphanumeric() || self.s[j] == b'_') {
            j += 1;
        }
        if j > start && !self.s[start].is_ascii_digit() {
            let name = core::str::from_utf8(&self.s[start..j]).unwrap_or("").to_string();
            self.i = j;
            for op in ["+=", "-=", "*=", "/=", "%=", "="] {
                if self.eat(op) {
                    let rhs = self.expr()?;
                    let old = self.var(&name)?;
                    let v = match op {
                        "=" => rhs,
                        "+=" => old.wrapping_add(rhs),
                        "-=" => old.wrapping_sub(rhs),
                        "*=" => old.wrapping_mul(rhs),
                        "/=" => old.checked_div(rhs).ok_or("division by zero")?,
                        _ => old.checked_rem(rhs).ok_or("division by zero")?,
                    };
                    self.shell.set(&name, v.to_string());
                    return Ok(v);
                }
            }
            self.i = start;
        }
        self.or()
    }

    fn or(&mut self) -> R {
        let mut v = self.and()?;
        while self.eat("||") {
            let r = self.and()?;
            v = (v != 0 || r != 0) as i64;
        }
        Ok(v)
    }

    fn and(&mut self) -> R {
        let mut v = self.cmp_eq()?;
        while self.eat("&&") {
            let r = self.cmp_eq()?;
            v = (v != 0 && r != 0) as i64;
        }
        Ok(v)
    }

    fn cmp_eq(&mut self) -> R {
        let mut v = self.cmp()?;
        loop {
            if self.eat("==") {
                v = (v == self.cmp()?) as i64;
            } else if self.eat("!=") {
                v = (v != self.cmp()?) as i64;
            } else {
                return Ok(v);
            }
        }
    }

    fn cmp(&mut self) -> R {
        let mut v = self.sum()?;
        loop {
            if self.eat("<=") {
                v = (v <= self.sum()?) as i64;
            } else if self.eat(">=") {
                v = (v >= self.sum()?) as i64;
            } else if self.eat("<") {
                v = (v < self.sum()?) as i64;
            } else if self.eat(">") {
                v = (v > self.sum()?) as i64;
            } else {
                return Ok(v);
            }
        }
    }

    fn sum(&mut self) -> R {
        let mut v = self.product()?;
        loop {
            if self.eat("+") {
                v = v.wrapping_add(self.product()?);
            } else if self.eat("-") {
                v = v.wrapping_sub(self.product()?);
            } else {
                return Ok(v);
            }
        }
    }

    fn product(&mut self) -> R {
        let mut v = self.unary()?;
        loop {
            if self.eat("*") {
                v = v.wrapping_mul(self.unary()?);
            } else if self.eat("/") {
                let r = self.unary()?;
                v = v.checked_div(r).ok_or("division by zero")?;
            } else if self.eat("%") {
                let r = self.unary()?;
                v = v.checked_rem(r).ok_or("division by zero")?;
            } else {
                return Ok(v);
            }
        }
    }

    fn unary(&mut self) -> R {
        if self.eat("-") {
            return Ok(self.unary()?.wrapping_neg());
        }
        if self.eat("+") {
            return self.unary();
        }
        if self.eat("!") {
            return Ok((self.unary()? == 0) as i64);
        }
        self.primary()
    }

    fn primary(&mut self) -> R {
        self.ws();
        if self.eat("(") {
            let v = self.expr()?;
            if !self.eat(")") {
                return Err("missing ')'");
            }
            return Ok(v);
        }
        let start = self.i;
        if self.i < self.s.len() && self.s[self.i].is_ascii_digit() {
            while self.i < self.s.len() && self.s[self.i].is_ascii_digit() {
                self.i += 1;
            }
            let text = core::str::from_utf8(&self.s[start..self.i]).unwrap_or("0");
            return text.parse().map_err(|_| "number too big");
        }
        while self.i < self.s.len() && (self.s[self.i].is_ascii_alphanumeric() || self.s[self.i] == b'_') {
            self.i += 1;
        }
        if self.i == start {
            return Err("syntax error");
        }
        let name = core::str::from_utf8(&self.s[start..self.i]).unwrap_or("").to_string();
        self.var(&name)
    }

    /// A variable's value as a number (unset or empty is 0).
    fn var(&mut self, name: &str) -> R {
        let v = self.shell.get(name);
        let v = v.trim();
        if v.is_empty() {
            return Ok(0);
        }
        v.parse().map_err(|_| "not a number")
    }
}
