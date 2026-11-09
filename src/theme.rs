//! Terminal colors and the shapes of korin's run output.
//!
//! The palette is nightfox.nvim's carbonfox, and each color takes the nvim role that does
//! the same job, as in velocipede's theme. Carbonfox has no warm yellow, so warnings use
//! magenta, as nvim does.
//!
//! Color is added last, to lines that are already redacted. An escape code inserted earlier
//! could split a secret so the redactor no longer matches it. The journal, the report and
//! any output that is not a terminal stay plain.

use std::fmt::Write as _;
use std::io::IsTerminal as _;
use std::sync::OnceLock;

use regex::Regex;

use crate::render::{VALUE_CLOSE, VALUE_OPEN, strip_marks};

/// A 24-bit color.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Rgb(pub u8, pub u8, pub u8);

/// Carbonfox colors, named by the role they play in nvim.
pub mod palette {
    use super::Rgb;

    /// fg1: main text, titles.
    pub const FG: Rgb = Rgb(0xf2, 0xf4, 0xf8);
    /// fg3: dim text: step numbers, the journal path, waiting lines.
    pub const FG_DIM: Rgb = Rgb(0x7b, 0x7c, 0x7e);
    /// bg4: frames and the output gutter.
    pub const BORDER: Rgb = Rgb(0x53, 0x53, 0x53);
    /// blue: the prompt, stage markers and keywords.
    pub const BLUE: Rgb = Rgb(0x78, 0xa9, 0xff);
    /// cyan: values korin put into a command, and placeholders for values still to come.
    pub const CYAN: Rgb = Rgb(0x33, 0xb1, 0xff);
    /// teal (git.changed): captures and expects.
    pub const TEAL: Rgb = Rgb(0x08, 0xbd, 0xba);
    /// green: success and strings.
    pub const GREEN: Rgb = Rgb(0x25, 0xbe, 0x6a);
    /// magenta (diag.warn): warnings and questions.
    pub const MAGENTA: Rgb = Rgb(0xbe, 0x95, 0xff);
    /// red (diag.error): errors and masked secrets.
    pub const RED: Rgb = Rgb(0xee, 0x53, 0x96);
    /// comment: steps still to run, shown as ghost lines.
    pub const COMMENT: Rgb = Rgb(0x6e, 0x6f, 0x70);
}

/// Whether to color output: `--color`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum Choice {
    /// Color a terminal, unless `NO_COLOR` is set or `TERM` is `dumb`.
    #[default]
    Auto,
    Always,
    Never,
}

/// How many colors the terminal shows.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Depth {
    Plain,
    Ansi256,
    TrueColor,
}

/// What a piece of text is, which decides its color.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Role {
    Frame,
    Brand,
    Title,
    Path,
    Dim,
    Keyword,
    Prompt,
    Value,
    Secret,
    Str,
    Ok,
    Warn,
    Error,
    Label,
    Ghost,
}

impl Role {
    fn style(self) -> (Rgb, bool) {
        use palette::{BLUE, BORDER, COMMENT, CYAN, FG, FG_DIM, GREEN, MAGENTA, RED, TEAL};
        match self {
            Role::Frame => (BORDER, false),
            Role::Brand | Role::Prompt => (BLUE, true),
            Role::Title => (FG, true),
            Role::Path | Role::Value => (CYAN, false),
            Role::Dim => (FG_DIM, false),
            Role::Keyword => (BLUE, false),
            Role::Secret => (RED, false),
            Role::Str => (GREEN, false),
            Role::Ok => (GREEN, true),
            Role::Warn => (MAGENTA, false),
            Role::Error => (RED, true),
            Role::Label => (TEAL, false),
            Role::Ghost => (COMMENT, false),
        }
    }
}

/// The colors for one output stream.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Theme {
    depth: Depth,
}

impl Theme {
    /// No color at all.
    pub const PLAIN: Theme = Theme {
        depth: Depth::Plain,
    };

    /// The theme for stdout.
    pub fn stdout(choice: Choice) -> Self {
        Self::detect(choice, std::io::stdout().is_terminal())
    }

    /// The theme for stderr, where diagnostics go.
    pub fn stderr(choice: Choice) -> Self {
        Self::detect(choice, std::io::stderr().is_terminal())
    }

    /// Decides from `--color`, whether the stream is a terminal, and the environment:
    /// `NO_COLOR` turns color off, `CLICOLOR_FORCE` turns it on, and `COLORTERM` tells
    /// whether the terminal shows 24-bit color.
    pub fn detect(choice: Choice, terminal: bool) -> Self {
        let set = |name: &str| std::env::var_os(name).is_some_and(|v| !v.is_empty());
        let on = match choice {
            Choice::Always => true,
            Choice::Never => false,
            Choice::Auto if set("NO_COLOR") => false,
            Choice::Auto
                if std::env::var_os("CLICOLOR_FORCE")
                    .is_some_and(|v| v != "0" && !v.is_empty()) =>
            {
                true
            }
            Choice::Auto => terminal && std::env::var_os("TERM").is_none_or(|t| t != "dumb"),
        };
        if !on {
            return Self::PLAIN;
        }
        let truecolor =
            std::env::var("COLORTERM").is_ok_and(|v| matches!(v.as_str(), "truecolor" | "24bit"));
        Theme {
            depth: if truecolor {
                Depth::TrueColor
            } else {
                Depth::Ansi256
            },
        }
    }

    pub fn enabled(self) -> bool {
        self.depth != Depth::Plain
    }

    fn paint(self, role: Role, text: &str, out: &mut String) {
        if text.is_empty() {
            return;
        }
        let (rgb, bold) = role.style();
        match self.depth {
            Depth::Plain => out.push_str(text),
            Depth::Ansi256 => {
                let _ = write!(
                    out,
                    "\x1b[{}38;5;{}m{text}\x1b[0m",
                    if bold { "1;" } else { "" },
                    to_256(rgb)
                );
            }
            Depth::TrueColor => {
                let Rgb(r, g, b) = rgb;
                let _ = write!(
                    out,
                    "\x1b[{}38;2;{r};{g};{b}m{text}\x1b[0m",
                    if bold { "1;" } else { "" }
                );
            }
        }
    }

    /// Colors one or more lines of run or plan output. The text must already be redacted.
    /// Value marks from [`crate::render::argv`] become color, and are removed either way.
    pub fn lines(self, text: &str) -> String {
        if !self.enabled() {
            return strip_marks(text);
        }
        let mut out = String::with_capacity(text.len() * 2);
        for (i, line) in text.split('\n').enumerate() {
            if i > 0 {
                out.push('\n');
            }
            self.line(line, &mut out);
        }
        out
    }

    /// A step still to run, dimmed as one piece. The line must already be redacted.
    pub fn ghost(self, line: &str) -> String {
        let mut out = String::new();
        self.paint(Role::Ghost, &strip_marks(line), &mut out);
        out
    }

    /// The prefix for each line of a command's output.
    pub fn gutter(self) -> String {
        let mut out = String::from("      ");
        self.paint(Role::Frame, "│", &mut out);
        out.push(' ');
        out
    }

    /// A yes/no question, as at a stage gate or a `confirm`.
    pub fn question(self, question: &str) -> String {
        let mut out = String::from("  ");
        self.paint(Role::Warn, "?", &mut out);
        out.push(' ');
        self.paint(Role::Title, question, &mut out);
        out.push(' ');
        self.paint(Role::Dim, "[y/N]", &mut out);
        out.push(' ');
        out
    }

    /// A prompt that asks for a value, such as an `ask` answer or a param.
    pub fn ask(self, label: &str) -> String {
        let mut out = String::from("  ");
        self.paint(Role::Warn, "?", &mut out);
        out.push(' ');
        self.paint(Role::Title, label, &mut out);
        out.push(' ');
        out
    }

    /// The theme for miette's diagnostics.
    pub fn diagnostics(self) -> miette::GraphicalTheme {
        use miette::{GraphicalTheme, ThemeCharacters, ThemeStyles};
        if !self.enabled() {
            return GraphicalTheme::unicode_nocolor();
        }
        let style = |rgb: Rgb| {
            let base = owo_colors::Style::new();
            match self.depth {
                Depth::TrueColor => base.truecolor(rgb.0, rgb.1, rgb.2),
                _ => base.color(owo_colors::XtermColors::from(to_256(rgb))),
            }
        };
        GraphicalTheme {
            characters: ThemeCharacters::unicode(),
            styles: ThemeStyles {
                error: style(palette::RED).bold(),
                warning: style(palette::MAGENTA).bold(),
                advice: style(palette::TEAL),
                help: style(palette::FG_DIM),
                link: style(palette::BLUE).underline().bold(),
                linum: style(palette::FG_DIM),
                highlights: vec![
                    style(palette::RED).bold(),
                    style(palette::MAGENTA).bold(),
                    style(palette::CYAN).bold(),
                ],
            },
        }
    }

    // ---- the line highlighter ----

    fn line(self, line: &str, out: &mut String) {
        let p = patterns();
        if let Some(rest) = line.strip_prefix("╭─ korin ─ ") {
            self.intro_top(rest, out);
        } else if let Some(rest) = line.strip_prefix("╰─❯ ") {
            self.paint(Role::Frame, "╰─", out);
            self.paint(Role::Prompt, "❯", out);
            out.push(' ');
            self.inline(rest, None, false, out);
        } else if let Some(c) = p.stage.captures(line) {
            out.push_str(&c[1]);
            self.paint(Role::Prompt, "▸", out);
            out.push(' ');
            self.paint(Role::Dim, &c[2], out);
            self.paint(Role::Frame, " · ", out);
            self.paint(Role::Title, &c[3], out);
            if let Some(tags) = c.get(4) {
                self.paint(Role::Dim, tags.as_str(), out);
            }
        } else if let Some(c) = p.ran.captures(line) {
            out.push_str(&c[1]);
            self.paint(Role::Dim, &c[2], out);
            out.push(' ');
            self.paint(Role::Prompt, "❯", out);
            out.push(' ');
            self.inline(&c[3], None, false, out);
        } else if let Some(c) = p.planned_command.captures(line) {
            out.push_str(&c[1]);
            self.paint(Role::Dim, &c[2], out);
            out.push_str("  ");
            self.paint(Role::Keyword, &c[3], out);
            out.push(' ');
            self.paint(Role::Prompt, "❯", out);
            out.push(' ');
            self.inline(&c[4], None, false, out);
        } else if let Some(c) = p.planned_step.captures(line) {
            out.push_str(&c[1]);
            self.paint(Role::Dim, &c[2], out);
            out.push_str("  ");
            self.paint(Role::Keyword, &c[3], out);
            self.inline(&c[4], None, true, out);
        } else if let Some(c) = p.call.captures(line) {
            out.push_str(&c[1]);
            self.paint(Role::Dim, &c[2], out);
            out.push_str("  ");
            self.inline(&c[3], None, true, out);
        } else if let Some(c) = p.detail.captures(line) {
            out.push_str(&c[1]);
            match &c[2] {
                "→" => {
                    self.paint(Role::Frame, "→", out);
                    self.paint(Role::Label, &c[3], out);
                }
                "?" | "expect:" => {
                    self.paint(Role::Dim, &c[2], out);
                    self.paint(Role::Label, &c[3], out);
                }
                keyword => {
                    self.paint(Role::Keyword, keyword, out);
                    self.inline(&c[3], None, true, out);
                }
            }
        } else if let Some(c) = p.status.captures(line) {
            out.push_str(&c[1]);
            let (mark_role, text_role) = match &c[2] {
                "✓" => (Role::Ok, Some(Role::Title)),
                "✗" => (Role::Error, Some(Role::Title)),
                "■" => (Role::Warn, Some(Role::Title)),
                _ => (Role::Warn, Some(Role::Warn)),
            };
            self.paint(mark_role, &c[2], out);
            out.push(' ');
            self.inline(&c[3], text_role, true, out);
        } else if let Some(c) = p.quiet.captures(line) {
            out.push_str(&c[1]);
            self.paint(Role::Dim, &c[2], out);
            self.inline(&c[3], Some(Role::Dim), false, out);
        } else {
            self.inline(line, None, true, out);
        }
    }

    /// `╭─ korin ─ <title> ─ <cwd> ─ <n> stages`. The title may itself hold ` ─ `, so the
    /// last two fields are split off from the right.
    fn intro_top(self, rest: &str, out: &mut String) {
        self.paint(Role::Frame, "╭─ ", out);
        self.paint(Role::Brand, "korin", out);
        let mut fields = rest.rsplitn(3, " ─ ");
        let (count, cwd, title) = (fields.next(), fields.next(), fields.next());
        if let (Some(count), Some(cwd), Some(title)) = (count, cwd, title) {
            self.paint(Role::Frame, " ─ ", out);
            self.paint(Role::Title, &strip_marks(title), out);
            self.paint(Role::Frame, " ─ ", out);
            self.paint(Role::Path, cwd, out);
            self.paint(Role::Frame, " ─ ", out);
            self.paint(Role::Dim, count, out);
        } else {
            self.paint(Role::Frame, " ─ ", out);
            self.inline(rest, None, false, out);
        }
    }

    /// Colors the values, masked secrets, placeholders and (when `strings` is set) quoted
    /// strings inside a piece of text. Other text takes `base`, or no color.
    fn inline(self, text: &str, base: Option<Role>, strings: bool, out: &mut String) {
        let mut last = 0;
        for m in patterns().inline.find_iter(text) {
            self.plain(&text[last..m.start()], base, out);
            let token = m.as_str();
            if let Some(value) = token
                .strip_prefix(VALUE_OPEN)
                .and_then(|t| t.strip_suffix(VALUE_CLOSE))
            {
                self.paint(Role::Value, value, out);
            } else if token.starts_with('«') {
                self.paint(Role::Secret, token, out);
            } else if token.starts_with('<') {
                self.paint(Role::Value, token, out);
            } else if strings {
                // A quoted string: green, with any placeholders inside it still marked.
                self.inline(token, Some(Role::Str), false, out);
            } else {
                self.plain(token, base, out);
            }
            last = m.end();
        }
        self.plain(&text[last..], base, out);
    }

    fn plain(self, text: &str, base: Option<Role>, out: &mut String) {
        let text = strip_marks(text);
        match base {
            Some(role) => self.paint(role, &text, out),
            None => out.push_str(&text),
        }
    }
}

struct Patterns {
    stage: Regex,
    ran: Regex,
    planned_command: Regex,
    planned_step: Regex,
    call: Regex,
    detail: Regex,
    status: Regex,
    quiet: Regex,
    inline: Regex,
}

fn patterns() -> &'static Patterns {
    static PATTERNS: OnceLock<Patterns> = OnceLock::new();
    PATTERNS.get_or_init(|| {
        let re = |s: &str| Regex::new(s).expect("theme pattern compiles");
        Patterns {
            // `▸ Stage 1/2 · Title`, with plan tags such as `  [read-only, gate; …]`.
            stage: re(r"^(\s*)▸ (Stage \d+/\d+) · (.*?)(  \[[^\]]*\])?$"),
            // A command as it runs: `  2.1 ❯ echo hi`.
            ran: re(r"^(\s*)(\d+(?:\.\d+)*) ❯ (.*)$"),
            // A command in a plan or at a gate: `  2.1  read ❯ echo hi`.
            planned_command: re(
                r"^(\s*)(\d+(?:\.\d+)*)  (read|mutate|script read|script mutate|interactive) ❯ (.*)$",
            ),
            // Any other step in a plan or at a gate: `  2.2  when x == "y"`.
            planned_step: re(
                r"^(\s*)(\d+(?:\.\d+)*)  (when|each|poll|note|warn|record|confirm|ask|assert)\b(.*)$",
            ),
            // A template call: `  3  aws.identity() as me`.
            call: re(r"^(\s*)(\d+(?:\.\d+)*)  (.*)$"),
            // Lines under a step: captures, expects, `env`, a poll's `until` and `fail`.
            detail: re(r"^(\s+)(→|\?|expect:|env|until|fail)(\s.*)$"),
            // Outcomes: `✓ done`, `✗ failed`, `■ cancelled`, `⚠ warning`.
            status: re(r"^(\s*)(✓|✗|■|⚠) (.*)$"),
            // Bookkeeping lines that should not draw the eye.
            quiet: re(
                r"^(\s+)(journal:|approved by --yes|… waiting|next:|output withheld:|used at|nothing ran)(.*)$",
            ),
            inline: re(&format!(
                r#"{VALUE_OPEN}[^{VALUE_CLOSE}]*{VALUE_CLOSE}|«[^»]*»|<[A-Za-z_][\w.]*(?: ← [0-9.]+)?>|"(?:[^"\\]|\\.)*""#
            )),
        }
    })
}

/// The nearest color in the xterm 256-color palette: the 6×6×6 cube or the gray ramp.
fn to_256(Rgb(r, g, b): Rgb) -> u8 {
    const STEPS: [u8; 6] = [0, 95, 135, 175, 215, 255];
    let nearest = |c: u8| -> u8 {
        let mut best = 0u8;
        for (i, step) in (0u8..).zip(STEPS) {
            if c.abs_diff(step) < c.abs_diff(STEPS[usize::from(best)]) {
                best = i;
            }
        }
        best
    };
    let (ri, gi, bi) = (nearest(r), nearest(g), nearest(b));
    let cube = (
        STEPS[usize::from(ri)],
        STEPS[usize::from(gi)],
        STEPS[usize::from(bi)],
    );
    let average = (u16::from(r) + u16::from(g) + u16::from(b)) / 3;
    let gray_index = u8::try_from(average.saturating_sub(8) / 10)
        .unwrap_or(23)
        .min(23);
    let gray = 8 + 10 * gray_index;
    let distance = |(x, y, z): (u8, u8, u8)| {
        u32::from(r.abs_diff(x)).pow(2)
            + u32::from(g.abs_diff(y)).pow(2)
            + u32::from(b.abs_diff(z)).pow(2)
    };
    if distance(cube) <= distance((gray, gray, gray)) {
        16 + 36 * ri + 6 * gi + bi
    } else {
        232 + gray_index
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const TRUE: Theme = Theme {
        depth: Depth::TrueColor,
    };

    fn strip_ansi(text: &str) -> String {
        Regex::new("\x1b\\[[0-9;]*m")
            .unwrap()
            .replace_all(text, "")
            .into_owned()
    }

    #[test]
    fn color_never_changes_the_visible_text() {
        let lines = [
            "╭─ korin ─ Say ─ hello ─ ~/demo ─ 2 stages",
            "╰─❯ korin run w.korin",
            "▸ Stage 1/2 · Greet  [read-only, gate; valid --from start]",
            "  1.1  read ❯ echo 'hi' <message ← 2.1> «secret:pw»",
            "  1.2  note \"greeted {x}\"",
            "  2.1 ❯ echo \u{e000}'hi susan'\u{e001}",
            "        → message, items [list]",
            "    expect: gone",
            "✓ done",
            "  journal: /tmp/x",
            "  ⚠ careful",
            "  3  aws.identity() as me",
        ];
        for line in lines {
            assert_eq!(strip_ansi(&TRUE.lines(line)), strip_marks(line), "{line}");
            assert_eq!(Theme::PLAIN.lines(line), strip_marks(line), "{line}");
        }
    }

    #[test]
    fn substituted_values_and_secrets_are_colored() {
        let shown = TRUE.lines("  2.1 ❯ echo \u{e000}susan\u{e001} «secret:pw»");
        assert!(
            shown.contains("\x1b[38;2;51;177;255msusan\x1b[0m"),
            "{shown:?}"
        );
        assert!(
            shown.contains("\x1b[38;2;238;83;150m«secret:pw»\x1b[0m"),
            "{shown:?}"
        );
    }

    #[test]
    fn the_256_color_fallback_picks_close_colors() {
        assert_eq!(to_256(Rgb(0, 0, 0)), 16);
        assert_eq!(to_256(Rgb(255, 255, 255)), 231);
        assert_eq!(to_256(Rgb(0x7b, 0x7c, 0x7e)), 243);
    }
}
