// Copyright (c) 2026 tre4surehunter9
//! date: print the date and time (UTC).  date [+%s] prints seconds since 1970.
#![no_std]
#![no_main]

use userlib::{civil_time, println, unix_time};

userlib::entry!(main);

const DAYS: [&str; 7] = ["Thu", "Fri", "Sat", "Sun", "Mon", "Tue", "Wed"]; // 1970-01-01 was a Thursday
const MONTHS: [&str; 12] = ["Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec"];

fn main(args: userlib::Args) -> i32 {
    let t = unix_time();
    if args.get(1) == Some("+%s") {
        println!("{}", t);
        return 0;
    }
    let (y, mo, d, h, mi, s) = civil_time(t);
    let weekday = DAYS[((t / 86400) % 7) as usize];
    println!("{} {} {:2} {:02}:{:02}:{:02} UTC {}", weekday, MONTHS[(mo - 1) as usize], d, h, mi, s, y);
    0
}
