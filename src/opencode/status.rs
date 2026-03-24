use std::collections::VecDeque;

/// Maximum number of thinking/tool sections to keep in the sliding window.
const MAX_SECTIONS: usize = 3;

/// Renders Pi events into Telegram-friendly status text.
/// Pure, testable, no I/O.
pub struct StatusBuilder {
    sections: VecDeque<(String, String)>,
    response: String,
}

impl StatusBuilder {
    pub fn new() -> Self {
        Self {
            sections: VecDeque::new(),
            response: String::new(),
        }
    }

    /// Push a section while enforcing the sliding window limit.
    fn push_section(&mut self, icon: &str, text: String) {
        if self.sections.len() >= MAX_SECTIONS {
            self.sections.pop_front();
        }
        self.sections.push_back((icon.to_string(), text));
    }

    /// Record a thinking block. Prefix with the thought-bubble icon and
    /// truncate to 200 characters.
    pub fn on_thinking_end(&mut self, text: &str) {
        let truncated = truncate(text, 200);
        self.push_section("\u{1f4ad}", truncated);
    }

    /// Record the start of a tool invocation with an appropriate icon.
    pub fn on_tool_start(&mut self, name: &str, args: &serde_json::Value) {
        let icon = tool_icon(name);
        let summary = match name {
            "bash" | "shell" => args
                .get("command")
                .and_then(|v| v.as_str())
                .unwrap_or(name)
                .to_string(),
            _ => {
                let compact = serde_json::to_string(args).unwrap_or_default();
                if compact.len() > 120 {
                    format!("{}\u{2026}", &compact[..120])
                } else {
                    compact
                }
            }
        };
        self.push_section(icon, summary);
    }

    /// Record the end of a tool invocation. Prefix with the page icon and
    /// truncate to 150 characters.
    pub fn on_tool_end(&mut self, _name: &str, output: &str) {
        let truncated = truncate(output, 150);
        self.push_section("\u{1f4c4}", truncated);
    }

    /// Set the final response text.
    pub fn on_response_text(&mut self, text: &str) {
        self.response = text.to_string();
    }

    /// Render all accumulated sections into a single string suitable for
    /// Telegram. The total output is capped at 3800 characters, keeping the
    /// tail (most recent events) when truncation is necessary.
    pub fn render(&self) -> String {
        if self.sections.is_empty() && self.response.is_empty() {
            return "\u{2699} Pi is working\u{2026}".to_string();
        }

        let mut parts: Vec<String> = self
            .sections
            .iter()
            .map(|(icon, text)| format!("{icon} {text}"))
            .collect();

        if !self.response.is_empty() {
            parts.push(self.response.clone());
        }

        let joined = parts.join("\n");

        if joined.len() <= 3800 {
            joined
        } else {
            // Keep the tail (most recent events).
            // Walk forward from (len - 3800) to a char boundary to avoid
            // slicing inside a multi-byte UTF-8 sequence (e.g. Cyrillic/CJK).
            let raw_start = joined.len() - 3800;
            let char_start = (raw_start..=joined.len())
                .find(|&i| joined.is_char_boundary(i))
                .unwrap_or(joined.len());
            // Then skip to the next newline so we don't start mid-line.
            let cut = joined[char_start..]
                .find('\n')
                .map_or(char_start, |pos| char_start + pos + 1);
            joined[cut..].to_string()
        }
    }
}

/// Pick an icon for a tool by name.
fn tool_icon(name: &str) -> &'static str {
    match name {
        "read" | "cat" => "\u{1f4d6}",
        "write" | "edit" => "\u{270f}\u{fe0f}",
        "find" | "grep" | "glob" | "search" => "\u{1f50d}",
        // bash, shell, and everything else get the wrench
        _ => "\u{1f527}",
    }
}

/// Truncate a string to at most `max` *bytes*, appending an ellipsis if
/// truncated. The cut is always on a char boundary to avoid panics with
/// multi-byte UTF-8 sequences (e.g. Cyrillic, CJK, emoji).
fn truncate(s: &str, max: usize) -> String {
    if s.len() <= max {
        s.to_string()
    } else {
        // Walk back from `max` to find a valid char boundary
        let boundary = (0..=max)
            .rev()
            .find(|&i| s.is_char_boundary(i))
            .unwrap_or(0);
        format!("{}\u{2026}", &s[..boundary])
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn renders_thinking() {
        let mut b = StatusBuilder::new();
        b.on_thinking_end("I need to check the file");
        let out = b.render();
        assert!(
            out.contains('\u{1f4ad}'),
            "should contain thought-bubble icon"
        );
        assert!(out.contains("I need to check the file"));
    }

    #[test]
    fn renders_tool_start() {
        let mut b = StatusBuilder::new();
        b.on_tool_start("bash", &json!({"command": "ls -la"}));
        let out = b.render();
        assert!(out.contains('\u{1f527}'), "should contain wrench icon");
        assert!(out.contains("ls -la"));
    }

    #[test]
    fn renders_tool_output() {
        let mut b = StatusBuilder::new();
        b.on_tool_end("bash", "total 42\ndrwxr-xr-x");
        let out = b.render();
        assert!(out.contains('\u{1f4c4}'), "should contain page icon");
        assert!(out.contains("total 42"));
    }

    #[test]
    fn truncates_to_limit() {
        let mut b = StatusBuilder::new();
        for i in 0..100 {
            b.on_tool_start(
                "bash",
                &json!({"command": format!("command-number-{i}-with-padding-text-here")}),
            );
        }
        assert_eq!(
            b.sections.len(),
            MAX_SECTIONS,
            "sections should be capped at {MAX_SECTIONS}, got {}",
            b.sections.len()
        );
        let out = b.render();
        assert!(
            out.len() <= 3800,
            "render length {} exceeds 3800",
            out.len()
        );
    }

    #[test]
    fn truncates_cyrillic_without_panic() {
        // Cyrillic chars are 2 bytes each. A naïve &s[..3800] would panic
        // when the byte boundary falls inside a 2-byte sequence.
        let mut b = StatusBuilder::new();
        let cyrillic_chunk = "Привет мир! ".repeat(40); // ~480 bytes per repeat
        for _ in 0..10 {
            b.on_tool_end("bash", &cyrillic_chunk);
        }
        // Must not panic and must produce valid UTF-8 within length limit.
        let out = b.render();
        assert!(out.len() <= 3800);
        assert!(std::str::from_utf8(out.as_bytes()).is_ok());
    }

    #[test]
    fn sliding_window_keeps_last_3() {
        let mut b = StatusBuilder::new();
        for i in 1..=5 {
            b.on_thinking_end(&format!("thought-{i}"));
        }
        assert_eq!(b.sections.len(), 3);
        let out = b.render();
        // Oldest two (thought-1, thought-2) should have been evicted.
        assert!(!out.contains("thought-1"), "thought-1 should be evicted");
        assert!(!out.contains("thought-2"), "thought-2 should be evicted");
        assert!(out.contains("thought-3"));
        assert!(out.contains("thought-4"));
        assert!(out.contains("thought-5"));
    }

    #[test]
    fn empty_renders_fallback() {
        let b = StatusBuilder::new();
        assert_eq!(b.render(), "\u{2699} Pi is working\u{2026}");
    }

    #[test]
    fn renders_response_text() {
        let mut b = StatusBuilder::new();
        b.on_thinking_end("thinking about it");
        b.on_response_text("Here is the answer.");
        let out = b.render();
        assert!(out.contains('\u{1f4ad}'));
        assert!(out.contains("Here is the answer."));
    }
}
