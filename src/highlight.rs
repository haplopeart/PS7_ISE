//! PowerShell syntax colouring for the script pane.
//!
//! The colouring is applied over the text the control already holds: rewriting the text
//! (the old `set_text` approach) resets the caret and the undo stack, which is what made
//! the script pane impossible to type in.

use native_windows_gui as nwg;

const KEYWORDS: &[&str] = &[
    "if", "else", "elseif", "foreach", "for", "while", "do", "switch", "try", "catch", "finally",
    "begin", "process", "end", "param", "return", "throw", "break", "continue", "function",
    "filter", "workflow", "class", "enum", "using", "module", "data", "in", "and", "or", "not",
    "is", "isnot", "as", "from", "define", "var",
];

const OPERATORS: &[&str] = &[
    "eq", "ne", "ge", "gt", "le", "lt", "like", "notlike", "match", "notmatch", "contains",
    "notcontains", "in", "notin", "replace", "split", "join", "band", "bor", "shl", "shr",
];

const COMMENT: [u8; 3] = [0x00, 0x80, 0x00];
const STRING: [u8; 3] = [0x8B, 0x00, 0x00];
const VARIABLE: [u8; 3] = [0xC0, 0x40, 0x00];
const PARAMETER: [u8; 3] = [0x00, 0x72, 0x9B];
const NUMBER: [u8; 3] = [0x80, 0x00, 0x80];
const KEYWORD: [u8; 3] = [0x00, 0x00, 0xFF];
const COMMAND: [u8; 3] = [0x00, 0x00, 0x8B];
const TEXT: [u8; 3] = [0x00, 0x00, 0x00];

/// Maps character positions in the string we colour to indices inside the control,
/// because a rich text control may count a line break as one character while the string
/// we read back holds two.
fn control_indices(text: &str) -> Vec<u32> {
    let mut indices = Vec::with_capacity(text.len() + 1);
    let mut current = 0u32;
    let mut previous_return = false;
    for ch in text.chars() {
        indices.push(current);
        if !(ch == '\n' && previous_return) {
            current += 1;
        }
        previous_return = ch == '\r';
    }
    indices.push(current);
    indices
}

fn colour(rtb: &nwg::RichTextBox, range: std::ops::Range<u32>, color: [u8; 3], bold: bool) {
    if range.end <= range.start {
        return;
    }
    rtb.set_selection(range);
    rtb.set_char_format(&nwg::CharFormat {
        text_color: Some(color),
        effects: if bold {
            Some(nwg::CharEffects::BOLD)
        } else {
            Some(nwg::CharEffects::empty())
        },
        ..Default::default()
    });
}

/// Repaints the colours of `text`, leaving the caret where the user left it.
pub fn highlight(rtb: &nwg::RichTextBox, text: &str) {
    let caret = rtb.selection();
    let index = control_indices(text);
    let chars: Vec<char> = text.chars().collect();

    // Everything starts black; individual rules override as they match.
    colour(
        rtb,
        0..index[chars.len()],
        TEXT,
        false,
    );

    let mut i = 0usize;
    while i < chars.len() {
        let ch = chars[i];

        if ch.is_whitespace() {
            i += 1;
            continue;
        }

        // Comment to end of line
        if ch == '#' {
            let start = i;
            while i < chars.len() && chars[i] != '\n' {
                i += 1;
            }
            colour(rtb, index[start]..index[i], COMMENT, false);
            continue;
        }

        // Block comment <# ... #>
        if ch == '<' && i + 1 < chars.len() && chars[i + 1] == '#' {
            let start = i;
            i += 2;
            while i + 1 < chars.len() && !(chars[i] == '#' && chars[i + 1] == '>') {
                i += 1;
            }
            i = (i + 2).min(chars.len());
            colour(rtb, index[start]..index[i], COMMENT, false);
            continue;
        }

        // Strings, including here-strings
        if ch == '"' || ch == '\'' {
            let start = i;
            let triple = i + 2 < chars.len() && chars[i + 1] == ch && chars[i + 2] == ch;
            if triple {
                i += 3;
                while i + 2 < chars.len() && !(chars[i] == ch && chars[i + 1] == ch && chars[i + 2] == ch)
                {
                    i += 1;
                }
                i = (i + 3).min(chars.len());
            } else {
                i += 1;
                while i < chars.len() && chars[i] != ch {
                    if chars[i] == '`' && i + 1 < chars.len() {
                        i += 2;
                    } else if ch == '"' && chars[i] == '$' && i + 1 < chars.len() && chars[i + 1] == '(' {
                        // stop colouring inside a sub-expression, it is code, not text
                        break;
                    } else {
                        i += 1;
                    }
                }
                if i < chars.len() {
                    i += 1;
                }
            }
            colour(rtb, index[start]..index[i], STRING, false);
            continue;
        }

        // Variables: $name, $env:PATH, ${a b}, @name
        if ch == '$' || ch == '@' {
            let start = i;
            i += 1;
            if i < chars.len() && chars[i] == '{' {
                while i < chars.len() && chars[i] != '}' {
                    i += 1;
                }
                i = (i + 1).min(chars.len());
            } else {
                while i < chars.len()
                    && (chars[i].is_alphanumeric() || chars[i] == '_' || chars[i] == ':')
                {
                    i += 1;
                }
            }
            colour(rtb, index[start]..index[i], VARIABLE, false);
            continue;
        }

        // Parameter: -Name
        if ch == '-' && i + 1 < chars.len() && chars[i + 1].is_alphabetic() {
            let start = i;
            i += 1;
            while i < chars.len() && (chars[i].is_alphanumeric() || chars[i] == '-') {
                i += 1;
            }
            colour(rtb, index[start]..index[i], PARAMETER, false);
            continue;
        }

        // Numbers
        if ch.is_ascii_digit() {
            let start = i;
            while i < chars.len()
                && (chars[i].is_ascii_digit()
                    || chars[i] == '.'
                    || chars[i] == 'x'
                    || chars[i] == 'X')
            {
                i += 1;
            }
            colour(rtb, index[start]..index[i], NUMBER, false);
            continue;
        }

        // Words: keyword, comparison operator or command name
        if ch.is_alphanumeric() || ch == '_' {
            let start = i;
            while i < chars.len()
                && (chars[i].is_alphanumeric() || chars[i] == '_' || chars[i] == '-' || chars[i] == '\\')
            {
                i += 1;
            }
            let word: String = chars[start..i].iter().collect();
            let lower = word.to_lowercase();
            if KEYWORDS.contains(&lower.as_str()) {
                colour(rtb, index[start]..index[i], KEYWORD, true);
            } else if OPERATORS.contains(&lower.as_str()) {
                colour(rtb, index[start]..index[i], KEYWORD, false);
            } else if word.contains('-') && word.chars().next().map(|c| c.is_alphabetic()).unwrap_or(false)
            {
                colour(rtb, index[start]..index[i], COMMAND, false);
            }
            continue;
        }

        i += 1;
    }

    rtb.set_selection(caret.start..caret.end);
}
