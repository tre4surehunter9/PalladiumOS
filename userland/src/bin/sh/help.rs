// Copyright (c) 2026 tre4surehunter9
//! The `help` builtin: an index of topics, and a page per topic.
//!
//!   help            the topics
//!   help TOPIC      e.g. help files, help builtins, help syntax
//!   help COMMAND    one line about a command (or builtin)
//!
//! Programs are grouped by what they're for. Only programs that exist are
//! listed; a program in /bin that no category knows shows up under
//! "other", so nothing new goes missing from help.

use alloc::string::{String, ToString};
use alloc::vec::Vec;
use userlib::*;

use crate::builtins::BUILTINS;
use crate::print_wrapped;

/// (topic, what it's about, [(program, description)])
const CATEGORIES: &[(&str, &str, &[(&str, &str)])] = &[
    (
        "files",
        "files and directories",
        &[
            ("ls", "list a directory (-l long, -a all)"),
            ("pwd", "print the current directory"),
            ("cat", "print files"),
            ("cp", "copy files (-r directories)"),
            ("mv", "move or rename"),
            ("rm", "remove files (-r directories)"),
            ("mkdir", "make a directory (-p parents)"),
            ("rmdir", "remove an empty directory"),
            ("ln", "make a link (-s symbolic)"),
            ("readlink", "show where a symlink points"),
            ("touch", "create a file or update its time"),
            ("stat", "show a file's details"),
            ("chmod", "change permissions (755, u+x)"),
            ("chown", "change owner (root only)"),
            ("chgrp", "change group"),
            ("df", "free space on each filesystem"),
            ("du", "space used by files"),
            ("edit", "the PalladiumOS text editor"),
            ("sync", "write cached data to disk"),
        ],
    ),
    (
        "text",
        "working with text",
        &[
            ("echo", "print arguments"),
            ("grep", "find lines matching a pattern"),
            ("wc", "count lines, words, bytes"),
            ("head", "first lines (-n N)"),
            ("tail", "last lines (-n N)"),
            ("sort", "sort lines (-r, -n)"),
            ("uniq", "drop repeated lines (-c count)"),
            ("tee", "copy input to files and output"),
            ("yes", "repeat a line forever"),
        ],
    ),
    (
        "processes",
        "running programs",
        &[
            ("ps", "list processes"),
            ("top", "live process view (q quits)"),
            ("kill", "send a signal (kill -9 PID)"),
            ("pkill", "signal processes by name"),
            ("pgrep", "find processes by name"),
            ("sleep", "wait N seconds"),
            ("env", "show the environment"),
        ],
    ),
    (
        "system",
        "the system",
        &[
            ("fastfetch", "system information, with the logo"),
            ("uname", "system name (-a everything)"),
            ("date", "the date and time"),
            ("uptime", "time since boot"),
            ("free", "memory use"),
            ("mount", "mounted filesystems (mount -t proc|tmpfs X DIR mounts)"),
            ("alpine", "Alpine Linux, where apk add installs packages (root)"),
            ("dmesg", "the boot log"),
            ("clear", "clear the screen"),
            ("about", "about PalladiumOS"),
            ("reboot", "restart"),
            ("poweroff", "turn off"),
        ],
    ),
    (
        "users",
        "users and logging in",
        &[
            ("whoami", "your user name"),
            ("id", "your user and group IDs"),
            ("groups", "your groups"),
            ("su", "become another user (root by default)"),
            ("passwd", "change a password"),
            ("sudo", "run a command as root (the wheel group, with your own password)"),
            ("visudo", "edit /etc/sudoers, who may use sudo (checks it before saving)"),
            ("login", "log in"),
            ("useradd", "add a user (root)"),
            ("userdel", "remove a user (root)"),
        ],
    ),
    (
        "network",
        "the network",
        &[
            ("dropbear", "SSH server (dropbear -R: then ssh -p 2222 root@localhost)"),
            ("dbclient", "SSH client (dbclient user@host)"),
            ("scp", "copy files over SSH"),
            ("dropbearkey", "make an SSH key (dropbearkey -t ed25519 -f FILE)"),
            ("wget", "download a URL (wget -O file http://...)"),
            ("nslookup", "look up a name in DNS"),
            ("ping", "check a host answers (as root: ping -c 3 HOST)"),
            ("nc", "connect to (or listen on) a TCP/UDP port"),
            ("telnet", "talk to a TCP server"),
            ("httpd", "a web server (httpd -f -p 8080 -h DIR)"),
            ("ftpget", "download a file over FTP"),
        ],
    ),
    (
        "graphics",
        "the X Window System",
        &[
            ("startx", "X with IceWM and a terminal (Logout, or Ctrl+Alt+Backspace: back; startx fvwm95)"),
            ("icewm", "the window manager: a taskbar, a menu (Ctrl+Esc), workspaces, themes"),
            ("fvwm95", "a window manager like Windows 95: a taskbar and a Start menu"),
            ("Xfbdev", "the X server, on the whole screen (Xfbdev &; X programs use DISPLAY=:0)"),
            ("evilwm", "a window manager (move: Alt+drag, resize: Alt+middle-drag, terminal: Ctrl+Alt+Enter)"),
            ("rxvt", "a terminal window (rxvt &, or rxvt -e PROGRAM)"),
            ("xterm", "X's own terminal (xterm &, or xterm -e PROGRAM)"),
            ("links", "a web browser: links -g URL in X (pictures too), links URL in a terminal"),
            ("xfm", "a file manager: folder windows, drag and drop"),
            ("doom", "DOOM, with Freedoom's levels (arrows, Ctrl fires, Space opens)"),
            ("xboard", "chess against Fairy-Max"),
            ("xjewel", "falling jewels: line up three"),
            ("xmahjongg", "mahjongg solitaire: take matching free tiles"),
            ("dosbox", "a PC with DOS in a window (C: is /usr/share/games/dos)"),
            ("mpg123", "play MP3 files: mpg123 song.mp3 (-Z: a shuffled playlist; q quits)"),
            ("out123", "play raw sound or a tone: out123 --wave-freq 440 --timelimit 44100"),
            ("ogg123", "play Ogg Vorbis files: ogg123 song.ogg (-z: shuffle)"),
            ("oggenc", "make an Ogg Vorbis file from a WAV: oggenc song.wav (-q 0..10: quality)"),
            ("oggdec", "make a WAV from an Ogg Vorbis file: oggdec song.ogg"),
            ("sox", "convert, change and play sound: sox in.wav out.ogg rate 22050; sox song.ogg -d (-d: the sound card)"),
            ("soxi", "a sound file's details (rate, channels, length)"),
            ("nes", "a NES emulator: a menu of the games (or nes GAME.nes); Z/X, Enter, arrows"),
            ("wordlos", "WORDLOS, Wordle for DOS (in DOSBox)"),
            ("floppybird", "Floppy Bird, for DOS (in DOSBox; Space flaps)"),
            ("xsnow", "snow on the desktop (xroach: roaches under the windows)"),
            ("xpmroot", "set the desktop's background to an XPM picture"),
            ("xcalc", "a calculator"),
            ("xclock", "a clock (xclock -digital for digits)"),
            ("xeyes", "eyes that follow the mouse"),
            ("xlogo", "the X Window System's logo"),
            ("xedit", "a text editor (xedit FILE)"),
            ("xmessage", "show a message in a window (xmessage 'hello')"),
            ("xwd", "a screenshot (xwd -root -out FILE; xwud -in FILE shows it)"),
            ("xev", "show the X events a window gets (keys, the mouse)"),
        ],
    ),
    (
        "sound",
        "sound (a sound card: QEMU's -device AC97)",
        &[
            ("play", "play WAV files, or a tone: play -t 440 1"),
            ("volume", "show or set the volume: volume 70"),
        ],
    ),
    (
        "programming",
        "compilers, languages and other shells",
        &[
            ("tcc", "C compiler: tcc hi.c -o hi, or tcc -run hi.c"),
            ("vim", "Vim, the editor (vi is BusyBox's)"),
            ("tclsh", "Tcl 8.6"),
            ("wish", "Tcl with Tk, for windows in X (try wish /usr/lib/tk8.6/demos/widget)"),
            ("lua", "Lua 5.4"),
            ("luac", "Lua compiler"),
            ("micropython", "MicroPython (Python 3)"),
            ("dash", "the Debian Almquist shell (POSIX sh)"),
            ("sh", "this shell"),
            ("busybox", "400 more tools (see help busybox)"),
            ("chello", "hello world in C"),
            ("hello", "hello world in Rust"),
        ],
    ),
];

/// One line per builtin.
const BUILTIN_HELP: &[(&str, &str)] = &[
    ("cd", "change directory (cd alone goes home)"),
    ("pwd", "print the current directory"),
    ("exit", "leave the shell (exit N: with status N)"),
    ("export", "export NAME=value to programs"),
    ("unset", "remove a variable or function"),
    ("local", "local NAME[=value]: a variable for this function call"),
    ("alias", "alias ll='ls -l': a short name for a command (unalias removes)"),
    ("set", "shell options: set -e (stop on errors), set -x (trace)"),
    ("read", "read a line into variables"),
    ("test", "check files and values ([ is the same)"),
    ("jobs", "list background and stopped jobs"),
    ("fg", "continue a job in the foreground"),
    ("bg", "continue a job in the background"),
    ("wait", "wait for background jobs"),
    ("kill", "send a signal (also to %1 jobs)"),
    ("trap", "run commands on a signal or EXIT"),
    ("type", "say what a name is (builtin, function, program)"),
    ("eval", "run text as commands"),
    ("source", "run a script in this shell (also: .)"),
    ("shift", "drop the first script argument"),
    ("break", "leave a loop"),
    ("continue", "next round of a loop"),
    ("return", "leave a function"),
    ("true", "do nothing, successfully (also :)"),
    ("false", "do nothing, unsuccessfully"),
    ("run", "run a program (like the kernel shell)"),
    ("script", "run a script file"),
    ("help", "this help"),
];

fn exists(path: &str) -> bool {
    stat(path).is_ok()
}

/// Is /bin/NAME a link to BusyBox (one of the tools it provides instead)?
fn is_busybox_link(name: &str) -> bool {
    let mut buf = [0u8; 64];
    readlink(&alloc::format!("/bin/{}", name), &mut buf).is_ok_and(|t| t == "/bin/busybox")
}

/// BusyBox's tools in /usr/bin: the links to it there (other programs on
/// the system disk, like fvwm95, aren't).
fn busybox_tools() -> Vec<String> {
    let mut buf = [0u8; 64];
    dir_names("/usr/bin")
        .into_iter()
        .filter(|t| readlink(&alloc::format!("/usr/bin/{}", t), &mut buf).is_ok_and(|l| l == "/bin/busybox"))
        .collect()
}

/// The names in a directory (sorted).
fn dir_names(path: &str) -> Vec<String> {
    let fd = open(path, O_RDONLY | O_DIRECTORY);
    let mut names = Vec::new();
    if fd >= 0 {
        let _ = for_each_entry(fd as i32, |e| {
            if !e.name.starts_with('.') {
                names.push(e.name.to_string());
            }
        });
        close(fd as i32);
    }
    names.sort();
    names
}

fn known(name: &str) -> bool {
    CATEGORIES.iter().any(|(_, _, progs)| progs.iter().any(|(n, _)| *n == name))
}

/// Programs in /bin that no category lists (tests are their own topic).
fn others() -> Vec<String> {
    dir_names("/bin")
        .into_iter()
        .filter(|n| !known(n) && !n.starts_with("test_") && !stat(&alloc::format!("/bin/{}", n)).is_ok_and(|s| s.is_dir()))
        .collect()
}

fn index() {
    println!("PalladiumOS shell help. Type 'help TOPIC' (or 'help COMMAND'):");
    for (topic, about, progs) in CATEGORIES {
        let present: Vec<&str> =
            progs.iter().map(|(n, _)| *n).filter(|n| installed(n)).collect();
        if present.is_empty() {
            continue;
        }
        // As many example names as fit on an 80-column line.
        let room = 78usize.saturating_sub(2 + 13 + about.len() + 3 + 4);
        let mut sample = String::new();
        let mut shown = 0;
        for name in &present {
            if sample.len() + name.len() + 1 > room {
                break;
            }
            if !sample.is_empty() {
                sample.push(' ');
            }
            sample.push_str(name);
            shown += 1;
        }
        if shown < present.len() {
            sample.push_str(" ...");
        }
        println!("  {:<12} {} ({})", topic, about, sample);
    }
    if !others().is_empty() {
        println!("  {:<12} programs not in a category", "other");
    }
    if exists("/usr/bin") {
        println!("  {:<12} {} more tools: vi awk sed find tar ...", "busybox", busybox_tools().len());
    }
    println!("  {:<12} shell commands: cd export jobs fg trap set ...", "builtins");
    println!("  {:<12} pipes, redirection, variables, if/for/while", "syntax");
    println!("  {:<12} line editing, Ctrl-C, Ctrl-Z, history", "keys");
    println!("Run a program by typing its name, e.g. ls -l /bin.");
}

/// In /bin, or /usr/bin (BusyBox's tools).
fn installed(name: &str) -> bool {
    exists(&alloc::format!("/bin/{}", name)) || exists(&alloc::format!("/usr/bin/{}", name))
}

fn category(progs: &[(&str, &str)]) {
    for (name, desc) in progs {
        if installed(name) {
            println!("  {:<12} {}", name, desc);
        }
    }
}

fn builtins() {
    println!("Shell builtins (they run inside the shell):");
    for (name, desc) in BUILTIN_HELP {
        println!("  {:<10} {}", name, desc);
    }
    let undocumented: Vec<&str> =
        BUILTINS.iter().copied().filter(|b| !BUILTIN_HELP.iter().any(|(n, _)| n == b)).collect();
    if !undocumented.is_empty() {
        println!("Also: {}", undocumented.join(" "));
    }
}

fn syntax() {
    println!("Shell syntax:");
    println!("  cmd1 | cmd2          pipe output into the next command");
    println!("  cmd1; cmd2           one after the other");
    println!("  cmd1 && cmd2         cmd2 only if cmd1 worked (|| if it failed)");
    println!("  cmd &                run in the background (see jobs, fg, bg)");
    println!("  > file  >> file      write / append output (2> for errors, 2>&1 both)");
    println!("  < file               read input from a file");
    println!("  cat <<EOF ... EOF    a here-document: the lines up to EOF as input");
    println!("                       (<<'EOF': no $ expansion; <<-EOF: tabs removed)");
    println!("  'text'  \"$VAR text\"  quoting; \\ escapes one character");
    println!("  $VAR ${{VAR:-default}} variables (NAME=value sets one)");
    println!("  $(cmd)  $((1 + 2))   command output, arithmetic");
    println!("  *.txt  file?  [ab]*  file name patterns");
    println!("  if cmd; then ...; elif ...; else ...; fi");
    println!("  while cmd; do ...; done      for x in a b c; do ...; done");
    println!("  case $x in a*) ...;; *) ...;; esac");
    println!("  name() {{ ... }}       define a function ($1, $2..., $#, $@)");
    println!("  local x=1            a variable for this function call only");
    println!("  alias ll='ls -l'     a short name for a command (unalias ll)");
    println!("Scripts: sh script.sh args, or chmod +x it (with a #!/bin/sh line).");
}

fn keys() {
    println!("Keys:");
    println!("  Up / Down            previous / next command (history)");
    println!("  Left / Right, Home / End   move in the line");
    println!("  Tab                  complete a command or file name");
    println!("  Ctrl-C               interrupt the running program (or clear the line)");
    println!("  Ctrl-Z               stop the running program (then fg or bg)");
    println!("  Ctrl-\\               quit the running program");
    println!("  Ctrl-D               end of input (exits the shell)");
    println!("  Ctrl-L               clear the screen");
}

fn busybox() {
    let tools = busybox_tools();
    if tools.is_empty() {
        println!("BusyBox isn't installed (build it with tools/ports/busybox.sh).");
        return;
    }
    let ours = |t: &String| exists(&alloc::format!("/bin/{}", t)) && !is_busybox_link(t);
    let shadowed: Vec<&String> = tools.iter().filter(|t| ours(t)).collect();
    println!("BusyBox tools in /usr/bin ({}):", tools.len());
    print_wrapped(tools.iter().filter(|t| !ours(t)).map(String::as_str));
    println!("PalladiumOS's versions come first for these ('busybox NAME' runs BusyBox's):");
    print_wrapped(shadowed.iter().map(|s| s.as_str()));
    let replaced = dir_names("/bin/palladium");
    if !replaced.is_empty() {
        println!("BusyBox's versions are the defaults for these (originals: /bin/palladium):");
        print_wrapped(replaced.iter().map(String::as_str));
    }
    println!("Most tools explain themselves with --help (e.g. awk --help).");
}

/// One command (or builtin): what it is.
fn command(name: &str) -> bool {
    if let Some((_, desc)) = BUILTIN_HELP.iter().find(|(n, _)| *n == name) {
        println!("{}: shell builtin: {}", name, desc);
        return true;
    }
    for (topic, _, progs) in CATEGORIES {
        if let Some((_, desc)) = progs.iter().find(|(n, _)| *n == name) {
            println!("{}: {} (in 'help {}')", name, desc, topic);
            return true;
        }
    }
    if exists(&alloc::format!("/bin/{}", name)) || exists(&alloc::format!("/usr/bin/{}", name)) {
        println!("{}: a program; try '{} --help'", name, name);
        return true;
    }
    false
}

pub fn help(args: &[String]) -> i32 {
    let Some(topic) = args.get(1).map(String::as_str) else {
        index();
        return 0;
    };
    match topic {
        "builtins" => builtins(),
        "syntax" | "scripting" => syntax(),
        "keys" => keys(),
        "busybox" => busybox(),
        "other" => print_wrapped(others().iter().map(String::as_str)),
        "tests" => print_wrapped(dir_names("/bin").iter().filter(|n| n.starts_with("test_")).map(String::as_str)),
        _ => {
            if let Some((_, about, progs)) = CATEGORIES.iter().find(|(t, _, _)| *t == topic) {
                println!("{}{}:", about[..1].to_uppercase(), &about[1..]);
                category(progs);
            } else if !command(topic) {
                eprintln!("help: no topic or command '{}' (type 'help' for the list)", topic);
                return 1;
            }
        }
    }
    0
}
