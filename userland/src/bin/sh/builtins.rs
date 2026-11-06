// Copyright (c) 2026 tre4surehunter9
//! Builtin commands: the ones that must run inside the shell (cd, exit,
//! variables, control flow) plus test/[ and read.

use alloc::string::{String, ToString};
use alloc::vec::Vec;
use userlib::*;

use crate::shell::{Shell, Var};
use crate::{print_wrapped, shell_status};

pub const BUILTINS: &[&str] = &[
    ".", ":", "[", "break", "cd", "continue", "eval", "exit", "export", "false", "help", "jobs", "kill", "pwd", "read",
    "return", "run", "script", "set", "shift", "source", "test", "true", "type", "unset", "wait",
];

pub fn is_builtin(name: &str) -> bool {
    BUILTINS.contains(&name)
}

impl Shell {
    pub fn builtin(&mut self, args: &[String]) -> i32 {
        let arg = |i: usize| args.get(i).map(String::as_str);
        // A numeric argument (for exit/return/break/...), or Err after printing.
        let number = |i: usize, default: i64| -> Result<i64, ()> {
            match args.get(i) {
                None => Ok(default),
                Some(s) => s.parse::<i64>().map_err(|_| {
                    eprintln!("sh: {}: {}: numeric argument required", args[0], s);
                }),
            }
        };
        match args[0].as_str() {
            "true" | ":" => 0,
            "false" => 1,
            "cd" => {
                let home = self.get("HOME");
                let target = arg(1).unwrap_or(&home);
                let r = chdir(target);
                if r < 0 {
                    eprintln!("sh: cd: {}: {}", target, strerror(-r));
                    return 1;
                }
                let pwd = crate::cwd();
                self.set("PWD", pwd);
                0
            }
            "pwd" => {
                println!("{}", crate::cwd());
                0
            }
            "exit" => {
                let code = match number(1, self.status as i64) {
                    Ok(n) => (n & 0xff) as i32,
                    Err(()) => 2,
                };
                self.exit_requested = Some(code);
                code
            }
            "return" => {
                if self.function_depth == 0 {
                    eprintln!("sh: return: can only be used in a function or sourced script");
                    return 1;
                }
                let code = match number(1, self.status as i64) {
                    Ok(n) => (n & 0xff) as i32,
                    Err(()) => 2,
                };
                self.returning = true;
                code
            }
            "break" | "continue" => {
                let Ok(n) = number(1, 1) else { return 1 };
                if n < 1 {
                    eprintln!("sh: {}: loop count must be at least 1", args[0]);
                    return 1;
                }
                if self.loop_depth == 0 {
                    eprintln!("sh: {}: only meaningful in a loop", args[0]);
                    return 0;
                }
                let n = (n as usize).min(self.loop_depth);
                if args[0] == "break" {
                    self.breaking = n;
                } else {
                    self.continuing = n;
                }
                0
            }
            "shift" => {
                let Ok(n) = number(1, 1) else { return 1 };
                let n = n.max(0) as usize;
                if n > self.params.len() - 1 {
                    eprintln!("sh: shift: can't shift that many");
                    return 1;
                }
                self.params.drain(1..1 + n);
                0
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
                    if a == "-f" {
                        continue;
                    }
                    if args.get(1).map(String::as_str) == Some("-f") {
                        self.functions.remove(a.as_str());
                    } else {
                        self.vars.remove(a.as_str());
                    }
                }
                0
            }
            "set" => {
                if args.get(1).map(String::as_str) == Some("--") {
                    // set -- a b c: replace the positional parameters
                    let name = self.params[0].clone();
                    self.params = core::iter::once(name).chain(args[2..].iter().cloned()).collect();
                    return 0;
                }
                for (k, v) in &self.vars {
                    println!("{}={}", k, v.value);
                }
                0
            }
            "read" => self.read_builtin(&args[1..]),
            "test" => test(&args[1..]),
            "[" => {
                if args.last().map(String::as_str) != Some("]") {
                    eprintln!("sh: [: missing ']'");
                    return 2;
                }
                test(&args[1..args.len() - 1])
            }
            "eval" => {
                let text = args[1..].join(" ");
                self.run_text(&text);
                self.status
            }
            "." | "source" => {
                let Some(file) = arg(1) else {
                    eprintln!("sh: {}: filename argument required", args[0]);
                    return 2;
                };
                match crate::read_file(file) {
                    Ok(text) => {
                        // `return` is allowed in a sourced file.
                        self.function_depth += 1;
                        self.run_text(&text);
                        self.function_depth -= 1;
                        self.returning = false;
                        self.status
                    }
                    Err(e) => {
                        eprintln!("sh: {}: {}", file, strerror(-e));
                        1
                    }
                }
            }
            "type" => {
                let mut status = 0;
                for name in &args[1..] {
                    if self.functions.contains_key(name.as_str()) {
                        println!("{} is a function", name);
                    } else if is_builtin(name) {
                        println!("{} is a shell builtin", name);
                    } else if let Some(path) = self.find_program(name) {
                        println!("{} is {}", name, path);
                    } else {
                        eprintln!("sh: type: {}: not found", name);
                        status = 1;
                    }
                }
                status
            }
            "jobs" => {
                for (i, pid) in self.jobs.iter().enumerate() {
                    println!("[{}] {} Running", i + 1, pid);
                }
                0
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
            "kill" => self.kill_builtin(args),
            // Kernel-shell compatibility: `run prog args` and `script file`.
            "run" | "script" => {
                if args.len() < 2 {
                    eprintln!("usage: {} <{}> [args...]", args[0], if args[0] == "run" { "program" } else { "file" });
                    return 2;
                }
                let mut words: Vec<String> = Vec::new();
                if args[0] == "script" {
                    words.push("sh".into());
                }
                words.extend(args[1..].iter().cloned());
                let cmd = crate::parse::Cmd::Simple(crate::parse::Simple {
                    words: words.into_iter().map(|w| alloc::vec![crate::lex::Part::Quoted(w)]).collect(),
                    ..Default::default()
                });
                self.run_cmd(&cmd);
                self.status
            }
            "help" => {
                println!("PalladiumOS sh. Builtins:");
                print_wrapped(BUILTINS.iter().copied());
                println!("Programs in /bin (type the name to run one, e.g. ls -l /bin):");
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
                println!("Syntax: | ; && || & > >> < 2> 2>&1 'quotes' \"$VAR\" $(cmd) $((1+2)) * ? [ ]");
                println!("Scripting: if/then/elif/else/fi, while/until/do/done, for/in, case/esac,");
                println!("  name() {{ ... }}, test or [ ], read, break, continue, return, shift");
                println!("Keys: Up/Down history, Tab completes, Ctrl-C interrupts, Ctrl-D exits");
                println!("Type 'exit' to return to the kernel shell.");
                0
            }
            _ => 127,
        }
    }

    /// read [-r] [NAME...]: read a line from standard input into variables
    /// (split on whitespace; the last name gets the rest). Status 1 at
    /// end of input.
    fn read_builtin(&mut self, args: &[String]) -> i32 {
        let names: Vec<&str> = args.iter().map(String::as_str).filter(|a| *a != "-r").collect();
        let mut line = Vec::new();
        let mut byte = [0u8; 1];
        let mut got_newline = false;
        // One byte at a time, so nothing after the line is consumed.
        loop {
            let n = read(STDIN, &mut byte);
            if n <= 0 {
                break;
            }
            if byte[0] == b'\n' {
                got_newline = true;
                break;
            }
            line.push(byte[0]);
        }
        let text = String::from_utf8_lossy(&line).into_owned();
        let names = if names.is_empty() { alloc::vec!["REPLY"] } else { names };
        let mut rest = text.trim_start();
        for (i, name) in names.iter().enumerate() {
            if i + 1 == names.len() {
                self.set(name, rest.trim_end().to_string());
            } else {
                let (word, after) = match rest.find(|c: char| c.is_ascii_whitespace()) {
                    Some(at) => (&rest[..at], rest[at..].trim_start()),
                    None => (rest, ""),
                };
                self.set(name, word.to_string());
                rest = after;
            }
        }
        if got_newline || !line.is_empty() { 0 } else { 1 }
    }

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
}

// ---------------------------------------------------------------- test / [

/// test EXPRESSION: 0 if true, 1 if false, 2 on error.
pub fn test(args: &[String]) -> i32 {
    let args: Vec<&str> = args.iter().map(String::as_str).collect();
    if args.is_empty() {
        return 1;
    }
    let mut t = Test { args: &args, i: 0 };
    match t.or() {
        Ok(v) if t.i == args.len() => (!v) as i32,
        Ok(_) => {
            eprintln!("sh: test: unexpected argument '{}'", args[t.i]);
            2
        }
        Err(e) => {
            eprintln!("sh: test: {}", e);
            2
        }
    }
}

struct Test<'a> {
    args: &'a [&'a str],
    i: usize,
}

const BINARY: &[&str] = &["=", "==", "!=", "-eq", "-ne", "-lt", "-le", "-gt", "-ge"];
const UNARY: &[&str] = &["-e", "-f", "-d", "-c", "-s", "-r", "-w", "-x", "-z", "-n"];

impl<'a> Test<'a> {
    fn peek(&self, n: usize) -> Option<&'a str> {
        self.args.get(self.i + n).copied()
    }

    fn or(&mut self) -> Result<bool, String> {
        let mut v = self.and()?;
        while self.peek(0) == Some("-o") {
            self.i += 1;
            let r = self.and()?;
            v = v || r;
        }
        Ok(v)
    }

    fn and(&mut self) -> Result<bool, String> {
        let mut v = self.not()?;
        while self.peek(0) == Some("-a") {
            self.i += 1;
            let r = self.not()?;
            v = v && r;
        }
        Ok(v)
    }

    fn not(&mut self) -> Result<bool, String> {
        // "!" is negation unless it's being compared ([ ! = x ]).
        if self.peek(0) == Some("!") && !self.peek(1).is_some_and(|op| BINARY.contains(&op)) {
            self.i += 1;
            return Ok(!self.not()?);
        }
        self.primary()
    }

    fn primary(&mut self) -> Result<bool, String> {
        let Some(a) = self.peek(0) else { return Err("argument expected".into()) };
        // binary: ARG OP ARG
        if let (Some(op), Some(b)) = (self.peek(1), self.peek(2)) {
            if BINARY.contains(&op) {
                self.i += 3;
                return binary(a, op, b);
            }
        }
        if a == "(" {
            self.i += 1;
            let v = self.or()?;
            if self.peek(0) != Some(")") {
                return Err("missing ')'".into());
            }
            self.i += 1;
            return Ok(v);
        }
        // unary: -f FILE, -z STRING, ...
        if UNARY.contains(&a) {
            if let Some(operand) = self.peek(1) {
                self.i += 2;
                return Ok(unary(a, operand));
            }
        }
        // a lone string: true if non-empty
        self.i += 1;
        Ok(!a.is_empty())
    }
}

fn unary(op: &str, s: &str) -> bool {
    match op {
        "-z" => s.is_empty(),
        "-n" => !s.is_empty(),
        _ => match stat(s) {
            Err(_) => false,
            Ok(st) => match op {
                "-f" => st.is_file(),
                "-d" => st.is_dir(),
                "-c" => st.st_mode & S_IFMT == S_IFCHR, // character device (/dev/null...)
                "-s" => st.st_size > 0,
                _ => true, // -e, and -r/-w/-x (there are no permissions yet)
            },
        },
    }
}

fn binary(a: &str, op: &str, b: &str) -> Result<bool, String> {
    match op {
        "=" | "==" => return Ok(a == b),
        "!=" => return Ok(a != b),
        _ => {}
    }
    let parse = |s: &str| s.trim().parse::<i64>().map_err(|_| alloc::format!("{}: integer expression expected", s));
    let (x, y) = (parse(a)?, parse(b)?);
    Ok(match op {
        "-eq" => x == y,
        "-ne" => x != y,
        "-lt" => x < y,
        "-le" => x <= y,
        "-gt" => x > y,
        _ => x >= y,
    })
}
