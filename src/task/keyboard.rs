// Copyright (c) 2026 tre4surehunter9
use alloc::string::String;

use conquer_once::spin::OnceCell;
use crossbeam_queue::ArrayQueue;

static SCANCODE_QUEUE: OnceCell<ArrayQueue<u8>> = OnceCell::uninit();

use crate::println;

pub struct ScancodeStream {
    _private: (),
}

impl ScancodeStream {
    pub fn new() -> Self {
        init_queue();
        ScancodeStream { _private: () }
    }
}

/// Work around a pc-keyboard 0.7 bug: scancode set 1 decodes the
/// backslash key (0x2B) as Oem7, but the US layout only maps that key as
/// Oem5, so the backslash and '|' characters could not be typed at all.
pub fn fix_key_event(mut event: pc_keyboard::KeyEvent) -> pc_keyboard::KeyEvent {
    if event.code == pc_keyboard::KeyCode::Oem7 {
        event.code = pc_keyboard::KeyCode::Oem5;
    }
    event
}

/// Create the scancode queue if it doesn't exist yet. Safe to call repeatedly.
pub fn init_queue() {
    // Room for plenty of type-ahead (each key press is 2 scancodes, 4 with Shift).
    let _ = SCANCODE_QUEUE.try_init_once(|| ArrayQueue::new(1024));
}

/// Take the next scancode without waiting (used by the TTY while a
/// user program is running and the async executor isn't).
pub fn pop_scancode() -> Option<u8> {
    SCANCODE_QUEUE.try_get().ok()?.pop()
}

use core::{pin::Pin, task::{Poll, Context}};
use futures_util::stream::Stream;
use futures_util::task::AtomicWaker;

static WAKER: AtomicWaker = AtomicWaker::new();

impl Stream for ScancodeStream {
    type Item = u8;

    fn poll_next(self: Pin<&mut Self>, cx: &mut Context) -> Poll<Option<u8>> {
        let queue = SCANCODE_QUEUE
            .try_get()
            .expect("scancode queue not initialized");

        if let Some(scancode) = queue.pop() {
            return Poll::Ready(Some(scancode));
        }

        WAKER.register(&cx.waker());
        match queue.pop() {
            Some(scancode) => {
                WAKER.take();
                Poll::Ready(Some(scancode))
            }
            None => Poll::Pending,
        }
    }
}

pub fn add_scancode(scancode: u8) {
    if let Ok(queue) = SCANCODE_QUEUE.try_get() {
        if let Err(_) = queue.push(scancode) {
            println!("WARNING: scancode queue full; dropping keyboard input");
        } else {
            WAKER.wake();
        }
    } else {
        println!("WARNING: scancode queue uninitialized");
    }
}

use futures_util::stream::StreamExt;
use pc_keyboard::{layouts, DecodedKey, HandleControl, Keyboard, ScancodeSet1};
use crate::print;
use crate::shell;
use crate::vga_buffer::{draw_cursor, erase_cursor};
use crate::editor::{EDITOR, EDITOR_ACTIVE};
use core::sync::atomic::Ordering;

pub async fn run_shell() {
    let mut scancodes = ScancodeStream::new();
    let mut keyboard = Keyboard::new(
        ScancodeSet1::new(),
        layouts::Us104Key,
        HandleControl::MapLettersToUnicode,
    );

    let mut input_buffer = String::new();

    print_prompt();
    draw_cursor();

    loop {
        if let Some(scancode) = scancodes.next().await {
            if let Ok(Some(key_event)) = keyboard.add_byte(scancode) {
                let key_event = fix_key_event(key_event);
                // Route to editor if active
                if EDITOR_ACTIVE.load(Ordering::SeqCst) {
                    handle_editor_key(key_event, &mut keyboard);
                    continue;
                }

                if let Some(key) = keyboard.process_keyevent(key_event) {
                    match key {
                        DecodedKey::Unicode('\n') => {
                            erase_cursor();
                            println!();
                            shell::process_command(&input_buffer);
                            // A program may have had the keyboard and seen
                            // key presses whose releases we never got (or the
                            // other way round): start with no keys held.
                            keyboard = Keyboard::new(ScancodeSet1::new(), layouts::Us104Key, HandleControl::MapLettersToUnicode);
                            input_buffer.clear();
                            print_prompt();
                            draw_cursor();
                        }
                        DecodedKey::Unicode('\x08') => {
                            if !input_buffer.is_empty() {
                                erase_cursor();
                                input_buffer.pop();
                                print!("\x08");
                                draw_cursor();
                            }
                        }
                        DecodedKey::Unicode(c) if !c.is_control() => {
                            erase_cursor();
                            input_buffer.push(c);
                            print!("{}", c);
                            draw_cursor();
                        }
                        _ => {}
                    }
                }
            }
        }
    }
}

fn handle_editor_key(
    key_event: pc_keyboard::KeyEvent,
    keyboard: &mut Keyboard<layouts::Us104Key, ScancodeSet1>,
) {
    use pc_keyboard::KeyCode;
    // Every event must reach the decoder, including key releases: otherwise
    // it never sees Ctrl/Shift being let go, and they stay "held" (after
    // Ctrl+S, letters turned into control codes). It returns None for releases.
    if let Some(key) = keyboard.process_keyevent(key_event) {
        match key {
            DecodedKey::Unicode('\x13') => {
                crate::editor::save();
                if let Some(ed) = EDITOR.lock().as_ref() { ed.render(); }
            }
            DecodedKey::Unicode('\x11') => {
                crate::editor::close();
            }
            DecodedKey::Unicode('\n') => {
                if let Some(ed) = EDITOR.lock().as_mut() { ed.newline(); ed.render(); }
            }
            DecodedKey::Unicode('\x08') => {
                if let Some(ed) = EDITOR.lock().as_mut() { ed.backspace(); ed.render(); }
            }
            DecodedKey::Unicode(c) if !c.is_control() => {
                if let Some(ed) = EDITOR.lock().as_mut() { ed.insert_char(c); ed.render(); }
            }
            DecodedKey::RawKey(KeyCode::ArrowUp) => {
                if let Some(ed) = EDITOR.lock().as_mut() { ed.move_up(); ed.render(); }
            }
            DecodedKey::RawKey(KeyCode::ArrowDown) => {
                if let Some(ed) = EDITOR.lock().as_mut() { ed.move_down(); ed.render(); }
            }
            DecodedKey::RawKey(KeyCode::ArrowLeft) => {
                if let Some(ed) = EDITOR.lock().as_mut() { ed.move_left(); ed.render(); }
            }
            DecodedKey::RawKey(KeyCode::ArrowRight) => {
                if let Some(ed) = EDITOR.lock().as_mut() { ed.move_right(); ed.render(); }
            }
            _ => {}
        }
    }
}

fn print_prompt() {
    let cwd = crate::filesystem::CWD.lock().clone();
    crate::print!("PalladiumOS:{} > ", cwd);
}
