//! A small SQL scanner that knows where text is *not* code: string literals
//! (including `E'..'` escapes and dollar quotes), quoted identifiers and
//! comments (`--` line and nested `/* */` block). It records byte positions so
//! callers can cut and rewrite a statement without ever treating the inside of
//! a literal or a comment as syntax.
//!
//! It does not parse SQL. It only separates words, punctuation and the opaque
//! pieces above, which is all that statement classification and pagination
//! need.

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Kind {
    /// A bare word or number, uppercased (`SELECT`, `LIMIT`, `10`, `MY_TABLE`).
    Word(String),
    /// A double-quoted identifier.
    Quoted,
    /// A string literal or dollar-quoted body.
    Str,
    /// A `--` or `/* */` comment.
    Comment,
    Open,
    Close,
    Comma,
    Semi,
    Other,
}

#[derive(Debug, Clone)]
pub struct Token {
    pub kind: Kind,
    /// Byte range in the scanned text, `start..end`.
    pub start: usize,
    pub end: usize,
}

#[derive(Debug)]
pub struct Scan {
    pub tokens: Vec<Token>,
    /// False when a string, quoted identifier or block comment never closes.
    /// Such a statement is invalid SQL; rewriting it could turn an error into
    /// a different, valid statement, so callers must leave it alone.
    pub complete: bool,
}

fn is_word_char(c: char) -> bool {
    c.is_ascii_alphanumeric() || c == '_'
}

pub fn scan(sql: &str) -> Scan {
    let chars: Vec<(usize, char)> = sql.char_indices().collect();
    let pos = |i: usize| chars.get(i).map_or(sql.len(), |&(p, _)| p);
    let at = |i: usize| chars.get(i).map(|&(_, c)| c);

    let mut tokens: Vec<Token> = Vec::new();
    let mut complete = true;
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i].1;
        let start = pos(i);
        let kind = match c {
            c if c.is_whitespace() => {
                i += 1;
                continue;
            }
            '-' if at(i + 1) == Some('-') => {
                while i < chars.len() && chars[i].1 != '\n' {
                    i += 1;
                }
                Kind::Comment
            }
            '/' if at(i + 1) == Some('*') => {
                let mut depth = 1;
                i += 2;
                while i < chars.len() && depth > 0 {
                    if chars[i].1 == '*' && at(i + 1) == Some('/') {
                        depth -= 1;
                        i += 2;
                    } else if chars[i].1 == '/' && at(i + 1) == Some('*') {
                        depth += 1;
                        i += 2;
                    } else {
                        i += 1;
                    }
                }
                if depth > 0 {
                    complete = false;
                }
                Kind::Comment
            }
            '\'' => {
                // `E'...'` strings also escape with a backslash.
                let escaped = i > 0
                    && matches!(chars[i - 1].1, 'e' | 'E')
                    && (i < 2 || !is_word_char(chars[i - 2].1));
                let mut closed = false;
                i += 1;
                while i < chars.len() {
                    if escaped && chars[i].1 == '\\' {
                        i += 2;
                    } else if chars[i].1 == '\'' {
                        if at(i + 1) == Some('\'') {
                            i += 2;
                        } else {
                            i += 1;
                            closed = true;
                            break;
                        }
                    } else {
                        i += 1;
                    }
                }
                i = i.min(chars.len());
                if !closed {
                    complete = false;
                }
                // The `E` prefix belongs to the literal.
                let mut literal_start = start;
                if escaped {
                    if let Some(prev) = tokens.last() {
                        if matches!(&prev.kind, Kind::Word(w) if w == "E") && prev.end == start {
                            literal_start = prev.start;
                            tokens.pop();
                        }
                    }
                }
                tokens.push(Token {
                    kind: Kind::Str,
                    start: literal_start,
                    end: pos(i),
                });
                continue;
            }
            '"' => {
                let mut closed = false;
                i += 1;
                while i < chars.len() {
                    if chars[i].1 == '"' {
                        if at(i + 1) == Some('"') {
                            i += 2;
                        } else {
                            i += 1;
                            closed = true;
                            break;
                        }
                    } else {
                        i += 1;
                    }
                }
                if !closed {
                    complete = false;
                }
                Kind::Quoted
            }
            '$' => {
                // `$tag$ ... $tag$` (tag may be empty). `$1` is a placeholder.
                let mut j = i + 1;
                while j < chars.len() && is_word_char(chars[j].1) {
                    j += 1;
                }
                let tag_ok = j == i + 1 || !chars[i + 1].1.is_ascii_digit();
                if tag_ok && at(j) == Some('$') {
                    let delim: Vec<char> = chars[i..=j].iter().map(|&(_, c)| c).collect();
                    let body = j + 1;
                    let close = (body..chars.len()).find(|&k| {
                        k + delim.len() <= chars.len()
                            && chars[k..k + delim.len()]
                                .iter()
                                .map(|&(_, c)| c)
                                .eq(delim.iter().copied())
                    });
                    match close {
                        Some(k) => i = k + delim.len(),
                        None => {
                            i = chars.len();
                            complete = false;
                        }
                    }
                    Kind::Str
                } else {
                    i += 1;
                    Kind::Other
                }
            }
            '(' => {
                i += 1;
                Kind::Open
            }
            ')' => {
                i += 1;
                Kind::Close
            }
            ',' => {
                i += 1;
                Kind::Comma
            }
            ';' => {
                i += 1;
                Kind::Semi
            }
            c if is_word_char(c) => {
                while i < chars.len() && is_word_char(chars[i].1) {
                    i += 1;
                }
                Kind::Word(sql[start..pos(i)].to_ascii_uppercase())
            }
            _ => {
                i += 1;
                Kind::Other
            }
        };
        tokens.push(Token {
            kind,
            start,
            end: pos(i),
        });
    }
    Scan { tokens, complete }
}

#[cfg(test)]
#[path = "sqlscan_tests.rs"]
mod tests;
