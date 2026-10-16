//! TrueColor/ANSI terminal styling, gradients, pill badges, box-drawing widgets,
//! and multi-stage terminal animations (`LiveSpinner`, `AnimatedProgressBar`, `PipelineAnimator`).
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
const SPARK_FRAMES: &[&str] = &["⚡", "✦", "✧", "✦"];

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

    pub fn bold(&self, s: &str) -> String {
        if self.enabled {
            format!("\x1b[1m{s}\x1b[0m")
        } else {
            s.to_string()
        }
    }

    pub fn dim(&self, s: &str) -> String {
        if self.enabled {
            format!("\x1b[2m{s}\x1b[0m")
        } else {
            s.to_string()
        }
    }

    pub fn italic(&self, s: &str) -> String {
        if self.enabled {
            format!("\x1b[3m{s}\x1b[0m")
        } else {
            s.to_string()
        }
    }

    pub fn red_bold(&self, s: &str) -> String {
        if self.enabled {
            format!("\x1b[1;38;2;255;85;114m{s}\x1b[0m")
        } else {
            s.to_string()
        }
    }

    pub fn green_bold(&self, s: &str) -> String {
        if self.enabled {
            format!("\x1b[1;38;2;16;235;156m{s}\x1b[0m")
        } else {
            s.to_string()
        }
    }

    pub fn yellow_bold(&self, s: &str) -> String {
        if self.enabled {
            format!("\x1b[1;38;2;255;209;102m{s}\x1b[0m")
        } else {
            s.to_string()
        }
    }

    pub fn magenta_bold(&self, s: &str) -> String {
        if self.enabled {
            format!("\x1b[1;38;2;199;125;255m{s}\x1b[0m")
        } else {
            s.to_string()
        }
    }

    pub fn cyan_bold(&self, s: &str) -> String {
        if self.enabled {
            format!("\x1b[1;38;2;0;245;212m{s}\x1b[0m")
        } else {
            s.to_string()
        }
    }

    pub fn blue_bold(&self, s: &str) -> String {
        if self.enabled {
            format!("\x1b[1;38;2;56;189;248m{s}\x1b[0m")
        } else {
            s.to_string()
        }
    }

    pub fn cyan(&self, s: &str) -> String {
        if self.enabled {
            format!("\x1b[38;2;56;219;255m{s}\x1b[0m")
        } else {
            s.to_string()
        }
    }

    pub fn green(&self, s: &str) -> String {
        if self.enabled {
            format!("\x1b[38;2;52;211;153m{s}\x1b[0m")
        } else {
            s.to_string()
        }
    }

    pub fn red(&self, s: &str) -> String {
        if self.enabled {
            format!("\x1b[38;2;251;113;133m{s}\x1b[0m")
        } else {
            s.to_string()
        }
    }

    pub fn yellow(&self, s: &str) -> String {
        if self.enabled {
            format!("\x1b[38;2;251;191;36m{s}\x1b[0m")
        } else {
            s.to_string()
        }
    }

    pub fn magenta(&self, s: &str) -> String {
        if self.enabled {
            format!("\x1b[38;2;216;180;254m{s}\x1b[0m")
        } else {
            s.to_string()
        }
    }

    pub fn rgb(&self, r: u8, g: u8, b: u8, s: &str) -> String {
        if self.enabled {
            format!("\x1b[38;2;{r};{g};{b}m{s}\x1b[0m")
        } else {
            s.to_string()
        }
    }

    pub fn rgb_bold(&self, r: u8, g: u8, b: u8, s: &str) -> String {
        if self.enabled {
            format!("\x1b[1;38;2;{r};{g};{b}m{s}\x1b[0m")
        } else {
            s.to_string()
        }
    }

    /// Render smooth horizontal TrueColor RGB gradient across `text`.
    pub fn gradient(&self, text: &str, start: (u8, u8, u8), end: (u8, u8, u8)) -> String {
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
            let r = (start.0 as f64 + (end.0 as f64 - start.0 as f64) * t).round() as u8;
            let g = (start.1 as f64 + (end.1 as f64 - start.1 as f64) * t).round() as u8;
            let b = (start.2 as f64 + (end.2 as f64 - start.2 as f64) * t).round() as u8;
            out.push_str(&format!("\x1b[38;2;{r};{g};{b}m{ch}"));
        }
        out.push_str("\x1b[0m");
        out
    }

    /// Signature Claim Wire brand gradient (Electric Cyan -> Neon Violet -> Coral Pink).
    pub fn brand_gradient(&self, text: &str) -> String {
        if !self.enabled {
            return text.to_string();
        }
        let chars: Vec<char> = text.chars().collect();
        if chars.is_empty() {
            return String::new();
        }
        let len = (chars.len().saturating_sub(1)).max(1) as f64;
        let c1 = (0u8, 245u8, 212u8); // #00F5D4 Electric Cyan
        let c2 = (155u8, 93u8, 229u8); // #9B5DE5 Neon Violet
        let c3 = (241u8, 91u8, 181u8); // #F15BB5 Coral Pink
        let mut out = String::with_capacity(text.len() * 20);
        out.push_str("\x1b[1m");
        for (i, ch) in chars.iter().enumerate() {
            let t = (i as f64) / len;
            let (r, g, b) = if t < 0.5 {
                let u = t * 2.0;
                (
                    (c1.0 as f64 + (c2.0 as f64 - c1.0 as f64) * u).round() as u8,
                    (c1.1 as f64 + (c2.1 as f64 - c1.1 as f64) * u).round() as u8,
                    (c1.2 as f64 + (c2.2 as f64 - c1.2 as f64) * u).round() as u8,
                )
            } else {
                let u = (t - 0.5) * 2.0;
                (
                    (c2.0 as f64 + (c3.0 as f64 - c2.0 as f64) * u).round() as u8,
                    (c2.1 as f64 + (c3.1 as f64 - c2.1 as f64) * u).round() as u8,
                    (c2.2 as f64 + (c3.2 as f64 - c2.2 as f64) * u).round() as u8,
                )
            };
            out.push_str(&format!("\x1b[38;2;{r};{g};{b}m{ch}"));
        }
        out.push_str("\x1b[0m");
        out
    }

    /// Pill badge with background color when styling is enabled, or plain text when disabled.
    pub fn pill(&self, label: &str, fg: (u8, u8, u8), bg: (u8, u8, u8)) -> String {
        if self.enabled {
            format!(
                "\x1b[1;38;2;{};{};{};48;2;{};{};{}m {label} \x1b[0m",
                fg.0, fg.1, fg.2, bg.0, bg.1, bg.2
            )
        } else {
            label.to_string()
        }
    }

    pub fn badge_ok(&self, label: &str) -> String {
        self.pill(label, (10, 25, 20), (16, 235, 156))
    }

    pub fn badge_err(&self, label: &str) -> String {
        self.pill(label, (255, 245, 247), (225, 29, 72))
    }

    pub fn badge_warn(&self, label: &str) -> String {
        self.pill(label, (30, 20, 5), (251, 191, 36))
    }

    pub fn badge_info(&self, label: &str) -> String {
        self.pill(label, (10, 20, 35), (56, 189, 248))
    }

    /// Render a horizontal progress bar `━━━╸───` with percentage.
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
        let grad_filled = self.gradient(&filled_str, (0, 245, 212), (155, 93, 229));
        let dim_empty = self.dim(&empty_str);
        let pct = (ratio * 100.0).round() as usize;
        format!("{grad_filled}{dim_empty} {:>3}%", self.cyan_bold(&pct.to_string()))
    }

    /// Render a decorative header banner when colored, or simple title when uncolored.
    pub fn banner(&self, title: &str, subtitle: &str) -> String {
        if !self.enabled {
            if subtitle.is_empty() {
                return title.to_string();
            }
            return format!("{title}  ({subtitle})");
        }
        let mut out = String::new();
        let top_rule = self.gradient(&"━".repeat(68), (0, 245, 212), (241, 91, 181));
        out.push_str(&format!(
            "{} {} {}",
            self.cyan_bold("⚡"),
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
        let step_tag = format!("\x1b[1;38;2;155;93;229m[{step_idx}/{total_steps}]\x1b[0m");
        let spinner = LiveSpinner::start(
            format!("{step_tag} \x1b[1m{label}\x1b[0m"),
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
            "  \x1b[1;38;2;16;235;156m✓\x1b[0m {step_tag} \x1b[2m{label} ({total_ms}ms)\x1b[0m"
        ));
        res
    }

    /// Play a multi-item sweep animation (e.g. scanning files or patching spans) when `self.animate` is true.
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
                self.cyan_bold(frame),
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

    /// Colorize a human diagnostic string when styling is enabled, while preserving
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
                    out.push_str(&self.red_bold(msg));
                } else {
                    out.push_str(&self.red_bold(line));
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
                out.push_str(&self.cyan_bold("╭─▸ "));
                out.push_str(&self.bold(rest));
            } else if line.contains(" | ") && line.trim_start().starts_with('^') {
                out.push_str(&self.red_bold(line));
            } else if let Some((left, right)) = line.split_once(" | ") {
                if left.trim().chars().all(|c| c.is_ascii_digit()) {
                    out.push_str(&self.cyan_bold(&format!("{left} │ ")));
                    if right.trim_start().starts_with('^') {
                        out.push_str(&self.red_bold(right));
                    } else {
                        out.push_str(right);
                    }
                } else {
                    out.push_str(line);
                }
            } else if line.trim_end() == "|" {
                let indent = &line[..line.len() - 1];
                out.push_str(indent);
                out.push_str(&self.cyan_bold("│"));
            } else if line.trim_start().starts_with("= help:") {
                let indent = &line[..line.len() - line.trim_start().len()];
                let rest = &line.trim_start()[7..];
                out.push_str(indent);
                out.push_str(&self.green_bold("╰─▸ help:"));
                out.push_str(&self.bold(rest));
            } else if line.trim_start().starts_with("= ") {
                let indent = &line[..line.len() - line.trim_start().len()];
                let rest = &line.trim_start()[2..];
                out.push_str(indent);
                out.push_str(&self.magenta_bold("├─• "));
                out.push_str(&self.cyan(rest));
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

/// Background thread spinner that renders smooth 60fps/30fps braille + neon pulse frames on stderr.
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
                let spark = SPARK_FRAMES[(tick / 3) % SPARK_FRAMES.len()];
                let elapsed_ms = start.elapsed().as_millis();
                let msg = m_clone.lock().map(|g| g.clone()).unwrap_or_default();
                let pulse_r = 0u8;
                let pulse_g = (195 + ((tick * 12) % 60) as u8).min(255);
                let pulse_b = 212u8;
                let _ = write!(
                    err,
                    "\r\x1b[2K  \x1b[1;38;2;{pulse_r};{pulse_g};{pulse_b}m{frame}\x1b[0m {spark} {msg} \x1b[2m({elapsed_ms}ms)\x1b[0m"
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
