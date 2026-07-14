//! A window is a titled, draggable/resizable rectangle on the desktop.
//! Content is drawn immediate-mode -- no retained widget tree -- by
//! whichever `WindowContent` variant it holds; `draw()` is the only thing
//! that touches the screen, `step_animation()` is the only thing that
//! mutates content state on its own (independent of input events).

use crate::demo_tasks;
use crate::fs::simplefs;
use crate::graphics::dirty_rect::Rect;
use crate::graphics::{self, Color, LINE_HEIGHT};
use alloc::format;
use alloc::string::String;
use alloc::vec;
use alloc::vec::Vec;
use pc_keyboard::{DecodedKey, KeyCode};

pub const TITLEBAR_HEIGHT: usize = 20;
pub const CLOSE_BTN_SIZE: usize = 14;
pub const RESIZE_HANDLE: usize = 10;
pub const MIN_WIDTH: usize = 150;
pub const MIN_HEIGHT: usize = 90;

const BOX_SIZE: i32 = 18;
const TOP_MARGIN: i32 = 34; // room for two HUD text lines above the bouncing box

pub type WindowId = u32;

pub enum WindowContent {
    /// Live scheduler counters + a bouncing box, bounced off this window's
    /// *own* content bounds (not the screen) -- proves per-window state
    /// survives drag/resize independently of every other window.
    Stats { box_x: i32, box_y: i32, vx: i32, vy: i32 },
    /// A real command line over SimpleFS: `help`, `ls`, `cat <name>`,
    /// `write <name> <text...>`. Only receives keystrokes while focused.
    Terminal { input: String, history: Vec<String> },
    /// Combined file browser + text editor: `Browsing` lists the root
    /// directory (arrows to move, Enter to open, `n` to create a new
    /// file); `Editing` holds the loaded buffer (F2 saves, Escape goes
    /// back to browsing).
    Editor(EditorState),
}

pub enum EditorState {
    Browsing { entries: Vec<(String, u32, u32)>, selected: usize },
    Editing { name: String, inode: u32, buf: String, status: String },
}

impl WindowContent {
    pub fn new_terminal() -> Self {
        WindowContent::Terminal {
            input: String::new(),
            history: vec![String::from("Bedrock terminal -- type 'help'.")],
        }
    }

    pub fn new_editor() -> Self {
        WindowContent::Editor(EditorState::Browsing { entries: simplefs::list_root(), selected: 0 })
    }
}

/// A tiny shell over SimpleFS: `ls`/`cat`/`write` are the classic trio
/// needed to prove read + write + directory listing all work, without
/// building a whole command grammar.
fn run_terminal_command(cmd: &str) -> String {
    let cmd = cmd.trim();
    if cmd.is_empty() {
        return String::new();
    }
    let mut parts = cmd.splitn(2, ' ');
    let verb = parts.next().unwrap_or("");
    let rest = parts.next().unwrap_or("").trim();
    match verb {
        "help" => String::from("commands: help, ls, cat <name>, write <name> <text>"),
        "ls" => {
            let entries = simplefs::list_root();
            if entries.is_empty() {
                String::from("(empty)")
            } else {
                let mut s = String::new();
                for (name, _, size) in entries {
                    s.push_str(&format!("{} ({}B)\n", name, size));
                }
                s.trim_end().into()
            }
        }
        "cat" => {
            if rest.is_empty() {
                return String::from("usage: cat <name>");
            }
            match simplefs::list_root().into_iter().find(|(n, _, _)| n == rest) {
                Some((_, inode, _)) => match simplefs::read_file(inode) {
                    Ok(bytes) => String::from_utf8_lossy(&bytes).into_owned(),
                    Err(e) => format!("read error: {}", e),
                },
                None => format!("no such file: {}", rest),
            }
        }
        "write" => {
            let mut wparts = rest.splitn(2, ' ');
            let name = wparts.next().unwrap_or("");
            let text = wparts.next().unwrap_or("");
            if name.is_empty() {
                return String::from("usage: write <name> <text>");
            }
            let inode = match simplefs::list_root().into_iter().find(|(n, _, _)| n == name) {
                Some((_, inode, _)) => Some(inode),
                None => simplefs::create_file(name).ok(),
            };
            match inode {
                Some(id) => match simplefs::write_file(id, text.as_bytes()) {
                    Ok(()) => String::from("OK"),
                    Err(e) => format!("write error: {}", e),
                },
                None => String::from("could not create file"),
            }
        }
        other => format!("unknown command: {} (try 'help')", other),
    }
}

/// Handles one keystroke for the file-browser/editor content, returning
/// whether it changed anything (and so needs a repaint).
fn handle_editor_key(state: &mut EditorState, key: DecodedKey) -> bool {
    match state {
        EditorState::Browsing { entries, selected } => match key {
            DecodedKey::RawKey(KeyCode::ArrowUp) => {
                *selected = selected.saturating_sub(1);
                true
            }
            DecodedKey::RawKey(KeyCode::ArrowDown) => {
                if *selected + 1 < entries.len() {
                    *selected += 1;
                }
                true
            }
            DecodedKey::Unicode('\r') | DecodedKey::Unicode('\n') => {
                if let Some((name, inode, _)) = entries.get(*selected).cloned() {
                    let buf = simplefs::read_file(inode)
                        .map(|b| String::from_utf8_lossy(&b).into_owned())
                        .unwrap_or_default();
                    *state = EditorState::Editing { name, inode, buf, status: String::from("Loaded.") };
                }
                true
            }
            DecodedKey::Unicode('n') | DecodedKey::Unicode('N') => {
                let name = format!("note{}.txt", entries.len());
                match simplefs::create_file(&name) {
                    Ok(inode) => {
                        *state = EditorState::Editing { name, inode, buf: String::new(), status: String::from("New file.") };
                    }
                    Err(e) => crate::serial_println!("editor: create_file({}) failed: {}", name, e),
                }
                true
            }
            _ => false,
        },
        EditorState::Editing { inode, buf, status, .. } => match key {
            DecodedKey::Unicode('\u{1b}') => {
                *state = EditorState::Browsing { entries: simplefs::list_root(), selected: 0 };
                true
            }
            DecodedKey::RawKey(KeyCode::F2) => {
                *status = match simplefs::write_file(*inode, buf.as_bytes()) {
                    Ok(()) => String::from("Saved."),
                    Err(e) => format!("Save failed: {}", e),
                };
                true
            }
            DecodedKey::Unicode('\u{8}') => {
                buf.pop();
                true
            }
            DecodedKey::Unicode('\r') | DecodedKey::Unicode('\n') => {
                buf.push('\n');
                true
            }
            DecodedKey::Unicode(c) if !c.is_control() => {
                if buf.len() < 4096 {
                    buf.push(c);
                }
                true
            }
            _ => false,
        },
    }
}

pub struct Window {
    #[allow(dead_code)] // identity handle for future multi-window IPC/focus APIs
    pub id: WindowId,
    pub rect: Rect,
    pub title: String,
    pub content: WindowContent,
}

impl Window {
    pub fn titlebar_rect(&self) -> Rect {
        Rect::new(self.rect.x, self.rect.y, self.rect.w, TITLEBAR_HEIGHT)
    }

    pub fn close_button_rect(&self) -> Rect {
        let pad = 3;
        Rect::new(
            self.rect.right().saturating_sub(CLOSE_BTN_SIZE + pad),
            self.rect.y + pad,
            CLOSE_BTN_SIZE,
            CLOSE_BTN_SIZE,
        )
    }

    pub fn resize_handle_rect(&self) -> Rect {
        Rect::new(
            self.rect.right().saturating_sub(RESIZE_HANDLE),
            self.rect.bottom().saturating_sub(RESIZE_HANDLE),
            RESIZE_HANDLE,
            RESIZE_HANDLE,
        )
    }

    pub fn content_rect(&self) -> Rect {
        Rect::new(
            self.rect.x + 1,
            self.rect.y + TITLEBAR_HEIGHT,
            self.rect.w.saturating_sub(2),
            self.rect.h.saturating_sub(TITLEBAR_HEIGHT + 1),
        )
    }

    /// Steps this window's own animation (if it has one), returning the
    /// screen-space rect that needs repainting because of it -- the
    /// bounding-box union of the box's old and new position, same merge
    /// primitive the dirty-rect tracker uses.
    pub fn step_animation(&mut self) -> Option<Rect> {
        let cr = self.content_rect();
        let WindowContent::Stats { box_x, box_y, vx, vy } = &mut self.content else {
            return None;
        };
        let max_x = (cr.w as i32 - BOX_SIZE - 2).max(1);
        let max_y = (cr.h as i32 - BOX_SIZE - 2).max(TOP_MARGIN + 1);

        let old = Rect::new(
            (cr.x as i32 + *box_x).max(0) as usize,
            (cr.y as i32 + *box_y).max(0) as usize,
            BOX_SIZE as usize,
            BOX_SIZE as usize,
        );

        *box_x += *vx;
        *box_y += *vy;
        if *box_x <= 0 || *box_x >= max_x {
            *vx = -*vx;
            *box_x = (*box_x).clamp(0, max_x);
        }
        if *box_y <= TOP_MARGIN || *box_y >= max_y {
            *vy = -*vy;
            *box_y = (*box_y).clamp(TOP_MARGIN, max_y);
        }

        let new = Rect::new(
            (cr.x as i32 + *box_x).max(0) as usize,
            (cr.y as i32 + *box_y).max(0) as usize,
            BOX_SIZE as usize,
            BOX_SIZE as usize,
        );
        Some(old.union(&new))
    }

    /// Routes one decoded keystroke into this window's content, if it
    /// accepts keyboard input. Returns the rect to repaint, if anything
    /// changed. Called only for the currently *focused* window -- the
    /// smallest possible proof that keyboard focus routing works.
    pub fn handle_key(&mut self, key: DecodedKey) -> Option<Rect> {
        let rect = self.rect;
        let changed = match &mut self.content {
            WindowContent::Stats { .. } => false,
            WindowContent::Terminal { input, history } => match key {
                DecodedKey::Unicode('\u{8}') => {
                    input.pop();
                    true
                }
                DecodedKey::Unicode('\r') | DecodedKey::Unicode('\n') => {
                    let cmd = core::mem::take(input);
                    let output = run_terminal_command(&cmd);
                    history.push(format!("> {}", cmd));
                    for line in output.lines() {
                        history.push(String::from(line));
                    }
                    const HISTORY_CAP: usize = 200;
                    if history.len() > HISTORY_CAP {
                        let excess = history.len() - HISTORY_CAP;
                        history.drain(0..excess);
                    }
                    true
                }
                DecodedKey::Unicode(c) if !c.is_control() => {
                    if input.len() < 120 {
                        input.push(c);
                    }
                    true
                }
                _ => false,
            },
            WindowContent::Editor(state) => handle_editor_key(state, key),
        };
        if changed {
            Some(rect)
        } else {
            None
        }
    }

    pub fn draw(&self, g: &mut graphics::Graphics, focused: bool) {
        let border = if focused { Color::CYAN } else { Color::rgb(80, 84, 100) };
        let titlebar_bg = if focused { Color::rgb(40, 70, 90) } else { Color::rgb(35, 37, 46) };

        // Border, then an inset fill, gives a 1px frame without a dedicated
        // stroke primitive.
        g.fill_rect(self.rect, border);
        let inner = Rect::new(
            self.rect.x + 1,
            self.rect.y + 1,
            self.rect.w.saturating_sub(2),
            self.rect.h.saturating_sub(2),
        );
        g.fill_rect(inner, Color::BG);

        g.fill_rect(self.titlebar_rect(), titlebar_bg);
        g.draw_string(self.rect.x + 6, self.rect.y + 3, &self.title, Color::WHITE, titlebar_bg);

        let cb = self.close_button_rect();
        g.fill_rect(cb, Color::rgb(180, 60, 60));
        g.draw_string(cb.x + 3, cb.y, "x", Color::WHITE, Color::rgb(180, 60, 60));

        let rh = self.resize_handle_rect();
        g.fill_rect(rh, Color::rgb(60, 63, 76));

        self.draw_content(g, focused);
    }

    fn draw_content(&self, g: &mut graphics::Graphics, focused: bool) {
        let cr = self.content_rect();
        match &self.content {
            WindowContent::Stats { box_x, box_y, .. } => {
                let counters = demo_tasks::snapshot();
                let l1 = format!("{}={} {}={}", counters[0].0, counters[0].1, counters[1].0, counters[1].1);
                let l2 = format!(
                    "{}={} {}={} {}={}",
                    counters[2].0, counters[2].1, counters[3].0, counters[3].1, counters[4].0, counters[4].1
                );
                g.draw_string(cr.x + 4, cr.y + 4, &l1, Color::GREEN, Color::BG);
                g.draw_string(cr.x + 4, cr.y + 4 + LINE_HEIGHT, &l2, Color::GREEN, Color::BG);
                let bx = (cr.x as i32 + *box_x).max(0) as usize;
                let by = (cr.y as i32 + *box_y).max(0) as usize;
                g.fill_rect(Rect::new(bx, by, BOX_SIZE as usize, BOX_SIZE as usize), Color::ORANGE);
            }
            WindowContent::Terminal { input, history } => {
                let max_lines = cr.h.saturating_sub(4) / LINE_HEIGHT;
                let total_lines = history.len() + 1; // +1 for the live input line
                let skip = total_lines.saturating_sub(max_lines).min(history.len());
                let mut y = cr.y + 4;
                for line in history.iter().skip(skip) {
                    if y + LINE_HEIGHT > cr.bottom() {
                        break;
                    }
                    g.draw_string(cr.x + 4, y, line, Color::GREEN, Color::BG);
                    y += LINE_HEIGHT;
                }
                let prompt_color = if focused { Color::WHITE } else { Color::rgb(120, 122, 130) };
                let prompt = format!("{}{}", if focused { "> " } else { "  " }, input);
                if y + LINE_HEIGHT <= cr.bottom() {
                    g.draw_string(cr.x + 4, y, &prompt, prompt_color, Color::BG);
                }
            }
            WindowContent::Editor(EditorState::Browsing { entries, selected }) => {
                g.draw_string(cr.x + 4, cr.y + 4, "Enter=open  N=new  arrows=move", Color::CYAN, Color::BG);
                let mut y = cr.y + 4 + LINE_HEIGHT;
                if entries.is_empty() {
                    g.draw_string(cr.x + 4, y, "(no files yet -- press N)", Color::WHITE, Color::BG);
                }
                for (i, (name, _, size)) in entries.iter().enumerate() {
                    if y + LINE_HEIGHT > cr.bottom() {
                        break;
                    }
                    let marker = if i == *selected { ">" } else { " " };
                    let color = if i == *selected { Color::ORANGE } else { Color::WHITE };
                    let line = format!("{} {} ({}B)", marker, name, size);
                    g.draw_string(cr.x + 4, y, &line, color, Color::BG);
                    y += LINE_HEIGHT;
                }
            }
            WindowContent::Editor(EditorState::Editing { name, buf, status, .. }) => {
                let header = format!("Editing {}  (F2=save  Esc=back)  {}", name, status);
                g.draw_string(cr.x + 4, cr.y + 4, &header, Color::CYAN, Color::BG);
                let mut y = cr.y + 4 + LINE_HEIGHT;
                for line in buf.split('\n') {
                    if y + LINE_HEIGHT > cr.bottom() {
                        break;
                    }
                    g.draw_string(cr.x + 4, y, line, Color::WHITE, Color::BG);
                    y += LINE_HEIGHT;
                }
            }
        }
    }
}
