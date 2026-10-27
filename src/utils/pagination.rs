//! Pagination: turn a statement plus a host `limit` / `page` into the SQL for
//! that page.
//!
//! The statement is read with `sqlscan`, so a comment, a string literal or a
//! quoted identifier is never mistaken for a `LIMIT` and never swallows the
//! clause that is appended:
//!
//! - a trailing `-- note` would comment out an appended `LIMIT`, so page 2
//!   would repeat page 1;
//! - a trailing `/* note */` after the user's own `LIMIT` would hide it, so a
//!   second `LIMIT` was appended and the server rejected the SQL.
//!
//! Trailing comments and semicolons are cut before the clause is added.
//!
//! Two forms are tried, in order:
//!
//! 1. Append `LIMIT n+1 OFFSET m` to the statement itself, after removing its
//!    own trailing `LIMIT` / `OFFSET` (a user `LIMIT` still caps the total
//!    across pages). `ORDER BY` stays in place, so table-qualified columns
//!    stay valid. The extra row tells the host there is another page.
//! 2. When the statement has a limit-like clause this module cannot read
//!    (`LIMIT ALL`, `LIMIT $1`, `FETCH FIRST ... ROWS ONLY`, `OFFSET n ROWS`),
//!    wrap it: `SELECT * FROM (<statement>) AS t LIMIT n+1 OFFSET m`. Without
//!    this, page 2 of such a query would repeat page 1.
//!
//! `None` means the statement cannot be rewritten safely (an unterminated
//! string or comment, or several statements); the caller then caps the rows on
//! its own side instead of changing the SQL.

use crate::sqlscan::{scan, Kind, Token};

/// Compute the SQL LIMIT and OFFSET for a given page and page size.
/// Pages are 1-indexed.
pub fn limit_offset(page: u32, page_size: u32) -> (u32, u32) {
    let offset = page.saturating_sub(1).saturating_mul(page_size);
    (page_size, offset)
}

/// The code tokens of a statement (comments dropped) and how many of them
/// remain once trailing semicolons are removed. `None` if the text is not
/// completely tokenized, or holds more than one statement.
fn code_tokens(query: &str) -> Option<(Vec<Token>, usize)> {
    let scanned = scan(query);
    if !scanned.complete {
        return None;
    }
    let code: Vec<Token> = scanned
        .tokens
        .into_iter()
        .filter(|t| t.kind != Kind::Comment)
        .collect();
    let mut end = code.len();
    while end > 0 && code[end - 1].kind == Kind::Semi {
        end -= 1;
    }
    if code[..end].iter().any(|t| t.kind == Kind::Semi) {
        return None;
    }
    Some((code, end))
}

fn digits(kind: &Kind) -> Option<&str> {
    match kind {
        Kind::Word(w) if !w.is_empty() && w.bytes().all(|b| b.is_ascii_digit()) => Some(w),
        _ => None,
    }
}

/// A `LIMIT` / `OFFSET` / `FETCH` keyword at the top level of the tokens.
fn has_top_level_limit_keyword(tokens: &[Token]) -> bool {
    let mut depth = 0usize;
    tokens.iter().any(|t| match &t.kind {
        Kind::Open => {
            depth += 1;
            false
        }
        Kind::Close => {
            depth = depth.saturating_sub(1);
            false
        }
        Kind::Word(w) => depth == 0 && matches!(w.as_str(), "LIMIT" | "OFFSET" | "FETCH"),
        _ => false,
    })
}

/// Form 1: the statement with its own trailing `LIMIT n` / `OFFSET n`
/// replaced by this page's clause.
pub fn build_paginated_query(query: &str, page_size: u32, page: u32) -> Option<String> {
    let (code, mut cut) = code_tokens(query)?;

    // Trailing `LIMIT n` and `OFFSET n`, in either order, each at most once.
    let (mut user_limit, mut user_offset) = (None::<u32>, None::<u32>);
    while cut >= 2 {
        let (Kind::Word(keyword), Some(number)) =
            (&code[cut - 2].kind, digits(&code[cut - 1].kind))
        else {
            break;
        };
        let slot = match keyword.as_str() {
            "LIMIT" => &mut user_limit,
            "OFFSET" => &mut user_offset,
            _ => break,
        };
        if slot.is_some() {
            break;
        }
        // A value too large for u32 cannot be combined with the page offset.
        *slot = Some(number.parse().ok()?);
        cut -= 2;
    }

    // Anything limit-like left at the top level is a form we cannot read.
    if cut == 0 || has_top_level_limit_keyword(&code[..cut]) {
        return None;
    }
    let base = &query[..code[cut - 1].end];

    let page_offset = limit_offset(page, page_size).1;
    let fetch_count = match user_limit {
        Some(user) => user
            .saturating_sub(page_offset)
            .min(page_size.saturating_add(1)),
        None => page_size.saturating_add(1),
    };
    let offset = user_offset.unwrap_or(0).saturating_add(page_offset);
    Some(format!("{base} LIMIT {fetch_count} OFFSET {offset}"))
}

/// Form 2: the whole statement as a subquery. The statement goes on its own
/// lines, so a comment inside it cannot reach the clause that follows.
pub fn wrap_paginated_query(query: &str, page_size: u32, page: u32) -> Option<String> {
    let (code, end) = code_tokens(query)?;
    if end == 0 {
        return None;
    }
    let inner = &query[..code[end - 1].end];
    let (_, offset) = limit_offset(page, page_size);
    Some(format!(
        "SELECT * FROM (\n{inner}\n) AS tabularis_page LIMIT {} OFFSET {offset}",
        page_size.saturating_add(1)
    ))
}

/// The SQL for one page, trying [`build_paginated_query`] and then
/// [`wrap_paginated_query`]. `None` when the statement cannot be rewritten.
pub fn paginate_query(query: &str, page_size: u32, page: u32) -> Option<String> {
    build_paginated_query(query, page_size, page)
        .or_else(|| wrap_paginated_query(query, page_size, page))
}

#[cfg(test)]
#[path = "pagination_tests.rs"]
mod tests;
