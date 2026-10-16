//! Monochrome & grayscale terminal styling, luminance gradients, grayscale pill badges,
//! box-drawing widgets, and multi-stage terminal animations (`LiveSpinner`, `animate_stage`, `animate_sweep`).
#![allow(dead_code)]

use std::env;
use std::io::{IsTerminal, Write};
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicBool, Ordering},
};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ColorChoice {
    Auto,
    Always,
    Never,
}

impl ColorChoice {
    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "auto" => Some(Self::Auto),
            "always" => Some(Self::Always),
            "never" => Some(Self::Never),
            _ => None,
        }
    }

    pub fn should_color_stdout(self) -> bool {
        match self {
            Self::Always => true,
            Self::Never => false,
            Self::Auto => {
                if env::var_os("NO_COLOR").is_some() {
                    return false;
                }
                if env::var("TERM").map(|t| t == "dumb").unwrap_or(false) {
                    return false;
                }
                std::io::stdout().is_terminal()
            }
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AnimChoice {
    Auto,
    Always,
    Never,
}

impl AnimChoice {
    pub fn should_animate(self, color_enabled: bool) -> bool {
        match self {
            Self::Always => true,
            Self::Never => false,
            Self::Auto => {
                if env::var_os("NO_COLOR").is_some() || env::var_os("CW_NO_ANIM").is_some() {
                    return false;
                }
                if env::var_os("CW_ANIMATE").is_some() {
                    return true;
                }
                color_enabled && (std::io::stdout().is_terminal() || std::io::stderr().is_terminal())
            }
        }
    }
}

const SPINNER_FRAMES: &[&str] = &["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"];
const PULSE_DOTS: &[&str] = &["·", "•", "∙", "•"];

#[derive(Debug, Clone, Copy)]
pub struct Styler {
    pub enabled: bool,
    pub animate: bool,
}

impl Styler {
    pub fn new(choice: ColorChoice) -> Self {
        let enabled = choice.should_color_stdout();
        Self {
            enabled,
            animate: AnimChoice::Auto.should_animate(enabled),
        }
    }

    pub fn with_anim(choice: ColorChoice, anim: AnimChoice) -> Self {
        let enabled = choice.should_color_stdout() || anim == AnimChoice::Always;
        Self {
            enabled,
            animate: anim.should_animate(enabled),
        }
    }

    pub fn for_stdout(choice: ColorChoice) -> Self {
        Self::new(choice)
    }

    pub fn for_stderr(choice: ColorChoice) -> Self {
        let enabled = match choice {
            ColorChoice::Always => true,
            ColorChoice::Never => false,
            ColorChoice::Auto => {
                if env::var_os("NO_COLOR").is_some() {
                    false
                } else if env::var("TERM").map(|t| t == "dumb").unwrap_or(false) {
                    false
                } else {
                    std::io::stderr().is_terminal()
                }
            }
        };
        Self {
            enabled,
            animate: AnimChoice::Auto.should_animate(enabled),
        }
    }

    pub fn is_colored(&self) -> bool {
        self.enabled
    }

    pub fn is_animated(&self) -> bool {
        self.animate
    }

    /// Pure bright white bold (`#FAFAFA`).
    pub fn bold(&self, s: &str) -> String {
        if self.enabled {
            format!("\x1b[1;38;2;250;250;250m{s}\x1b[0m")
        } else {
            s.to_string()
        }
    }

    /// Muted ash gray (`#8A8A8E`).
    pub fn dim(&self, s: &str) -> String {
        if self.enabled {
            format!("\x1b[38;2;138;138;142m{s}\x1b[0m")
        } else {
            s.to_string()
        }
    }

    pub fn italic(&self, s: &str) -> String {
        if self.enabled {
            format!("\x1b[3;38;2;180;180;185m{s}\x1b[0m")
        } else {
            s.to_string()
        }
    }

    /// High-contrast stark white bold with underline for errors/emphasis in monochrome.
    pub fn red_bold(&self, s: &str) -> String {
        if self.enabled {
            format!("\x1b[1;38;2;255;255;255m{s}\x1b[0m")
        } else {
            s.to_string()
        }
    }

    /// Crisp silver-white bold (`#E8E8EC`).
    pub fn green_bold(&self, s: &str) -> String {
        if self.enabled {
            format!("\x1b[1;38;2;232;232;236m{s}\x1b[0m")
        } else {
            s.to_string()
        }
    }

    /// Medium-bright platinum bold (`#D4D4D8`).
    pub fn yellow_bold(&self, s: &str) -> String {
        if self.enabled {
            format!("\x1b[1;38;2;212;212;216m{s}\x1b[0m")
        } else {
            s.to_string()
        }
    }

    /// Soft silver bold (`#C4C4CA`).
    pub fn magenta_bold(&self, s: &str) -> String {
        if self.enabled {
            format!("\x1b[1;38;2;196;196;202m{s}\x1b[0m")
        } else {
            s.to_string()
        }
    }

    /// Bright ice-gray bold (`#E4E4E7`).
    pub fn cyan_bold(&self, s: &str) -> String {
        if self.enabled {
            format!("\x1b[1;38;2;228;228;231m{s}\x1b[0m")
        } else {
            s.to_string()
        }
    }

    pub fn blue_bold(&self, s: &str) -> String {
        if self.enabled {
            format!("\x1b[1;38;2;210;210;215m{s}\x1b[0m")
        } else {
            s.to_string()
        }
    }

    /// Light gray (`#D0D0D5`).
    pub fn cyan(&self, s: &str) -> String {
        if self.enabled {
            format!("\x1b[38;2;208;208;213m{s}\x1b[0m")
        } else {
            s.to_string()
        }
    }

    /// Silver gray (`#C8C8CE`).
    pub fn green(&self, s: &str) -> String {
        if self.enabled {
            format!("\x1b[38;2;200;200;206m{s}\x1b[0m")
        } else {
            s.to_string()
        }
    }

    /// Mid-slate gray (`#A1A1AA`) for old/removed values.
    pub fn red(&self, s: &str) -> String {
        if self.enabled {
            format!("\x1b[38;2;161;161;170m{s}\x1b[0m")
        } else {
            s.to_string()
        }
    }

    /// Warm neutral gray (`#B8B8C0`).
    pub fn yellow(&self, s: &str) -> String {
        if self.enabled {
            format!("\x1b[38;2;184;184;192m{s}\x1b[0m")
        } else {
            s.to_string()
        }
    }

    /// Neutral gray (`#B0B0B8`).
    pub fn magenta(&self, s: &str) -> String {
        if self.enabled {
            format!("\x1b[38;2;176;176;184m{s}\x1b[0m")
        } else {
            s.to_string()
        }
    }

    pub fn gray(&self, luma: u8, s: &str) -> String {
        if self.enabled {
            format!("\x1b[38;2;{luma};{luma};{luma}m{s}\x1b[0m")
        } else {
            s.to_string()
        }
    }

    pub fn gray_bold(&self, luma: u8, s: &str) -> String {
        if self.enabled {
            format!("\x1b[1;38;2;{luma};{luma};{luma}m{s}\x1b[0m")
        } else {
            s.to_string()
        }
    }

    /// Render smooth horizontal grayscale luminance gradient across `text`.
    pub fn gradient(&self, text: &str, _start: (u8, u8, u8), _end: (u8, u8, u8)) -> String {
        self.gray_gradient(text, 245, 165)
    }

    pub fn gray_gradient(&self, text: &str, start_luma: u8, end_luma: u8) -> String {
        if !self.enabled {
            return text.to_string();
        }
        let chars: Vec<char> = text.chars().collect();
        if chars.is_empty() {
            return String::new();
        }
        let len = (chars.len().saturating_sub(1)).max(1) as f64;
        let mut out = String::with_capacity(text.len() * 20);
        out.push_str("\x1b[1m");
        for (i, ch) in chars.iter().enumerate() {
            let t = (i as f64) / len;
            let l = (start_luma as f64 + (end_luma as f64 - start_luma as f64) * t).round() as u8;
            out.push_str(&format!("\x1b[38;2;{l};{l};{l}m{ch}"));
        }
        out.push_str("\x1b[0m");
        out
    }

    /// Signature monochrome gradient (Pure White `#FFFFFF` -> Silver `#D4D4D8` -> Slate `#9CA3AF`).
    pub fn brand_gradient(&self, text: &str) -> String {
        self.gray_gradient(text, 255, 175)
    }

    /// Grayscale pill badge with foreground and background luminance.
    pub fn pill(&self, label: &str, fg_luma: u8, bg_luma: u8) -> String {
        if self.enabled {
            format!(
                "\x1b[1;38;2;{fg_luma};{fg_luma};{fg_luma};48;2;{bg_luma};{bg_luma};{bg_luma}m {label} \x1b[0m"
            )
        } else {
            label.to_string()
        }
    }

    /// High-contrast light-on-charcoal pill (`#F4F4F5` on `#27272A`).
    pub fn badge_ok(&self, label: &str) -> String {
        self.pill(label, 244, 42)
    }

    /// Inverted stark black-on-white pill (`#09090B` on `#FAFAFA`) for errors/alerts.
    pub fn badge_err(&self, label: &str) -> String {
        self.pill(label, 12, 245)
    }

    /// Medium contrast silver-on-graphite pill (`#E4E4E7` on `#3F3F46`).
    pub fn badge_warn(&self, label: &str) -> String {
        self.pill(label, 228, 63)
    }

    /// Subtle slate pill (`#D4D4D8` on `#27272A`).
    pub fn badge_info(&self, label: &str) -> String {
        self.pill(label, 212, 36)
    }

    /// Render a horizontal monochrome progress bar `━━━╸───` with grayscale gradient.
    pub fn progress_bar(&self, current: usize, total: usize, width: usize) -> String {
        let total_safe = total.max(1);
        let ratio = (current as f64 / total_safe as f64).clamp(0.0, 1.0);
        if !self.enabled {
            return format!("[{current}/{total}]");
        }
        let filled = (ratio * width as f64).round() as usize;
        let empty = width.saturating_sub(filled);
        let filled_str = "━".repeat(filled);
        let empty_str = "─".repeat(empty);
        let grad_filled = self.gray_gradient(&filled_str, 250, 170);
        let dim_empty = self.gray(82, &empty_str);
        let pct = (ratio * 100.0).round() as usize;
        format!("{grad_filled}{dim_empty} {:>3}%", self.bold(&pct.to_string()))
    }

    /// Render a sleek monochrome header banner when styled, or simple title when uncolored.
    pub fn banner(&self, title: &str, subtitle: &str) -> String {
        if !self.enabled {
            if subtitle.is_empty() {
                return title.to_string();
            }
            return format!("{title}  ({subtitle})");
        }
        let mut out = String::new();
        let top_rule = self.gray_gradient(&"─".repeat(68), 210, 85);
        out.push_str(&format!(
            "{} {} {}",
            self.gray_bold(245, "▪"),
            self.brand_gradient(title),
            if subtitle.is_empty() {
                String::new()
            } else {
                self.dim(&format!("· {subtitle}"))
            }
        ));
        out.push('\n');
        out.push_str(&top_rule);
        out
    }

    /// Play a brief animated stage sequence on stderr when `self.animate` is true.
    pub fn animate_stage<F, R>(&self, step_idx: usize, total_steps: usize, label: &str, f: F) -> R
    where
        F: FnOnce() -> R,
    {
        if !self.animate {
            return f();
        }
        let step_tag = format!("\x1b[1;38;2;160;160;168m[{step_idx}/{total_steps}]\x1b[0m");
        let spinner = LiveSpinner::start(
            format!("{step_tag} \x1b[38;2;225;225;230m{label}\x1b[0m"),
            Duration::from_millis(28),
        );
        let started = Instant::now();
        let res = f();
        let elapsed = started.elapsed();
        if elapsed < Duration::from_millis(75) {
            thread::sleep(Duration::from_millis(75) - elapsed);
        }
        let total_ms = started.elapsed().as_millis();
        spinner.stop_with_line(&format!(
            "  \x1b[1;38;2;245;245;245m✓\x1b[0m {step_tag} \x1b[38;2;138;138;142m{label} ({total_ms}ms)\x1b[0m"
        ));
        res
    }

    /// Play a multi-item monochrome sweep animation on stderr when `self.animate` is true.
    pub fn animate_sweep(&self, prefix: &str, items: &[String], duration_per_item_ms: u64) {
        if !self.animate || items.is_empty() {
            return;
        }
        let mut err = std::io::stderr();
        let _ = write!(err, "\x1b[?25l");
        let total = items.len();
        for (idx, item) in items.iter().enumerate() {
            let frame = SPINNER_FRAMES[idx % SPINNER_FRAMES.len()];
            let bar = self.progress_bar(idx + 1, total, 20);
            let _ = write!(
                err,
                "\r\x1b[2K  {} {} {} {}",
                self.gray_bold(240, frame),
                self.bold(prefix),
                bar,
                self.dim(item)
            );
            let _ = err.flush();
            thread::sleep(Duration::from_millis(duration_per_item_ms));
        }
        let _ = write!(err, "\r\x1b[2K\x1b[?25h");
        let _ = err.flush();
    }

    /// Style a human diagnostic string in monochrome/grayscale when styling is enabled, while preserving
    /// byte-for-byte plain output when styling is disabled.
    pub fn style_diagnostic(&self, human_text: &str) -> String {
        if !self.enabled {
            return human_text.to_string();
        }
        let mut out = String::new();
        for line in human_text.lines() {
            if let Some(rest) = line.strip_prefix("error[") {
                if let Some((code, msg)) = rest.split_once("]: ") {
                    out.push_str(&self.badge_err(&format!("ERROR {code}")));
                    out.push(' ');
                    out.push_str(&self.bold(msg));
                } else {
                    out.push_str(&self.bold(line));
                }
            } else if let Some(rest) = line.strip_prefix("warning[") {
                if let Some((code, msg)) = rest.split_once("]: ") {
                    out.push_str(&self.badge_warn(&format!("WARN {code}")));
                    out.push(' ');
                    out.push_str(&self.yellow_bold(msg));
                } else {
                    out.push_str(&self.yellow_bold(line));
                }
            } else if line.trim_start().starts_with("--> ") {
                let indent = &line[..line.len() - line.trim_start().len()];
                let rest = &line.trim_start()[4..];
                out.push_str(indent);
                out.push_str(&self.gray_bold(170, "╭─▸ "));
                out.push_str(&self.bold(rest));
            } else if line.contains(" | ") && line.trim_start().starts_with('^') {
                out.push_str(&self.bold(line));
            } else if let Some((left, right)) = line.split_once(" | ") {
                if left.trim().chars().all(|c| c.is_ascii_digit()) {
                    out.push_str(&self.gray(145, &format!("{left} │ ")));
                    if right.trim_start().starts_with('^') {
                        out.push_str(&self.bold(right));
                    } else {
                        out.push_str(&self.gray(215, right));
                    }
                } else {
                    out.push_str(line);
                }
            } else if line.trim_end() == "|" {
                let indent = &line[..line.len() - 1];
                out.push_str(indent);
                out.push_str(&self.gray(145, "│"));
            } else if line.trim_start().starts_with("= help:") {
                let indent = &line[..line.len() - line.trim_start().len()];
                let rest = &line.trim_start()[7..];
                out.push_str(indent);
                out.push_str(&self.gray_bold(220, "╰─▸ help:"));
                out.push_str(&self.bold(rest));
            } else if line.trim_start().starts_with("= ") {
                let indent = &line[..line.len() - line.trim_start().len()];
                let rest = &line.trim_start()[2..];
                out.push_str(indent);
                out.push_str(&self.gray(150, "├─• "));
                out.push_str(&self.dim(rest));
            } else {
                out.push_str(line);
            }
            out.push('\n');
        }
        out
    }
}

pub fn style_diagnostic(human_text: &str, styler: &Styler) -> String {
    styler.style_diagnostic(human_text)
}

/// Background thread spinner that renders smooth monochrome braille + luminance pulse frames on stderr.
pub struct LiveSpinner {
    running: Arc<AtomicBool>,
    message: Arc<Mutex<String>>,
    handle: Option<JoinHandle<()>>,
}

impl LiveSpinner {
    pub fn start(initial_message: impl Into<String>, interval: Duration) -> Self {
        let running = Arc::new(AtomicBool::new(true));
        let message = Arc::new(Mutex::new(initial_message.into()));
        let r_clone = Arc::clone(&running);
        let m_clone = Arc::clone(&message);

        let handle = thread::spawn(move || {
            let mut err = std::io::stderr();
            let _ = write!(err, "\x1b[?25l");
            let _ = err.flush();
            let start = Instant::now();
            let mut tick = 0usize;

            while r_clone.load(Ordering::Relaxed) {
                let frame = SPINNER_FRAMES[tick % SPINNER_FRAMES.len()];
                let dot = PULSE_DOTS[(tick / 3) % PULSE_DOTS.len()];
                let elapsed_ms = start.elapsed().as_millis();
                let msg = m_clone.lock().map(|g| g.clone()).unwrap_or_default();
                // Oscillate grayscale luma between 175 and 255
                let wave = ((tick % 10) as i32 - 5).unsigned_abs() as u8;
                let luma = 180 + wave * 15;
                let _ = write!(
                    err,
                    "\r\x1b[2K  \x1b[1;38;2;{luma};{luma};{luma}m{frame}\x1b[0m \x1b[38;2;140;140;145m{dot}\x1b[0m {msg} \x1b[38;2;120;120;126m({elapsed_ms}ms)\x1b[0m"
                );
                let _ = err.flush();
                tick += 1;
                thread::sleep(interval);
            }
        });

        Self {
            running,
            message,
            handle: Some(handle),
        }
    }

    pub fn set_message(&self, new_msg: impl Into<String>) {
        if let Ok(mut guard) = self.message.lock() {
            *guard = new_msg.into();
        }
    }

    pub fn stop_with_line(mut self, final_line: &str) {
        self.running.store(false, Ordering::Relaxed);
        if let Some(h) = self.handle.take() {
            let _ = h.join();
        }
        let mut err = std::io::stderr();
        let _ = writeln!(err, "\r\x1b[2K{final_line}\x1b[?25h");
        let _ = err.flush();
    }

    pub fn stop_and_clear(mut self) {
        self.running.store(false, Ordering::Relaxed);
        if let Some(h) = self.handle.take() {
            let _ = h.join();
        }
        let mut err = std::io::stderr();
        let _ = write!(err, "\r\x1b[2K\x1b[?25h");
        let _ = err.flush();
    }
}

impl Drop for LiveSpinner {
    fn drop(&mut self) {
        if self.running.swap(false, Ordering::Relaxed) {
            if let Some(h) = self.handle.take() {
                let _ = h.join();
            }
            let mut err = std::io::stderr();
            let _ = write!(err, "\r\x1b[2K\x1b[?25h");
            let _ = err.flush();
        }
    }
}
