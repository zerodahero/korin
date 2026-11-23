//! The plan pane's layout: which lines it shows, for the height the terminal has.
//!
//! The pane sits under the run log while a stage runs. It shows the last stages that ended,
//! the current stage with every step, and the titles of the stages to come. It takes about a
//! third of the terminal, and on a short terminal only the current step. Every line here is
//! already themed; this module only chooses lines, so it never sees a secret.

use unicode_width::UnicodeWidthChar as _;

use crate::render::{PREFIX_CLOSE, PREFIX_OPEN, VALUE_CLOSE, VALUE_OPEN};

/// Below this many rows the pane shows only the current step and its status lines.
pub const SHORT: usize = 15;
/// Below this many rows there is no pane at all.
pub const TINY: usize = 6;
/// How many ended stages the pane keeps on show.
pub const DONE_SHOWN: usize = 2;

/// Where a stage or step stands, as its pane line marks it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mark {
    /// Running now: `▶`.
    Current,
    /// Ended well: `✓`.
    Done,
    /// Still to come: no mark.
    Pending,
    /// Skipped: `↷`.
    Skipped,
    /// Failed or stopped: `✗`.
    Failed,
    /// A line under a step, such as its captures: no mark.
    None,
}

/// The pane's content before it is fitted to the terminal.
#[derive(Debug, Default, Clone)]
pub struct Plan {
    /// One line for each stage that has ended, oldest first.
    pub done: Vec<String>,
    /// The current stage's title line.
    pub current: Option<String>,
    /// The current stage's step lines.
    pub steps: Vec<String>,
    /// Which step line is running now.
    pub at: Option<usize>,
    /// Lines under the current step, such as a poll's state.
    pub status: Vec<String>,
    /// One line for each stage still to come.
    pub later: Vec<String>,
    /// The legend line for path prefixes, if it shows.
    pub legend: Option<String>,
}

/// Fits a plan into `height` terminal rows. `more` themes a `… N more` line.
pub fn fit(plan: &Plan, height: usize, more: &dyn Fn(&str) -> String) -> Vec<String> {
    if height < TINY {
        return Vec::new();
    }
    if height < SHORT {
        let mut out: Vec<String> = plan
            .at
            .and_then(|at| plan.steps.get(at))
            .cloned()
            .into_iter()
            .collect();
        out.extend(plan.status.iter().cloned());
        return out;
    }
    let budget = (height / 3).max(TINY);
    let done_start = plan.done.len().saturating_sub(DONE_SHOWN);
    let mut done: Vec<String> = plan.done[done_start..].to_vec();
    let mut later = plan.later.clone();
    let mut legend = plan.legend.clone();
    let fixed = usize::from(plan.current.is_some()) + plan.status.len();
    let total = |done: &[String], later: &[String], legend: &Option<String>, steps: usize| {
        done.len() + fixed + steps + later.len() + usize::from(legend.is_some())
    };
    // Give up rows in order: ended stages, the legend, then the stages to come (one line
    // says how many are left out), and last the steps far from the current one.
    if total(&done, &later, &legend, plan.steps.len()) > budget {
        done.clear();
    }
    if total(&done, &later, &legend, plan.steps.len()) > budget {
        legend = None;
    }
    if total(&done, &later, &legend, plan.steps.len()) > budget && later.len() > 1 {
        let hidden = later.len();
        later = vec![more(&format!("  … {hidden} more stages"))];
    }
    let room = budget
        .saturating_sub(total(&done, &later, &legend, 0))
        .max(1);
    let steps = window(&plan.steps, plan.at, room, more);

    let mut out = done;
    out.extend(plan.current.iter().cloned());
    // Status lines go under the step they describe.
    let current = plan.at.and_then(|at| plan.steps.get(at));
    let mut placed = false;
    for line in steps {
        let is_current = !placed && current == Some(&line);
        out.push(line);
        if is_current {
            out.extend(plan.status.iter().cloned());
            placed = true;
        }
    }
    if !placed {
        out.extend(plan.status.iter().cloned());
    }
    out.extend(later);
    out.extend(legend);
    out
}

/// At most `room` step lines, kept around the current one. Each cut end gets a `… N`
/// line, counted in `room`.
fn window(
    steps: &[String],
    at: Option<usize>,
    room: usize,
    more: &dyn Fn(&str) -> String,
) -> Vec<String> {
    if steps.len() <= room {
        return steps.to_vec();
    }
    let at = at.unwrap_or(0).min(steps.len().saturating_sub(1));
    // Room for the steps themselves, after up to two `… N` lines.
    let shown = room.saturating_sub(2).max(1);
    let mut start = at.saturating_sub(shown / 3);
    let end = (start + shown).min(steps.len());
    start = end.saturating_sub(shown);
    let mut out = Vec::new();
    if start > 0 {
        out.push(more(&format!("    … {start} above")));
    }
    out.extend(steps[start..end].iter().cloned());
    if end < steps.len() {
        out.push(more(&format!("    … {} below", steps.len() - end)));
    }
    out
}

/// Cuts a line that may hold value and prefix marks to `width` columns, ending it with `…`.
/// An open mark is closed, so the theme still pairs them. The line must not hold escape
/// sequences yet.
pub fn clip(line: &str, width: usize) -> String {
    let is_mark = |c: char| matches!(c, VALUE_OPEN | VALUE_CLOSE | PREFIX_OPEN | PREFIX_CLOSE);
    let visible: usize = line
        .chars()
        .filter(|c| !is_mark(*c))
        .map(|c| c.width().unwrap_or(0))
        .sum();
    if visible <= width {
        return line.to_string();
    }
    let mut out = String::new();
    let mut used = 0;
    let mut open: Option<char> = None;
    for c in line.chars() {
        if is_mark(c) {
            open = match c {
                VALUE_OPEN => Some(VALUE_CLOSE),
                PREFIX_OPEN => Some(PREFIX_CLOSE),
                _ => None,
            };
            out.push(c);
            continue;
        }
        let w = c.width().unwrap_or(0);
        if used + w + 1 > width {
            break;
        }
        used += w;
        out.push(c);
    }
    if let Some(close) = open {
        out.push(close);
    }
    out.push('…');
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn plan(steps: usize, at: usize) -> Plan {
        Plan {
            done: vec!["done 1".into(), "done 2".into(), "done 3".into()],
            current: Some("stage 4".into()),
            steps: (1..=steps).map(|n| format!("step {n}")).collect(),
            at: Some(at),
            status: vec!["status".into()],
            later: vec!["stage 5".into(), "stage 6".into()],
            legend: Some("legend".into()),
        }
    }

    fn more(text: &str) -> String {
        text.trim().to_string()
    }

    #[test]
    fn a_tall_terminal_shows_every_part_within_a_third() {
        let lines = fit(&plan(4, 1), 40, &more);
        assert_eq!(
            lines,
            [
                "done 2", "done 3", "stage 4", "step 1", "step 2", "status", "step 3", "step 4",
                "stage 5", "stage 6", "legend"
            ]
        );
        assert!(lines.len() <= 40 / 3);
    }

    #[test]
    fn a_long_stage_keeps_the_current_step_in_view() {
        let lines = fit(&plan(30, 20), 40, &more);
        assert!(lines.len() <= 40 / 3, "{lines:?}");
        assert!(lines.contains(&"step 21".to_string()), "{lines:?}");
        assert!(lines.iter().any(|l| l.ends_with("above")), "{lines:?}");
        assert!(lines.iter().any(|l| l.ends_with("below")), "{lines:?}");
        assert!(!lines.contains(&"done 3".to_string()), "{lines:?}");
        assert!(lines.contains(&"… 2 more stages".to_string()), "{lines:?}");
        // The status line stays under the current step.
        let at = lines.iter().position(|l| l == "step 21").unwrap();
        assert_eq!(lines[at + 1], "status");
    }

    #[test]
    fn a_short_terminal_shows_only_the_current_step() {
        assert_eq!(fit(&plan(4, 2), 14, &more), ["step 3", "status"]);
    }

    #[test]
    fn a_tiny_terminal_shows_no_pane() {
        assert_eq!(fit(&plan(4, 2), 5, &more), Vec::<String>::new());
    }

    #[test]
    fn clip_cuts_to_the_width_and_closes_marks() {
        assert_eq!(clip("short", 10), "short");
        assert_eq!(clip("abcdefghij", 5), "abcd…");
        let marked = format!("ab{VALUE_OPEN}cdefgh{VALUE_CLOSE}");
        assert_eq!(clip(&marked, 5), format!("ab{VALUE_OPEN}cd{VALUE_CLOSE}…"));
    }
}
