// Copyright (c) 2026 tre4surehunter9
//! The shell's state and the executor for the syntax tree.

use alloc::collections::BTreeMap;
use alloc::format;
use alloc::string::{String, ToString};
use alloc::vec::Vec;
use userlib::*;

use crate::lex::{lex, LexError, Redir, RedirKind};
use crate::parse::{parse, AndOr, Cmd, Compound, Connector, List, ParseError, Pipeline, Simple};
use crate::shell_status;

pub struct Var {
    pub value: String,
    pub exported: bool,
}

pub struct Shell {
    pub vars: BTreeMap<String, Var>,
    /// $0, $1, ...
    pub params: Vec<String>,
    pub functions: BTreeMap<String, Compound>,
    pub status: i32,
    pub interactive: bool,
    /// Background jobs (pids) not yet reported.
    pub jobs: Vec<i32>,
    pub last_background: i32,
    /// Set by `exit`.
    pub exit_requested: Option<i32>,
    /// Pending `break N` / `continue N`.
    pub breaking: usize,
    pub continuing: usize,
    pub loop_depth: usize,
    /// Set by `return` until the function call ends.
    pub returning: bool,
    pub function_depth: usize,
    /// A foreground command was killed by Ctrl-C: abandon the rest of the
    /// line (like bash), so `while true; do sleep 1; done` can be stopped.
    pub interrupted: bool,
    /// An expansion error (bad arithmetic, ${x?}) in the current command.
    pub expansion_failed: bool,
}

impl Shell {
    pub fn new(params: Vec<String>, interactive: bool) -> Self {
        let mut vars = BTreeMap::new();
        for kv in environ() {
            if let Some((k, v)) = kv.split_once('=') {
                vars.insert(k.to_string(), Var { value: v.to_string(), exported: true });
            }
        }
        for (k, v) in [("PATH", "/bin"), ("HOME", "/")] {
            vars.entry(k.to_string()).or_insert(Var { value: v.to_string(), exported: true });
        }
        Shell {
            vars,
            params,
            functions: BTreeMap::new(),
            status: 0,
            interactive,
            jobs: Vec::new(),
            last_background: 0,
            exit_requested: None,
            breaking: 0,
            continuing: 0,
            loop_depth: 0,
            returning: false,
            function_depth: 0,
            interrupted: false,
            expansion_failed: false,
        }
    }

    /// The status to exit with (after `exit` or at the end of a script).
    pub fn exit_status(&self) -> i32 {
        self.exit_requested.unwrap_or(self.status)
    }

    /// Should the current list stop (exit, break, continue, return, ^C)?
    pub fn stopping(&self) -> bool {
        self.exit_requested.is_some() || self.breaking > 0 || self.continuing > 0 || self.returning || self.interrupted
    }

    pub fn is_set(&self, name: &str) -> bool {
        match name {
            "?" | "$" | "#" | "@" | "*" | "0" | "PWD" => true,
            "!" => self.last_background != 0,
            n if n.chars().all(|c| c.is_ascii_digit()) => n.parse::<usize>().is_ok_and(|i| i < self.params.len()),
            n => self.vars.contains_key(n),
        }
    }

    pub fn get(&self, name: &str) -> String {
        match name {
            "?" => self.status.to_string(),
            "$" => getpid().to_string(),
            "!" => if self.last_background != 0 { self.last_background.to_string() } else { String::new() },
            "#" => self.params.len().saturating_sub(1).to_string(),
            "@" | "*" => self.params[1..].join(" "),
            "-" => if self.interactive { "i".into() } else { String::new() },
            n if !n.is_empty() && n.chars().all(|c| c.is_ascii_digit()) => {
                n.parse::<usize>().ok().and_then(|i| self.params.get(i).cloned()).unwrap_or_default()
            }
            "PWD" => crate::cwd(),
            n => self.vars.get(n).map(|v| v.value.clone()).unwrap_or_default(),
        }
    }

    pub fn set(&mut self, name: &str, value: String) {
        let exported = self.vars.get(name).is_some_and(|v| v.exported);
        self.vars.insert(name.to_string(), Var { value, exported });
    }

    /// The environment for a program: exported variables plus per-command
    /// assignments.
    pub fn child_env(&self, extra: &[(String, String)]) -> Vec<String> {
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
    pub fn find_program(&self, name: &str) -> Option<String> {
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

    /// In a forked child that runs shell code (subshell, pipeline part,
    /// command substitution, background job): no job control, and Ctrl-C
    /// works normally for whatever it runs.
    pub fn become_subshell(&mut self) {
        if self.interactive {
            set_signal(SIGINT, SIG_DFL);
        }
        self.interactive = false;
        self.jobs.clear();
    }

    // ------------------------------------------------------------ running

    /// Parse and run text (a script, `sh -c`, `.`, `eval`, $(...)).
    pub fn run_text(&mut self, text: &str) {
        match parse_text(text) {
            Ok(list) => self.run_list(&list),
            Err(e) => {
                eprintln!("sh: {}", e);
                self.status = 2;
            }
        }
    }

    pub fn run_list(&mut self, list: &List) {
        for (ao, background) in &list.0 {
            if self.stopping() {
                return;
            }
            if *background {
                self.run_background(ao);
            } else {
                self.run_and_or(ao);
            }
        }
    }

    fn run_and_or(&mut self, ao: &AndOr) {
        self.run_pipeline(&ao.first);
        for (conn, pipeline) in &ao.rest {
            if self.stopping() {
                return;
            }
            let run = match conn {
                Connector::And => self.status == 0,
                Connector::Or => self.status != 0,
            };
            if run {
                self.run_pipeline(pipeline);
            }
        }
    }

    /// `cmd &`: run it in a forked copy of the shell, without waiting.
    fn run_background(&mut self, ao: &AndOr) {
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
            }
            self.become_subshell();
            self.run_and_or(ao);
            exit(self.exit_status());
        }
        if self.interactive {
            setpgid(pid as i32, pid as i32); // also in the child: whichever runs first
            println!("[{}] {}", self.jobs.len() + 1, pid);
        }
        self.jobs.push(pid as i32);
        self.last_background = pid as i32;
        self.status = 0;
    }

    /// Report background jobs that have finished (before each prompt).
    pub fn reap_jobs(&mut self) {
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

    fn run_pipeline(&mut self, p: &Pipeline) {
        if p.cmds.len() == 1 {
            self.run_cmd(&p.cmds[0]);
        } else {
            self.run_multi(&p.cmds);
        }
        if p.negate {
            self.status = (self.status == 0) as i32;
        }
    }

    /// Run `cmd1 | cmd2 | ...`: each command in its own process, stdout of
    /// one connected to stdin of the next. The whole pipeline is one job
    /// (one process group), and its status is the last command's.
    fn run_multi(&mut self, cmds: &[Cmd]) {
        let mut pids: Vec<i32> = Vec::new();
        let mut pgid = 0i32;
        let mut prev_read: Option<i32> = None;
        let mut failed = false;

        for (i, cmd) in cmds.iter().enumerate() {
            let last = i + 1 == cmds.len();
            // Close-on-exec: programs only get the ends dup'ed onto 0 and 1.
            let (next_read, write_end) = if last {
                (None, None)
            } else {
                match pipe2(O_CLOEXEC) {
                    Ok((r, w)) => (Some(r), Some(w)),
                    Err(e) => {
                        eprintln!("sh: pipe: {}", strerror(-e));
                        failed = true;
                        break;
                    }
                }
            };

            let pid = fork();
            if pid < 0 {
                eprintln!("sh: fork: {}", strerror(-pid));
                for fd in [next_read, write_end].into_iter().flatten() {
                    close(fd);
                }
                failed = true;
                break;
            }
            if pid == 0 {
                if self.interactive {
                    setpgid(0, pgid); // pgid 0 for the first: it becomes the leader
                    if i == 0 {
                        tcsetpgrp(STDIN, getpid());
                    }
                }
                self.become_subshell();
                if let Some(r) = prev_read {
                    dup2(r, STDIN);
                    close(r);
                }
                if let Some(w) = write_end {
                    dup2(w, STDOUT);
                    close(w);
                }
                if let Some(r) = next_read {
                    close(r);
                }
                self.exec_in_child(cmd);
            }

            if pgid == 0 {
                pgid = pid as i32;
            }
            if self.interactive {
                setpgid(pid as i32, pgid);
            }
            // The parent keeps no pipe ends, or readers would never see EOF.
            if let Some(r) = prev_read {
                close(r);
            }
            if let Some(w) = write_end {
                close(w);
            }
            prev_read = next_read;
            pids.push(pid as i32);
        }
        if let Some(r) = prev_read {
            close(r);
        }

        if self.interactive && pgid > 0 {
            tcsetpgrp(STDIN, pgid);
        }
        let mut last_status = 0;
        let mut any_interrupted = false;
        for (i, &pid) in pids.iter().enumerate() {
            let mut status = 0;
            if waitpid(pid, &mut status, 0) == pid as isize {
                any_interrupted |= wifsignaled(status) && wtermsig(status) == SIGINT;
                if i + 1 == pids.len() {
                    last_status = status;
                }
            }
        }
        if self.interactive {
            tcsetpgrp(STDIN, getpgrp());
            if any_interrupted {
                self.interrupted = true;
            }
        }
        self.status = if failed { 1 } else { shell_status(last_status) };
    }

    /// In a forked child: run one command and exit. A plain program is
    /// exec'ed directly (no extra fork).
    pub fn exec_in_child(&mut self, cmd: &Cmd) -> ! {
        if let Cmd::Simple(simple) = cmd {
            let words = self.expand_words(&simple.words);
            let is_external = words
                .first()
                .is_some_and(|w| !crate::builtins::is_builtin(w) && !self.functions.contains_key(w.as_str()));
            if is_external && !self.expansion_failed {
                let assigns: Vec<(String, String)> =
                    simple.assigns.iter().map(|(k, v)| (k.clone(), self.expand_str(v))).collect();
                if self.apply_redirs(&simple.redirs).is_err() {
                    exit(1);
                }
                self.exec_program(&words, &assigns);
            }
        }
        self.run_cmd(cmd);
        exit(self.exit_status());
    }

    /// execve a program (in a child). Never returns.
    fn exec_program(&mut self, words: &[String], assigns: &[(String, String)]) -> ! {
        let Some(path) = self.find_program(&words[0]) else {
            eprintln!("sh: {}: command not found", words[0]);
            exit(127);
        };
        let env = self.child_env(assigns);
        let argv: Vec<&str> = words.iter().map(String::as_str).collect();
        let envp: Vec<&str> = env.iter().map(String::as_str).collect();
        let err = execve(&path, &argv, &envp);
        eprintln!("sh: {}: {}", words[0], strerror(-err));
        exit(if err == -ENOENT { 127 } else { 126 });
    }

    fn expand_words(&mut self, words: &[crate::lex::Word]) -> Vec<String> {
        let mut out = Vec::new();
        for w in words {
            out.extend(self.expand_fields(w));
        }
        out
    }

    pub fn run_cmd(&mut self, cmd: &Cmd) {
        self.expansion_failed = false;
        match cmd {
            Cmd::Simple(simple) => self.run_simple(simple),
            Cmd::FuncDef(name, body) => {
                self.functions.insert(name.clone(), body.clone());
                self.status = 0;
            }
            Cmd::Compound(compound, redirs) => {
                let saved = match self.apply_redirs(redirs) {
                    Ok(saved) => saved,
                    Err(()) => {
                        self.status = 1;
                        return;
                    }
                };
                self.run_compound(compound);
                crate::restore_fds(saved);
            }
        }
    }

    fn run_compound(&mut self, c: &Compound) {
        match c {
            Compound::Group(list) => self.run_list(list),
            Compound::Subshell(list) => self.run_subshell(list),
            Compound::If { branches, otherwise } => {
                for (cond, body) in branches {
                    self.run_list(cond);
                    if self.stopping() {
                        return;
                    }
                    if self.status == 0 {
                        self.run_list(body);
                        return;
                    }
                }
                match otherwise {
                    Some(body) => self.run_list(body),
                    None => self.status = 0,
                }
            }
            Compound::Loop { cond, body, until } => {
                let mut last = 0;
                self.loop_depth += 1;
                loop {
                    self.run_list(cond);
                    if self.loop_control() {
                        break;
                    }
                    if (self.status == 0) == *until {
                        break;
                    }
                    self.run_list(body);
                    last = self.status;
                    if self.loop_control() {
                        break;
                    }
                }
                self.loop_depth -= 1;
                if self.exit_requested.is_none() && !self.returning {
                    self.status = last; // the last body command's status (0 if none ran)
                }
            }
            Compound::For { var, words, body } => {
                let values = match words {
                    Some(words) => self.expand_words(words),
                    None => self.params[1..].to_vec(), // `for x; do` = for x in "$@"
                };
                let mut last = 0;
                self.loop_depth += 1;
                for v in values {
                    self.set(var, v);
                    self.run_list(body);
                    last = self.status;
                    if self.loop_control() {
                        break;
                    }
                }
                self.loop_depth -= 1;
                if self.exit_requested.is_none() && !self.returning {
                    self.status = last;
                }
            }
            Compound::Case { word, arms } => {
                let value: Vec<char> = self.expand_str(word).chars().collect();
                self.status = 0;
                for (patterns, body) in arms {
                    for pat in patterns {
                        let pattern = self.expand_pattern(pat);
                        if crate::expand::glob_match(&pattern, &value) {
                            self.run_list(body);
                            return;
                        }
                    }
                }
            }
        }
    }

    /// After a loop body: handle break/continue. Returns true if the loop
    /// should end.
    fn loop_control(&mut self) -> bool {
        if self.breaking > 0 {
            self.breaking -= 1;
            return true;
        }
        if self.continuing > 0 {
            self.continuing -= 1;
            // `continue 2` continues an outer loop: end this one.
            return self.continuing > 0;
        }
        self.exit_requested.is_some() || self.returning || self.interrupted
    }

    /// A case pattern: like a word, but quoted characters stay literal.
    fn expand_pattern(&mut self, word: &crate::lex::Word) -> Vec<(char, bool)> {
        use crate::lex::Part;
        let mut out = Vec::new();
        for part in word {
            match part {
                Part::Lit(s) => out.extend(s.chars().map(|c| (c, false))),
                Part::Quoted(s) => out.extend(s.chars().map(|c| (c, true))),
                other => {
                    let quoted = !matches!(
                        other,
                        Part::Param { quoted: false, .. } | Part::CmdSub { quoted: false, .. } | Part::Arith { quoted: false, .. }
                    );
                    let text = self.expand_str(&alloc::vec![other.clone()]);
                    out.extend(text.chars().map(|c| (c, quoted)));
                }
            }
        }
        out
    }

    /// ( list ): run it in a forked copy of the shell.
    fn run_subshell(&mut self, list: &List) {
        let pid = fork();
        if pid < 0 {
            eprintln!("sh: fork: {}", strerror(-pid));
            self.status = 1;
            return;
        }
        if pid == 0 {
            if self.interactive {
                setpgid(0, 0);
                tcsetpgrp(STDIN, getpid());
            }
            self.become_subshell();
            self.run_list(list);
            exit(self.exit_status());
        }
        self.wait_foreground(pid as i32, None);
    }

    /// Wait for a foreground child, giving it the terminal meanwhile.
    fn wait_foreground(&mut self, pid: i32, name: Option<&str>) {
        if self.interactive {
            setpgid(pid, pid);
            tcsetpgrp(STDIN, pid);
        }
        let mut status = 0;
        let r = waitpid(pid, &mut status, 0);
        if self.interactive {
            tcsetpgrp(STDIN, getpgrp()); // the terminal is ours again
        }
        if r < 0 {
            self.status = 1;
            return;
        }
        self.status = shell_status(status);
        if self.interactive && wifsignaled(status) {
            if wtermsig(status) == SIGINT {
                self.interrupted = true; // ^C was already printed
            } else if let Some(name) = name {
                let sig = signal_name(wtermsig(status)).unwrap_or("?");
                println!("[{}] killed by SIG{}", name, sig);
            }
        }
    }

    fn run_simple(&mut self, cmd: &Simple) {
        let words = self.expand_words(&cmd.words);
        let assigns: Vec<(String, String)> = cmd.assigns.iter().map(|(k, v)| (k.clone(), self.expand_str(v))).collect();
        if self.expansion_failed {
            self.status = 1;
            return;
        }

        if words.is_empty() {
            // Only assignments (and maybe redirections): set shell variables.
            for (k, v) in assigns {
                self.set(&k, v);
            }
            if !cmd.redirs.is_empty() {
                // e.g. `> file` creates/truncates the file
                match self.apply_redirs(&cmd.redirs) {
                    Ok(saved) => crate::restore_fds(saved),
                    Err(()) => {
                        self.status = 1;
                        return;
                    }
                }
            }
            // The status of a command substitution in an assignment counts.
            if !cmd.assigns.iter().any(|(_, v)| v.iter().any(|p| matches!(p, crate::lex::Part::CmdSub { .. }))) {
                self.status = 0;
            }
            return;
        }

        if let Some(body) = self.functions.get(&words[0]).cloned() {
            let saved = match self.apply_redirs(&cmd.redirs) {
                Ok(saved) => saved,
                Err(()) => {
                    self.status = 1;
                    return;
                }
            };
            self.call_function(&body, &words);
            crate::restore_fds(saved);
            return;
        }

        if crate::builtins::is_builtin(&words[0]) {
            // Builtins run in the shell, so redirect our own fds and put them back.
            let saved = match self.apply_redirs(&cmd.redirs) {
                Ok(saved) => saved,
                Err(()) => {
                    self.status = 1;
                    return;
                }
            };
            self.status = self.builtin(&words);
            crate::restore_fds(saved);
            return;
        }

        if self.find_program(&words[0]).is_none() {
            eprintln!("sh: {}: command not found", words[0]);
            self.status = 127;
            return;
        }

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
            self.exec_program(&words, &assigns);
        }
        self.wait_foreground(pid as i32, Some(&words[0]));
    }

    fn call_function(&mut self, body: &Compound, words: &[String]) {
        if self.function_depth >= 100 {
            eprintln!("sh: {}: too many nested function calls", words[0]);
            self.status = 1;
            return;
        }
        let mut params = Vec::with_capacity(words.len());
        params.push(self.params[0].clone()); // $0 stays the shell's name
        params.extend(words[1..].iter().cloned());
        let saved_params = core::mem::replace(&mut self.params, params);
        let saved_loops = core::mem::replace(&mut self.loop_depth, 0);
        self.function_depth += 1;
        self.run_compound(body);
        self.function_depth -= 1;
        self.loop_depth = saved_loops;
        self.params = saved_params;
        self.returning = false;
    }

    /// Apply redirections in this process. Returns the saved original fds
    /// so they can be restored (for builtins), or Err after printing an error.
    pub fn apply_redirs(&mut self, redirs: &[Redir]) -> Result<Vec<(i32, isize)>, ()> {
        let mut saved = Vec::new();
        for r in redirs {
            let source = match r.kind {
                RedirKind::ToFd(target) => {
                    if fstat(target).is_err() {
                        eprintln!("sh: {}: bad file descriptor", target);
                        crate::restore_fds(saved);
                        return Err(());
                    }
                    target as isize
                }
                kind => {
                    let file = self.expand_str(&r.target);
                    let flags = match kind {
                        RedirKind::Out => O_WRONLY | O_CREAT | O_TRUNC,
                        RedirKind::Append => O_WRONLY | O_CREAT | O_APPEND,
                        _ => O_RDONLY,
                    };
                    let fd = open(&file, flags);
                    if fd < 0 {
                        eprintln!("sh: {}: {}", file, strerror(-fd));
                        crate::restore_fds(saved);
                        return Err(());
                    }
                    fd
                }
            };
            saved.push((r.fd, fcntl(r.fd, F_DUPFD_CLOEXEC, 10)));
            dup2(source as i32, r.fd);
            if !matches!(r.kind, RedirKind::ToFd(_)) {
                close(source as i32);
            }
        }
        Ok(saved)
    }

    /// When the interactive shell exits, hang up its background jobs.
    pub fn hang_up_jobs(&mut self) {
        for &pid in &self.jobs {
            kill(-pid, SIGHUP);
        }
    }
}

/// Parse shell text. Errors are human-readable messages.
pub fn parse_text(text: &str) -> Result<List, String> {
    match parse_or_incomplete(text) {
        Ok(Some(list)) => Ok(list),
        Ok(None) => Err("syntax error: unexpected end of file".to_string()),
        Err(e) => Err(e),
    }
}

/// Parse shell text; Ok(None) means it's incomplete (an unclosed quote or
/// `if` ...), so an interactive shell should read another line.
pub fn parse_or_incomplete(text: &str) -> Result<Option<List>, String> {
    let toks = match lex(text) {
        Ok(t) => t,
        Err(LexError::Incomplete) => return Ok(None),
        Err(LexError::Syntax(e)) => return Err(e),
    };
    match parse(toks) {
        Ok(list) => Ok(Some(list)),
        Err(ParseError::Incomplete) => Ok(None),
        Err(ParseError::Syntax(e)) => Err(e),
    }
}
