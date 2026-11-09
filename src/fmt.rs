//! `korin fmt`: canonical layout for `.korin` files.
//!
//! The formatter works line by line, so comments survive. It re-indents each block to two
//! spaces per level, trims trailing whitespace, keeps at most one blank line in a row, puts one
//! blank line before every top-level `stage` and `define`, and aligns the `=` in a run of
//! `capture` lines at the same depth.

/// Formats a file that already parses.
pub fn format(text: &str) -> String {
    let raw: Vec<&str> = text.lines().map(str::trim_end).collect();
    let indents: Vec<Option<usize>> = raw
        .iter()
        .map(|line| {
            let content = line.trim_start();
            (!content.is_empty() && !content.starts_with('#')).then(|| line.len() - content.len())
        })
        .collect();

    // Depth of each code line, from the stack of enclosing indents.
    let mut depths: Vec<Option<usize>> = vec![None; raw.len()];
    let mut stack: Vec<usize> = Vec::new();
    for (index, indent) in indents.iter().enumerate() {
        let Some(indent) = *indent else { continue };
        while stack.last().is_some_and(|top| *top > indent) {
            stack.pop();
        }
        if stack.last() != Some(&indent) {
            stack.push(indent);
        }
        depths[index] = Some(stack.len() - 1);
    }
    // A comment takes the depth of the next code line.
    let mut next = 0;
    for index in (0..raw.len()).rev() {
        match depths[index] {
            Some(depth) => next = depth,
            None if !raw[index].trim().is_empty() => depths[index] = Some(next),
            None => {}
        }
    }

    let mut lines: Vec<String> = Vec::new();
    for (index, line) in raw.iter().enumerate() {
        let content = line.trim();
        if content.is_empty() {
            if lines.last().is_some_and(|l| !l.is_empty()) {
                lines.push(String::new());
            }
            continue;
        }
        let depth = depths[index].unwrap_or(0);
        let top_block =
            depth == 0 && (content.starts_with("stage ") || content.starts_with("define "));
        if top_block && lines.last().is_some_and(|l| !l.is_empty()) {
            // Keep a comment that introduces the block attached to it.
            let mut insert_at = lines.len();
            while insert_at > 0 && lines[insert_at - 1].starts_with('#') {
                insert_at -= 1;
            }
            if insert_at > 0 && !lines[insert_at - 1].is_empty() {
                lines.insert(insert_at, String::new());
            }
        }
        lines.push(format!(
            "{}{}",
            "  ".repeat(depth),
            collapse_spaces(content)
        ));
    }
    while lines.last().is_some_and(String::is_empty) {
        lines.pop();
    }
    align_captures(&mut lines);
    let mut out = lines.join("\n");
    out.push('\n');
    out
}

/// Collapses runs of spaces between the leading keyword and the name in option lines, so
/// alignment starts from a clean slate. Other text, such as argv and filters, is untouched.
fn collapse_spaces(content: &str) -> String {
    if let Some(rest) = content.strip_prefix("capture ")
        && let Some((name, value)) = rest.split_once('=')
    {
        return format!("capture {} = {}", name.trim(), value.trim());
    }
    content.to_string()
}

fn align_captures(lines: &mut [String]) {
    let mut start = 0;
    while start < lines.len() {
        let indent = |l: &str| l.len() - l.trim_start().len();
        let is_capture = |l: &str| l.trim_start().starts_with("capture ") && l.contains(" = ");
        if !is_capture(&lines[start]) {
            start += 1;
            continue;
        }
        let depth = indent(&lines[start]);
        let mut end = start;
        while end < lines.len() && is_capture(&lines[end]) && indent(&lines[end]) == depth {
            end += 1;
        }
        let width = lines[start..end]
            .iter()
            .map(|l| l.find(" = ").unwrap_or(0))
            .max()
            .unwrap_or(0);
        for line in &mut lines[start..end] {
            if let Some(at) = line.find(" = ") {
                let (head, tail) = line.split_at(at);
                *line = format!("{head:width$}{tail}");
            }
        }
        start = end;
    }
}

#[cfg(test)]
mod tests {
    use super::format;

    #[test]
    fn reindents_aligns_and_keeps_comments() {
        let input = concat!(
            "korin 1   \n",
            "title \"t\"\n",
            "# The first stage.\n",
            "stage \"a\"\n",
            "    # read it\n",
            "    read tool\n",
            "        capture status =   jq .Status\n",
            "        capture security_groups = jq list .Groups[]\n",
            "\n",
            "\n",
            "    note \"x\"\n",
            "stage \"b\"\n",
            "      note \"y\"\n",
            "\n",
        );
        let expected = concat!(
            "korin 1\n",
            "title \"t\"\n",
            "\n",
            "# The first stage.\n",
            "stage \"a\"\n",
            "  # read it\n",
            "  read tool\n",
            "    capture status          = jq .Status\n",
            "    capture security_groups = jq list .Groups[]\n",
            "\n",
            "  note \"x\"\n",
            "\n",
            "stage \"b\"\n",
            "  note \"y\"\n",
        );
        assert_eq!(format(input), expected);
        assert_eq!(format(expected), expected, "formatting is idempotent");
    }
}
