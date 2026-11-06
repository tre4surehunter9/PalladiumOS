// Copyright (c) 2026 tre4surehunter9
//! sh: the PalladiumOS shell, a POSIX-style shell.
//!
//!   sh                 interactive (reads commands from standard input)
//!   sh FILE [ARGS...]  run a script ($0 = FILE, $1... = ARGS)
//!   sh -c 'COMMANDS'   run COMMANDS
//!
//! Supported:
//!   'single quotes', "double quotes", \ escapes, # comments, line continuation
//!   $VAR ${VAR} ${VAR:-default} ${VAR:=x} ${VAR:+x} ${VAR:?msg} ${#VAR}
//!   $? $$ $! $# $@ "$@" $* $0-$9, ~, $(command) `command`, $((arithmetic))
//!   wildcards: * ? [abc] [a-z] [!x]; unquoted expansions are split into words
//!   NAME=value, NAME=value cmd, cmd1 | cmd2, ; && || &, ! pipeline
//!   > >> < 2> 2>&1 (also on compound commands: `while ...; done > file`)
//!   if/then/elif/else/fi, while/until/do/done, for NAME [in WORDS]; do/done,
//!   case WORD in PATTERN|PATTERN) ... ;; esac, { list; }, ( subshell ),
//!   functions: name() { ...; } with $1.. and return
//!   builtins: see BUILTINS (cd exit export read test [ break continue ...)
//!   interactive line editing: arrows, Home/End, history (Up/Down), Tab
//!   completion, Ctrl-A/E/U/K/L/C; incomplete commands continue with "> "
//!   options: -e (exit on failure), -x (print commands), also via `set`
//! Not yet: here-documents (<<), aliases, `local`.
#![no_std]
#![no_main]

extern crate alloc;

mod builtins;
mod editor;
mod expand;
mod lex;
mod parse;
mod shell;

use alloc::format;
use alloc::string::{String, ToString};
use alloc::vec;
use alloc::vec::Vec;
use userlib::*;

use editor::LineEditor;
use shell::Shell;

userlib::entry!(main);

pub fn print_wrapped<'a>(words: impl Iterator<Item = &'a str>) {
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
pub fn restore_fds(saved: Vec<(i32, isize)>) {
    for (fd, copy) in saved.into_iter().rev() {
        if copy >= 0 {
            dup2(copy as i32, fd);
            close(copy as i32);
        } else {
            close(fd);
        }
    }
}
/// $? for a wait status: the exit code, or 128 + the signal that killed
/// (or stopped) it.
pub fn shell_status(status: i32) -> i32 {
    if wifstopped(status) {
        128 + wstopsig(status)
    } else if wifsignaled(status) {
        128 + wtermsig(status)
    } else {
        wexitstatus(status)
    }
}
pub fn cwd() -> String {
    let mut buf = [0u8; 1024];
    getcwd(&mut buf).map(|s| s.to_string()).unwrap_or_else(|_| "?".into())
}
pub fn read_file(path: &str) -> Result<String, isize> {
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
pub fn read_line() -> Option<String> {
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
    let mut argv: Vec<String> = args.map(String::from).collect();

    // Options before the script: -e, -x (also combined: -ex), as from a
    // "#!/bin/sh -e" line.
    let (mut errexit, mut xtrace) = (false, false);
    while argv.len() > 1 && argv[1].len() > 1 && argv[1].starts_with('-') && argv[1] != "-c" && argv[1] != "--" {
        for c in argv[1][1..].chars() {
            match c {
                'e' => errexit = true,
                'x' => xtrace = true,
                'u' | 'v' | 'f' | 'i' | 's' => {} // accepted, not implemented
                _ => {
                    eprintln!("sh: -{}: invalid option", c);
                    return 2;
                }
            }
        }
        argv.remove(1);
    }
    if argv.get(1).map(String::as_str) == Some("--") {
        argv.remove(1);
    }
    let setup = |sh: &mut Shell| {
        sh.errexit = errexit;
        sh.xtrace = xtrace;
    };

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
            setup(&mut sh);
            sh.run_text(script);
            sh.run_exit_trap();
            sh.exit_status()
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
            setup(&mut sh);
            sh.run_text(&text);
            sh.run_exit_trap();
            sh.exit_status()
        }
        None => interactive(argv.first().cloned().unwrap_or_else(|| "sh".to_string())),
    }
}

/// "user@palladium:/path $ " ("#" for root), with the home directory as ~.
fn prompt_text() -> String {
    let user = userlib::users::user_name(geteuid());
    let mut dir = cwd();
    if let Some(home) = userlib::getenv("HOME").filter(|h| *h != "/") {
        if dir == home {
            dir = "~".to_string();
        } else if let Some(rest) = dir.strip_prefix(&format!("{}/", home)) {
            dir = format!("~/{}", rest);
        }
    }
    format!("{}@palladium:{} {} ", user, dir, if geteuid() == 0 { "#" } else { "$" })
}

fn interactive(name: String) -> i32 {
    let mut sh = Shell::new(vec![name], true);
    // Ctrl-C interrupts commands and Ctrl-Z stops them, not the interactive
    // shell itself.
    for sig in shell::SHELL_IGNORED {
        set_signal(sig, SIG_IGN);
    }
    tcsetpgrp(STDIN, getpgrp());
    println!("PalladiumOS shell. Type 'help' for commands.");
    let mut editor = LineEditor::new();
    let tty = isatty(STDIN);
    let mut warned_stopped = false;

    loop {
        sh.run_traps();
        sh.reap_jobs();
        sh.interrupted = false;
        // Read lines until they form a complete command (an `if` or a
        // quote may continue over several lines, prompted with "> ").
        let mut text = String::new();
        let mut prompt = prompt_text();
        let list = loop {
            let line = if tty {
                editor.read_line(&prompt)
            } else {
                print!("{}", prompt);
                read_line()
            };
            let Some(line) = line else {
                if !text.is_empty() {
                    eprintln!("sh: syntax error: unexpected end of file");
                }
                println!();
                sh.run_exit_trap();
                sh.hang_up_jobs();
                return sh.exit_status(); // Ctrl-D
            };
            text.push_str(&line);
            text.push('\n');
            match shell::parse_or_incomplete(&text) {
                Ok(Some(list)) => break Some(list),
                Ok(None) => prompt = "> ".to_string(),
                Err(e) => {
                    eprintln!("sh: {}", e);
                    sh.status = 2;
                    break None;
                }
            }
        };
        if let Some(list) = list {
            sh.run_list(&list);
            // break/continue outside a loop, or a stray return: just reset.
            sh.breaking = 0;
            sh.continuing = 0;
            sh.returning = false;
        }
        if sh.exit_requested.is_some() {
            if sh.jobs.iter().any(|j| j.stopped) && !warned_stopped {
                // Like bash: a second `exit` really exits.
                println!("There are stopped jobs.");
                warned_stopped = true;
                sh.exit_requested = None;
                continue;
            }
            sh.run_exit_trap();
            sh.hang_up_jobs();
            return sh.exit_status();
        }
        warned_stopped = false;
    }
}
