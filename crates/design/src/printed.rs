//! What a call's snippet printed (the snippet API's `Print`), for its
//! result, cut off past [`MAX_PRINTED`] so one call can't flood the agent.

/// How much one call's `Print` output holds, in bytes.
pub const MAX_PRINTED: usize = 64 * 1024;

#[derive(Debug, Default)]
pub struct Printed {
    lines: Vec<String>,
    bytes: usize,
}

impl Printed {
    /// Adds a line, or past [`MAX_PRINTED`] one saying the rest was cut.
    pub fn push(&mut self, line: String) {
        if self.bytes > MAX_PRINTED {
            return;
        }
        self.bytes += line.len();
        if self.bytes > MAX_PRINTED {
            self.lines
                .push(format!("(Print's output stops here: a call prints at most {MAX_PRINTED} bytes)"));
        } else {
            self.lines.push(line);
        }
    }

    pub fn into_lines(self) -> Vec<String> {
        self.lines
    }
}
