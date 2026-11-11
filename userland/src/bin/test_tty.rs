// Copyright (c) 2026 tre4surehunter9
//! Test program for the terminal API. Exits 0 on success, or the line
//! number of the first failed check.
//!   test_tty modes - termios, isatty, window size
//!   test_tty keys  - raw mode: expects the keys a, Backspace, Up, Delete, Enter
#![no_std]
#![no_main]

use userlib::*;

userlib::entry!(main);

macro_rules! check {
    ($cond:expr) => {
        if !$cond {
            eprintln!("test_tty: check failed at line {}: {}", line!(), stringify!($cond));
            return line!() as i32;
        }
    };
}

fn main(args: Args) -> i32 {
    match args.get(1) {
        Some("modes") => modes(),
        Some("keys") => keys(),
        _ => 98,
    }
}

fn modes() -> i32 {
    check!(isatty(STDIN) && isatty(STDOUT));
    let fd = open("/tty_test.txt", O_CREAT | O_WRONLY) as i32;
    check!(!isatty(fd));
    check!(tcgetattr(fd).err() == Some(-ENOTTY));
    close(fd);
    unlink("/tty_test.txt");
    check!(tcgetattr(99).err() == Some(-EBADF));

    let t = tcgetattr(STDIN).unwrap_or_default();
    check!(t.c_lflag & ICANON != 0 && t.c_lflag & ECHO != 0 && t.c_lflag & ISIG != 0);
    check!(t.c_cc[0] == 3); // VINTR = ^C

    let saved = enable_raw_mode(STDIN);
    check!(saved.is_ok());
    let raw = tcgetattr(STDIN).unwrap_or_default();
    check!(raw.c_lflag & (ICANON | ECHO | ISIG) == 0);
    check!(tcsetattr(STDIN, &saved.unwrap()) == 0);
    check!(tcgetattr(STDIN).map(|t| t.c_lflag & ICANON != 0) == Ok(true));

    let (rows, cols) = window_size(STDOUT);
    check!(rows >= 25 && cols >= 80); // (the framebuffer console: 80x25 or bigger)
    check!(tcgetpgrp(STDIN) > 0);
    0
}

fn keys() -> i32 {
    let Ok(saved) = enable_raw_mode(STDIN) else { return 1 };
    let mut keys = KeyReader::new(STDIN);
    let expected = [Key::Char('a'), Key::Backspace, Key::Up, Key::Delete, Key::Enter];
    for (i, want) in expected.iter().enumerate() {
        let got = keys.next_key();
        if got != Some(*want) {
            tcsetattr(STDIN, &saved);
            eprintln!("test_tty: key {}: got {:?}, want {:?}", i, got, want);
            return 10 + i as i32;
        }
    }
    tcsetattr(STDIN, &saved);
    0
}
