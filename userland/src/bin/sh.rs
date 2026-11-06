// Copyright (c) 2026 tre4surehunter9
//! sh: the PalladiumOS shell, a small POSIX-style shell.
//!
//!   sh                 interactive (reads commands from standard input)
//!   sh FILE [ARGS...]  run a script ($0 = FILE, $1... = ARGS)
//!   sh -c 'COMMANDS'   run COMMANDS
//!
//! Supported:
//!   'single quotes', "double quotes", \ escapes, # comments
//!   $VAR ${VAR} $? $$ $# $0..$9, ~ at the start of a word
//!   NAME=value (shell variable), NAME=value cmd (for one command only)
//!   cmd1 ; cmd2   cmd1 && cmd2   cmd1 || cmd2   cmd &  (background)
//!   > file   >> file   < file   2> file   2>> file   2>&1
//!   builtins: cd pwd exit export unset set wait jobs . source help true false
//!   interactive line editing: arrows, Home/End, history (Up/Down), Tab
//!   completion, Ctrl-A/E/U/K/L/C
//! Not yet: pipes, globbing, if/while/for, functions, field splitting.
#![no_std]
#![no_main]

extern crate alloc;

use alloc::collections::BTreeMap;
use alloc::format;
use alloc::string::{String, ToString};
use alloc::vec;
use alloc::vec::Vec;
use userlib::*;

userlib::entry!(main);

// ---------------------------------------------------------------- parsing

/// A piece of a word, before expansion.
#[derive(Debug, Clone, PartialEq)]
enum Part {
    Text(String),
    /// $NAME, ${NAME}, $?, $$, $#, $0-$9
    Var(String),
    /// ~ at the start of an unquoted word
    Home,
}

type Word = Vec<Part>;

#[derive(Debug, Clone, PartialEq)]
enum Token {
    Word(Word),
    Semi,      // ;
    Amp,       // &
    AndAnd,    // &&
    OrOr,      // ||
    Pipe,      // |
    Redirect(Redir),
}

#[derive(Debug, Clone, Copy, PartialEq)]
enum RedirKind {
    Out,     // >
    Append,  // >>
    In,      // <
    ToFd(i32), // >&N
}

#[derive(Debug, Clone, PartialEq)]
struct Redir {
    fd: i32,
    kind: RedirKind,
    /// The target file (empty for ToFd).
    target: Word,
}

fn is_name_char(c: char) -> bool {
    c.is_ascii_alphanumeric() || c == '_'
}

/// Split a line into tokens. Quotes and escapes are resolved here;
/// variables are kept as `Part::Var` and expanded when the command runs.
fn tokenize(line: &str) -> Result<Vec<Token>, String> {
    let chars: Vec<char> = line.chars().collect();
    let mut tokens = Vec::new();
    let mut word: Word = Vec::new();
    let mut text = String::new();
    let mut in_word = false;
    let mut i = 0;

    // Move pending literal text into the current word.
    fn flush_text(word: &mut Word, text: &mut String) {
        if !text.is_empty() {
            word.push(Part::Text(core::mem::take(text)));
        }
    }
    fn end_word(tokens: &mut Vec<Token>, word: &mut Word, text: &mut String, in_word: &mut bool) {
        flush_text(word, text);
        if *in_word {
            tokens.push(Token::Word(core::mem::take(word)));
        }
        *in_word = false;
    }

    // Parse a $... expansion starting at chars[i] == '$'. Returns the part
    // and the index after it, or None if '$' is just a literal.
    let parse_var = |i: usize| -> Result<Option<(Part, usize)>, String> {
        match chars.get(i + 1) {
            Some('{') => {
                let end = chars[i + 2..].iter().position(|&c| c == '}').ok_or("missing '}'")?;
                let name: String = chars[i + 2..i + 2 + end].iter().collect();
                if name.is_empty() {
                    return Err("bad substitution".into());
                }
                Ok(Some((Part::Var(name), i + 3 + end)))
            }
            Some(&c) if c == '?' || c == '$' || c == '#' || c.is_ascii_digit() => {
                Ok(Some((Part::Var(c.to_string()), i + 2)))
            }
            Some(&c) if is_name_char(c) => {
                let mut j = i + 1;
                while j < chars.len() && is_name_char(chars[j]) {
                    j += 1;
                }
                Ok(Some((Part::Var(chars[i + 1..j].iter().collect()), j)))
            }
            _ => Ok(None),
        }
    };

    while i < chars.len() {
        let c = chars[i];
        match c {
            ' ' | '\t' | '\r' | '\n' => {
                end_word(&mut tokens, &mut word, &mut text, &mut in_word);
                i += 1;
            }
            '#' if !in_word => break, // comment to end of line
            '\'' => {
                in_word = true;
                let end = chars[i + 1..].iter().position(|&c| c == '\'').ok_or("unterminated '")?;
                text.extend(&chars[i + 1..i + 1 + end]);
                i += end + 2;
            }
            '"' => {
                in_word = true;
                i += 1;
                loop {
                    match chars.get(i) {
                        None => return Err("unterminated \"".into()),
                        Some('"') => {
                            i += 1;
                            break;
                        }
                        Some('\\') if matches!(chars.get(i + 1), Some('"' | '\\' | '$' | '`')) => {
                            text.push(chars[i + 1]);
                            i += 2;
                        }
                        Some('$') => match parse_var(i)? {
                            Some((part, next)) => {
                                flush_text(&mut word, &mut text);
                                word.push(part);
                                i = next;
                            }
                            None => {
                                text.push('$');
                                i += 1;
                            }
                        },
                        Some(&c) => {
                            text.push(c);
                            i += 1;
                        }
                    }
                }
            }
            '\\' => {
                in_word = true;
                if let Some(&next) = chars.get(i + 1) {
                    text.push(next);
                }
                i += 2;
            }
            '$' => match parse_var(i)? {
                Some((part, next)) => {
                    in_word = true;
                    flush_text(&mut word, &mut text);
                    word.push(part);
                    i = next;
                }
                None => {
                    in_word = true;
                    text.push('$');
                    i += 1;
                }
            },
            // ~ alone or ~/path (not ~user): $HOME
            '~' if !in_word && matches!(chars.get(i + 1), None | Some('/' | ' ' | '\t' | ';' | '&' | '|' | '<' | '>')) => {
                in_word = true;
                word.push(Part::Home);
                i += 1;
            }
            ';' | '&' | '|' => {
                end_word(&mut tokens, &mut word, &mut text, &mut in_word);
                let double = chars.get(i + 1) == Some(&c);
                tokens.push(match (c, double) {
                    (';', _) => Token::Semi,
                    ('&', true) => Token::AndAnd,
                    ('&', false) => Token::Amp,
                    ('|', true) => Token::OrOr,
                    _ => Token::Pipe,
                });
                i += if double && c != ';' { 2 } else { 1 };
            }
            '>' | '<' => {
                // A word made only of digits right before > or < is the fd: 2>file
                let mut fd = if c == '>' { 1 } else { 0 };
                if in_word && word.is_empty() && !text.is_empty() && text.chars().all(|d| d.is_ascii_digit()) {
                    fd = text.parse().map_err(|_| "bad file descriptor")?;
                    text.clear();
                    in_word = false;
                } else {
                    end_word(&mut tokens, &mut word, &mut text, &mut in_word);
                }
                i += 1;
                let kind = if c == '<' {
                    RedirKind::In
                } else if chars.get(i) == Some(&'>') {
                    i += 1;
                    RedirKind::Append
                } else if chars.get(i) == Some(&'&') {
                    let start = i + 1;
                    let mut j = start;
                    while j < chars.len() && chars[j].is_ascii_digit() {
                        j += 1;
                    }
                    let n: String = chars[start..j].iter().collect();
                    let target = n.parse().map_err(|_| format!("bad file descriptor after >&"))?;
                    i = j;
                    RedirKind::ToFd(target)
                } else {
                    RedirKind::Out
                };
                tokens.push(Token::Redirect(Redir { fd, kind, target: Vec::new() }));
            }
            _ => {
                in_word = true;
                text.push(c);
                i += 1;
            }
        }
    }
    end_word(&mut tokens, &mut word, &mut text, &mut in_word);

    // Attach each redirection's target word.
    let mut out = Vec::new();
    let mut iter = tokens.into_iter().peekable();
    while let Some(tok) = iter.next() {
        match tok {
            Token::Redirect(mut r) if !matches!(r.kind, RedirKind::ToFd(_)) => match iter.next() {
                Some(Token::Word(w)) => {
                    r.target = w;
                    out.push(Token::Redirect(r));
                }
                _ => return Err("syntax error: missing file name after redirection".into()),
            },
            other => out.push(other),
        }
    }
    Ok(out)
}

#[derive(Debug, Default)]
struct Command {
    assignments: Vec<(String, Word)>,
    words: Vec<Word>,
    redirs: Vec<Redir>,
}

#[derive(Debug, Clone, Copy, PartialEq)]
enum Connector {
    And,
    Or,
}

/// cmd1 && cmd2 || cmd3 ...
#[derive(Debug)]
struct AndOr {
    first: Command,
    rest: Vec<(Connector, Command)>,
    background: bool,
}

/// A word that is a plain NAME=value assignment.
fn as_assignment(word: &Word) -> Option<(String, Word)> {
    let Some(Part::Text(first)) = word.first() else { return None };
    let eq = first.find('=')?;
    let name = &first[..eq];
    if name.is_empty() || !name.chars().all(is_name_char) || name.starts_with(|c: char| c.is_ascii_digit()) {
        return None;
    }
    let mut value = Vec::new();
    if eq + 1 < first.len() {
        value.push(Part::Text(first[eq + 1..].to_string()));
    }
    value.extend(word[1..].iter().cloned());
    Some((name.to_string(), value))
}

fn parse(tokens: Vec<Token>) -> Result<Vec<AndOr>, String> {
    let mut list = Vec::new();
    let mut cmd = Command::default();
    let mut current: Option<AndOr> = None;
    let mut pending: Option<Connector> = None;

    fn finish(cmd: &mut Command, current: &mut Option<AndOr>, pending: &mut Option<Connector>) -> Result<(), String> {
        let done = core::mem::take(cmd);
        let empty = done.words.is_empty() && done.assignments.is_empty() && done.redirs.is_empty();
        match (current.as_mut(), pending.take()) {
            (None, _) if empty => {}
            (None, _) => *current = Some(AndOr { first: done, rest: Vec::new(), background: false }),
            (Some(_), Some(_)) if empty => return Err("syntax error: missing command after && or ||".into()),
            (Some(ao), Some(conn)) => ao.rest.push((conn, done)),
            (Some(_), None) if empty => {}
            (Some(_), None) => return Err("syntax error".into()),
        }
        Ok(())
    }

    for tok in tokens {
        match tok {
            Token::Word(w) => {
                if cmd.words.is_empty() {
                    if let Some(assign) = as_assignment(&w) {
                        cmd.assignments.push(assign);
                        continue;
                    }
                }
                cmd.words.push(w);
            }
            Token::Redirect(r) => cmd.redirs.push(r),
            Token::AndAnd | Token::OrOr => {
                if cmd.words.is_empty() && cmd.assignments.is_empty() {
                    return Err("syntax error near && or ||".into());
                }
                finish(&mut cmd, &mut current, &mut pending)?;
                pending = Some(if tok == Token::AndAnd { Connector::And } else { Connector::Or });
            }
            Token::Semi | Token::Amp => {
                finish(&mut cmd, &mut current, &mut pending)?;
                if pending.is_some() {
                    return Err("syntax error: missing command after && or ||".into());
                }
                match current.take() {
                    Some(mut ao) => {
                        ao.background = tok == Token::Amp;
                        list.push(ao);
                    }
                    None if tok == Token::Amp => return Err("syntax error near &".into()),
                    None => {}
                }
            }
            Token::Pipe => return Err("pipes (|) are not supported yet".into()),
        }
    }
    finish(&mut cmd, &mut current, &mut pending)?;
    if pending.is_some() {
        return Err("syntax error: missing command after && or ||".into());
    }
    if let Some(ao) = current {
        list.push(ao);
    }
    Ok(list)
}

// ---------------------------------------------------------------- the shell

struct Var {
    value: String,
    exported: bool,
}

struct Shell {
    vars: BTreeMap<String, Var>,
    /// $0, $1, ...
    params: Vec<String>,
    status: i32,
    interactive: bool,
    /// Background jobs not yet reported.
    jobs: Vec<i32>,
    /// Set by `exit`.
    exit_requested: Option<i32>,
}

/// Builtins that must run in the shell process itself.
const BUILTINS: &[&str] = &[
    "cd", "pwd", "exit", "export", "unset", "set", "wait", "jobs", "kill", ".", "source", "help", "true", "false", "run",
    "script",
];

impl Shell {
    fn new(params: Vec<String>, interactive: bool) -> Self {
        let mut vars = BTreeMap::new();
        for kv in environ() {
            if let Some((k, v)) = kv.split_once('=') {
                vars.insert(k.to_string(), Var { value: v.to_string(), exported: true });
            }
        }
        for (k, v) in [("PATH", "/bin"), ("HOME", "/")] {
            vars.entry(k.to_string()).or_insert(Var { value: v.to_string(), exported: true });
        }
        Shell { vars, params, status: 0, interactive, jobs: Vec::new(), exit_requested: None }
    }

    fn get(&self, name: &str) -> String {
        match name {
            "?" => self.status.to_string(),
            "$" => getpid().to_string(),
            "#" => self.params.len().saturating_sub(1).to_string(),
            n if n.len() == 1 && n.as_bytes()[0].is_ascii_digit() => {
                self.params.get((n.as_bytes()[0] - b'0') as usize).cloned().unwrap_or_default()
            }
            "PWD" => cwd(),
            n => self.vars.get(n).map(|v| v.value.clone()).unwrap_or_default(),
        }
    }

    fn set(&mut self, name: &str, value: String) {
        let exported = self.vars.get(name).is_some_and(|v| v.exported);
        self.vars.insert(name.to_string(), Var { value, exported });
    }

    fn expand(&self, word: &Word) -> String {
        let mut out = String::new();
        for part in word {
            match part {
                Part::Text(t) => out.push_str(t),
                Part::Var(name) => out.push_str(&self.get(name)),
                Part::Home => out.push_str(&self.get("HOME")),
            }
        }
        out
    }

    /// The environment for a child: exported variables plus per-command
    /// assignments.
    fn child_env(&self, extra: &[(String, String)]) -> Vec<String> {
        let mut env: BTreeMap<&str, &str> = self
            .vars
            .iter()
            .filter(|(_, v)| v.exported)
            .map(|(k, v)| (k.as_str(), v.value.as_str()))
            .collect();
        for (k, v) in extra {
            env.insert(k, v);
        }
        env.iter().map(|(k, v)| format!("{}={}", k, v)).collect()
    }

    /// Find a program: names with '/' are paths, others are searched in $PATH.
    fn find_program(&self, name: &str) -> Option<String> {
        if name.contains('/') {
            return stat(name).ok().filter(|s| s.is_file()).map(|_| name.to_string());
        }
        let path = self.get("PATH");
        for dir in path.split(':') {
            let dir = if dir.is_empty() { "." } else { dir };
            let candidate = format!("{}/{}", dir.trim_end_matches('/'), name);
            if stat(&candidate).is_ok_and(|s| s.is_file()) {
                return Some(candidate);
            }
        }
        None
    }

    /// Run a whole line (or script text).
    fn run_text(&mut self, text: &str) {
        for line in text.lines() {
            if self.exit_requested.is_some() {
                return;
            }
            self.run_line(line);
        }
    }

    fn run_line(&mut self, line: &str) {
        let list = match tokenize(line).and_then(parse) {
            Ok(list) => list,
            Err(e) => {
                eprintln!("sh: {}", e);
                self.status = 2;
                return;
            }
        };
        for and_or in list {
            if self.exit_requested.is_some() {
                return;
            }
            if and_or.background {
                self.run_background(and_or);
            } else {
                self.run_and_or(&and_or);
            }
        }
    }

    fn run_and_or(&mut self, ao: &AndOr) {
        self.run_command(&ao.first);
        for (conn, cmd) in &ao.rest {
            if self.exit_requested.is_some() {
                return;
            }
            let run = match conn {
                Connector::And => self.status == 0,
                Connector::Or => self.status != 0,
            };
            if run {
                self.run_command(cmd);
            }
        }
    }

    /// `cmd &`: run the whole and-or list in a forked copy of the shell.
    fn run_background(&mut self, ao: AndOr) {
        let pid = fork();
        if pid < 0 {
            eprintln!("sh: fork: {}", strerror(-pid));
            self.status = 1;
            return;
        }
        if pid == 0 {
            if self.interactive {
                // Its own process group, so Ctrl-C (sent to the terminal's
                // foreground group) doesn't reach it.
                setpgid(0, 0);
                set_signal(SIGINT, SIG_DFL);
            }
            self.interactive = false;
            self.run_and_or(&ao);
            exit(self.exit_requested.unwrap_or(self.status));
        }
        if self.interactive {
            setpgid(pid as i32, pid as i32); // also in the child: whichever runs first
            println!("[{}] {}", self.jobs.len() + 1, pid);
        }
        self.jobs.push(pid as i32);
        self.status = 0;
    }

    /// Report background jobs that have finished (before each prompt).
    fn reap_jobs(&mut self) {
        let mut status = 0;
        let mut i = 0;
        while i < self.jobs.len() {
            let pid = self.jobs[i];
            if waitpid(pid, &mut status, WNOHANG) == pid as isize {
                if self.interactive {
                    println!("[{}] {} Done ({})", i + 1, pid, shell_status(status));
                }
                self.jobs.remove(i);
            } else {
                i += 1;
            }
        }
    }

    fn run_command(&mut self, cmd: &Command) {
        let words: Vec<String> = cmd.words.iter().map(|w| self.expand(w)).collect();
        let assigns: Vec<(String, String)> =
            cmd.assignments.iter().map(|(k, v)| (k.clone(), self.expand(v))).collect();

        if words.is_empty() {
            // Only assignments (and maybe redirections): set shell variables.
            for (k, v) in assigns {
                self.set(&k, v);
            }
            if !cmd.redirs.is_empty() {
                // e.g. `> file` creates/truncates the file
                match self.apply_redirs(&cmd.redirs) {
                    Ok(saved) => restore_fds(saved),
                    Err(()) => {
                        self.status = 1;
                        return;
                    }
                }
            }
            self.status = 0;
            return;
        }

        if BUILTINS.contains(&words[0].as_str()) {
            // Builtins run in the shell, so redirect our own fds and put them back.
            let saved = match self.apply_redirs(&cmd.redirs) {
                Ok(saved) => saved,
                Err(()) => {
                    self.status = 1;
                    return;
                }
            };
            self.status = self.builtin(&words);
            restore_fds(saved);
            return;
        }

        let Some(path) = self.find_program(&words[0]) else {
            eprintln!("sh: {}: command not found", words[0]);
            self.status = 127;
            return;
        };
        let env = self.child_env(&assigns);

        let pid = fork();
        if pid < 0 {
            eprintln!("sh: fork: {}", strerror(-pid));
            self.status = 1;
            return;
        }
        if pid == 0 {
            if self.interactive {
                // A new process group that owns the terminal, so Ctrl-C
                // interrupts this command and not the shell.
                setpgid(0, 0);
                tcsetpgrp(STDIN, getpid());
                set_signal(SIGINT, SIG_DFL);
            }
            if self.apply_redirs(&cmd.redirs).is_err() {
                exit(1);
            }
            let argv: Vec<&str> = words.iter().map(String::as_str).collect();
            let envp: Vec<&str> = env.iter().map(String::as_str).collect();
            let err = execve(&path, &argv, &envp);
            eprintln!("sh: {}: {}", words[0], strerror(-err));
            exit(if err == -ENOENT { 127 } else { 126 });
        }

        if self.interactive {
            setpgid(pid as i32, pid as i32);
            tcsetpgrp(STDIN, pid as i32);
        }
        let mut status = 0;
        let r = waitpid(pid as i32, &mut status, 0);
        if self.interactive {
            tcsetpgrp(STDIN, getpgrp()); // the terminal is ours again
        }
        if r < 0 {
            self.status = 1;
            return;
        }
        self.status = shell_status(status);
        if self.interactive && wifsignaled(status) && wtermsig(status) != SIGINT {
            // Ctrl-C already printed ^C; report other killing signals.
            let name = signal_name(wtermsig(status)).unwrap_or("?");
            println!("[{}] killed by SIG{}", words[0], name);
        }
    }

    /// Apply redirections in this process. Returns the saved original fds
    /// so a builtin can restore them, or Err after printing an error.
    fn apply_redirs(&self, redirs: &[Redir]) -> Result<Vec<(i32, isize)>, ()> {
        let mut saved = Vec::new();
        for r in redirs {
            let source = match r.kind {
                RedirKind::ToFd(target) => {
                    if fstat(target).is_err() {
                        eprintln!("sh: {}: bad file descriptor", target);
                        restore_fds(saved);
                        return Err(());
                    }
                    target as isize
                }
                kind => {
                    let file = self.expand(&r.target);
                    let flags = match kind {
                        RedirKind::Out => O_WRONLY | O_CREAT | O_TRUNC,
                        RedirKind::Append => O_WRONLY | O_CREAT | O_APPEND,
                        _ => O_RDONLY,
                    };
                    let fd = open(&file, flags);
                    if fd < 0 {
                        eprintln!("sh: {}: {}", file, strerror(-fd));
                        restore_fds(saved);
                        return Err(());
                    }
                    fd
                }
            };
            saved.push((r.fd, dup(r.fd)));
            dup2(source as i32, r.fd);
            if !matches!(r.kind, RedirKind::ToFd(_)) {
                close(source as i32);
            }
        }
        Ok(saved)
    }

    fn builtin(&mut self, args: &[String]) -> i32 {
        let arg = |i: usize| args.get(i).map(String::as_str);
        match args[0].as_str() {
            "true" => 0,
            "false" => 1,
            "cd" => {
                let home = self.get("HOME");
                let target = arg(1).unwrap_or(&home);
                let r = chdir(target);
                if r < 0 {
                    eprintln!("sh: cd: {}: {}", target, strerror(-r));
                    return 1;
                }
                let pwd = cwd();
                self.set("PWD", pwd);
                0
            }
            "pwd" => {
                println!("{}", cwd());
                0
            }
            "exit" => {
                let code = match arg(1) {
                    None => self.status,
                    Some(n) => match n.parse::<i32>() {
                        Ok(n) => n & 0xff,
                        Err(_) => {
                            eprintln!("sh: exit: {}: numeric argument required", n);
                            2
                        }
                    },
                };
                self.exit_requested = Some(code);
                code
            }
            "export" => {
                if args.len() == 1 {
                    for (k, v) in self.vars.iter().filter(|(_, v)| v.exported) {
                        println!("export {}={}", k, v.value);
                    }
                }
                for a in &args[1..] {
                    let (name, value) = match a.split_once('=') {
                        Some((n, v)) => (n, Some(v.to_string())),
                        None => (a.as_str(), None),
                    };
                    let value = value.unwrap_or_else(|| self.get(name));
                    self.vars.insert(name.to_string(), Var { value, exported: true });
                }
                0
            }
            "unset" => {
                for a in &args[1..] {
                    self.vars.remove(a.as_str());
                }
                0
            }
            "set" => {
                for (k, v) in &self.vars {
                    println!("{}={}", k, v.value);
                }
                0
            }
            "jobs" => {
                for (i, pid) in self.jobs.iter().enumerate() {
                    println!("[{}] {} Running", i + 1, pid);
                }
                0
            }
            "kill" => self.kill_builtin(args),
            // Kernel-shell compatibility: `run prog args` and `script file`.
            "run" => {
                if args.len() < 2 {
                    eprintln!("usage: run <program> [args...]");
                    return 2;
                }
                let cmd = Command {
                    words: args[1..].iter().map(|a| vec![Part::Text(a.clone())]).collect(),
                    ..Default::default()
                };
                self.run_command(&cmd);
                self.status
            }
            "script" => {
                if args.len() < 2 {
                    eprintln!("usage: script <file> [args...]");
                    return 2;
                }
                let mut words = vec![vec![Part::Text("sh".into())]];
                words.extend(args[1..].iter().map(|a| vec![Part::Text(a.clone())]));
                self.run_command(&Command { words, ..Default::default() });
                self.status
            }
            "wait" => {
                let mut status = 0;
                let mut last = 0;
                for pid in core::mem::take(&mut self.jobs) {
                    if waitpid(pid, &mut status, 0) == pid as isize {
                        last = shell_status(status);
                    }
                }
                last
            }
            "." | "source" => {
                let Some(file) = arg(1) else {
                    eprintln!("sh: {}: filename argument required", args[0]);
                    return 2;
                };
                match read_file(file) {
                    Ok(text) => {
                        self.run_text(&text);
                        self.status
                    }
                    Err(e) => {
                        eprintln!("sh: {}: {}", file, strerror(-e));
                        1
                    }
                }
            }
            "help" => {
                println!("PalladiumOS sh. Builtins:");
                print_wrapped(BUILTINS.iter().copied());
                println!("Programs in /bin (run with: name [args...]):");
                let fd = open("/bin", O_RDONLY | O_DIRECTORY);
                if fd >= 0 {
                    let mut names: Vec<String> = Vec::new();
                    let _ = for_each_entry(fd as i32, |e| {
                        if !e.name.starts_with('.') && !e.name.starts_with("test_") {
                            names.push(e.name.to_string());
                        }
                    });
                    close(fd as i32);
                    names.sort();
                    print_wrapped(names.iter().map(String::as_str));
                }
                println!("Syntax: ; && || & > >> < 2> 2>&1 'quotes' \"$VAR\" ~ # comments");
                println!("Also: run <program>, script <file> (like the kernel shell)");
                println!("Keys: Up/Down history, Tab completes, Ctrl-C interrupts, Ctrl-D exits");
                println!("Type 'exit' to return to the kernel shell.");
                0
            }
            _ => 127,
        }
    }
}

impl Shell {
    /// kill [-SIGNAL] PID|%JOB... (a builtin, so it understands job numbers)
    fn kill_builtin(&mut self, args: &[String]) -> i32 {
        let mut sig = SIGTERM;
        let mut rest = &args[1..];
        if let Some(first) = rest.first() {
            if first == "-l" {
                for (n, name) in SIGNAL_NAMES {
                    println!("{:2}) SIG{}", n, name);
                }
                return 0;
            }
            if first == "--" {
                rest = &rest[1..];
            } else if let Some(spec) = first.strip_prefix('-') {
                let name = spec.strip_prefix("SIG").unwrap_or(spec);
                let parsed = spec.parse::<i32>().ok().or_else(|| {
                    SIGNAL_NAMES.iter().find(|(_, n)| n.eq_ignore_ascii_case(name)).map(|(n, _)| *n)
                });
                match parsed {
                    Some(s) => sig = s,
                    None => {
                        eprintln!("sh: kill: {}: invalid signal", spec);
                        return 1;
                    }
                }
                rest = &rest[1..];
            }
        }
        if rest.is_empty() {
            eprintln!("usage: kill [-SIGNAL] PID|%JOB...");
            return 2;
        }
        let mut status = 0;
        for target in rest {
            // %N is job N from `jobs`; a job is its own process group.
            let pid = match target.strip_prefix('%') {
                Some(n) => match n.parse::<usize>().ok().and_then(|n| self.jobs.get(n.wrapping_sub(1))) {
                    // With job control (interactive) each job is a process
                    // group; without it, signal the job's process.
                    Some(&pid) if self.interactive => -pid,
                    Some(&pid) => pid,
                    None => {
                        eprintln!("sh: kill: {}: no such job", target);
                        status = 1;
                        continue;
                    }
                },
                None => match target.parse::<i32>() {
                    Ok(pid) => pid,
                    Err(_) => {
                        eprintln!("sh: kill: {}: arguments must be process or job IDs", target);
                        status = 1;
                        continue;
                    }
                },
            };
            let r = kill(pid, sig);
            if r < 0 {
                eprintln!("sh: kill: ({}) - {}", target, strerror(-r));
                status = 1;
            }
        }
        status
    }

    /// When the interactive shell exits, hang up its background jobs.
    fn hang_up_jobs(&mut self) {
        for &pid in &self.jobs {
            kill(-pid, SIGHUP);
        }
    }
}

/// Print words indented, wrapping before column 80 instead of mid-word.
fn print_wrapped<'a>(words: impl Iterator<Item = &'a str>) {
    let mut line = String::from(" ");
    for w in words {
        if line.len() + 1 + w.len() > 78 {
            println!("{}", line);
            line = String::from(" ");
        }
        line.push(' ');
        line.push_str(w);
    }
    if line.trim().len() > 0 {
        println!("{}", line);
    }
}

fn restore_fds(saved: Vec<(i32, isize)>) {
    for (fd, copy) in saved.into_iter().rev() {
        if copy >= 0 {
            dup2(copy as i32, fd);
            close(copy as i32);
        } else {
            close(fd);
        }
    }
}

/// $? for a wait status: exit code, or 128 + signal.
fn shell_status(status: i32) -> i32 {
    if wifsignaled(status) { 128 + wtermsig(status) } else { wexitstatus(status) }
}

fn cwd() -> String {
    let mut buf = [0u8; 1024];
    getcwd(&mut buf).map(|s| s.to_string()).unwrap_or_else(|_| "?".into())
}

fn read_file(path: &str) -> Result<String, isize> {
    let fd = open(path, O_RDONLY);
    if fd < 0 {
        return Err(fd);
    }
    let mut data = Vec::new();
    let mut buf = vec![0u8; 1024];
    loop {
        let n = read(fd as i32, &mut buf);
        if n < 0 {
            close(fd as i32);
            return Err(n);
        }
        if n == 0 {
            break;
        }
        data.extend_from_slice(&buf[..n as usize]);
    }
    close(fd as i32);
    Ok(String::from_utf8_lossy(&data).into_owned())
}

/// Read one line from stdin. None at end-of-file on an empty line.
fn read_line() -> Option<String> {
    let mut line = Vec::new();
    let mut byte = [0u8; 256];
    loop {
        let n = read(STDIN, &mut byte);
        if n <= 0 {
            return if line.is_empty() { None } else { Some(String::from_utf8_lossy(&line).into_owned()) };
        }
        line.extend_from_slice(&byte[..n as usize]);
        if line.last() == Some(&b'\n') {
            line.pop();
            return Some(String::from_utf8_lossy(&line).into_owned());
        }
    }
}

fn main(args: Args) -> i32 {
    let argv: Vec<String> = args.map(String::from).collect();

    match argv.get(1).map(String::as_str) {
        Some("-c") => {
            let Some(script) = argv.get(2) else {
                eprintln!("sh: -c: option requires an argument");
                return 2;
            };
            // $0 is "sh", or the next argument if given (POSIX)
            let params = argv[3..].to_vec();
            let params = if params.is_empty() { vec![argv[0].clone()] } else { params };
            let mut sh = Shell::new(params, false);
            sh.run_text(script);
            sh.exit_requested.unwrap_or(sh.status)
        }
        Some(file) => {
            let text = match read_file(file) {
                Ok(t) => t,
                Err(e) => {
                    eprintln!("sh: {}: {}", file, strerror(-e));
                    return 127;
                }
            };
            let mut sh = Shell::new(argv[1..].to_vec(), false);
            sh.run_text(&text);
            sh.exit_requested.unwrap_or(sh.status)
        }
        None => {
            let mut sh = Shell::new(vec![argv.first().cloned().unwrap_or("sh".into())], true);
            // Ctrl-C interrupts commands, not the interactive shell.
            set_signal(SIGINT, SIG_IGN);
            tcsetpgrp(STDIN, getpgrp());
            println!("PalladiumOS shell. Type 'help' for commands.");
            let mut editor = LineEditor::new();
            loop {
                sh.reap_jobs();
                let prompt = format!("PalladiumOS:{} $ ", cwd());
                let line = if isatty(STDIN) {
                    editor.read_line(&prompt)
                } else {
                    print!("{}", prompt);
                    read_line()
                };
                let Some(line) = line else {
                    println!();
                    break; // Ctrl-D
                };
                sh.run_line(&line);
                if let Some(code) = sh.exit_requested {
                    sh.hang_up_jobs();
                    return code;
                }
            }
            sh.hang_up_jobs();
            sh.status
        }
    }
}

// ---------------------------------------------------------------- line editor

/// Interactive line editing on the console (raw mode): cursor movement,
/// history, and Tab completion. Commands still run in cooked mode.
struct LineEditor {
    history: Vec<String>,
}

const HISTORY_MAX: usize = 100;

impl LineEditor {
    fn new() -> Self {
        LineEditor { history: Vec::new() }
    }

    /// Show `prompt` and read a line. None at Ctrl-D on an empty line.
    fn read_line(&mut self, prompt: &str) -> Option<String> {
        let Ok(saved) = enable_raw_mode(STDIN) else {
            print!("{}", prompt);
            return read_line();
        };
        let result = self.edit(prompt);
        tcsetattr(STDIN, &saved);
        if let Some(line) = &result {
            if !line.trim().is_empty() && self.history.last() != Some(line) {
                self.history.push(line.clone());
                if self.history.len() > HISTORY_MAX {
                    self.history.remove(0);
                }
            }
        }
        result
    }

    fn edit(&mut self, prompt: &str) -> Option<String> {
        let mut keys = KeyReader::new(STDIN);
        let mut buf: Vec<char> = Vec::new();
        let mut cur = 0usize;
        // Index into history while browsing it; history.len() = the new line.
        let mut hist = self.history.len();
        let mut draft: Vec<char> = Vec::new();
        print!("{}", prompt);

        loop {
            let (old_cur, old_len) = (cur, buf.len());
            let Some(key) = keys.next_key() else { return None };
            match key {
                Key::Enter => {
                    move_right(buf.len() - cur);
                    print!("\n");
                    return Some(buf.iter().collect());
                }
                Key::Ctrl('d') if buf.is_empty() => return None,
                Key::Ctrl('d') | Key::Delete => {
                    if cur < buf.len() {
                        buf.remove(cur);
                    }
                }
                Key::Ctrl('c') => {
                    move_right(buf.len() - cur);
                    print!("^C\n{}", prompt);
                    buf.clear();
                    cur = 0;
                    hist = self.history.len();
                    continue;
                }
                Key::Ctrl('l') => {
                    print!("\x1b[H\x1b[2J{}", prompt);
                    redraw(0, 0, &buf, cur);
                    continue;
                }
                Key::Backspace => {
                    if cur > 0 {
                        cur -= 1;
                        buf.remove(cur);
                    }
                }
                Key::Left => cur = cur.saturating_sub(1),
                Key::Right => cur = (cur + 1).min(buf.len()),
                Key::Home | Key::Ctrl('a') => cur = 0,
                Key::End | Key::Ctrl('e') => cur = buf.len(),
                Key::Ctrl('u') => {
                    buf.drain(..cur);
                    cur = 0;
                }
                Key::Ctrl('k') => buf.truncate(cur),
                Key::Up => {
                    if hist > 0 {
                        if hist == self.history.len() {
                            draft = buf.clone();
                        }
                        hist -= 1;
                        buf = self.history[hist].chars().collect();
                        cur = buf.len();
                    }
                }
                Key::Down => {
                    if hist < self.history.len() {
                        hist += 1;
                        buf = if hist == self.history.len() {
                            draft.clone()
                        } else {
                            self.history[hist].chars().collect()
                        };
                        cur = buf.len();
                    }
                }
                Key::Tab => {
                    if let Some(listing) = complete(&mut buf, &mut cur) {
                        // Ambiguous: list the choices, then redraw the line.
                        move_right(old_len - old_cur);
                        print!("\n{}\n{}", listing, prompt);
                        redraw(0, 0, &buf, cur);
                        continue;
                    }
                }
                Key::Char(c) if !c.is_control() => {
                    if buf.len() < 1000 {
                        buf.insert(cur, c);
                        cur += 1;
                    }
                }
                _ => continue,
            }
            redraw(old_cur, old_len, &buf, cur);
        }
    }
}

fn move_left(n: usize) {
    if n > 0 {
        print!("\x1b[{}D", n);
    }
}

fn move_right(n: usize) {
    if n > 0 {
        print!("\x1b[{}C", n);
    }
}

/// Repaint the input after an edit. The cursor was `old_cur` characters into
/// an `old_len`-character line; afterwards it's `cur` characters into `buf`.
/// Moves are relative, so this works when the line wraps.
fn redraw(old_cur: usize, old_len: usize, buf: &[char], cur: usize) {
    let mut out = String::new();
    if old_cur > 0 {
        out.push_str(&format!("\x1b[{}D", old_cur));
    }
    out.extend(buf.iter());
    let extra = old_len.saturating_sub(buf.len());
    out.extend(core::iter::repeat(' ').take(extra));
    let back = buf.len() + extra - cur;
    if back > 0 {
        out.push_str(&format!("\x1b[{}D", back));
    }
    print!("{}", out);
}

/// Tab completion of the word before the cursor: a command name (builtins
/// and programs in /bin) for the first word, otherwise a file name.
/// Completes as far as is unambiguous; if that adds nothing and there are
/// several choices, returns them for listing.
fn complete(buf: &mut Vec<char>, cur: &mut usize) -> Option<String> {
    let start = buf[..*cur].iter().rposition(|c| *c == ' ').map(|i| i + 1).unwrap_or(0);
    let word: String = buf[start..*cur].iter().collect();
    let first_word = buf[..start].iter().all(|c| *c == ' ');

    // (candidate name, is a directory)
    let mut candidates: Vec<(String, bool)> = Vec::new();
    let (dir, prefix) = match word.rfind('/') {
        Some(i) => (word[..=i].to_string(), word[i + 1..].to_string()),
        None => (String::new(), word.clone()),
    };
    if first_word && dir.is_empty() {
        for b in BUILTINS {
            if b.starts_with(prefix.as_str()) {
                candidates.push((b.to_string(), false));
            }
        }
        list_dir("/bin", &prefix, &mut candidates);
        candidates.retain(|(n, _)| !n.starts_with("test_"));
    } else {
        let d = if dir.is_empty() { ".".to_string() } else { dir.clone() };
        list_dir(&d, &prefix, &mut candidates);
    }
    candidates.sort();
    candidates.dedup();
    if candidates.is_empty() {
        return None;
    }

    // Longest common prefix of all candidates.
    let names: Vec<&str> = candidates.iter().map(|(n, _)| n.as_str()).collect();
    let mut common = names[0].to_string();
    for n in &names[1..] {
        while !n.starts_with(common.as_str()) {
            common.pop();
        }
    }
    let mut insert: String = common[prefix.len()..].to_string();
    if candidates.len() == 1 {
        insert.push(if candidates[0].1 { '/' } else { ' ' });
    }
    if insert.is_empty() {
        if candidates.len() > 1 {
            let listing: Vec<String> =
                candidates.iter().map(|(n, d)| if *d { format!("{}/", n) } else { n.clone() }).collect();
            return Some(listing.join("  "));
        }
        return None;
    }
    for c in insert.chars() {
        buf.insert(*cur, c);
        *cur += 1;
    }
    None
}

fn list_dir(dir: &str, prefix: &str, out: &mut Vec<(String, bool)>) {
    let fd = open(dir, O_RDONLY | O_DIRECTORY);
    if fd < 0 {
        return;
    }
    let _ = for_each_entry(fd as i32, |e| {
        if e.name != "." && e.name != ".." && e.name.starts_with(prefix) && (!e.name.starts_with('.') || prefix.starts_with('.')) {
            out.push((e.name.to_string(), e.d_type == DT_DIR));
        }
    });
    close(fd as i32);
}
