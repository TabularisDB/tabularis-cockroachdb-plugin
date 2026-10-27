//! Statement classification: which statements return rows, accept a trailing
//! `LIMIT`, change the transaction state, or cannot run inside a
//! transaction. Everything works on the leading keywords only, so a `BEGIN`
//! inside a string literal or a later clause is never mistaken for
//! transaction control.

/// The first `n` keywords of `query`, uppercased. Every `--` and (nested)
/// `/* */` comment is skipped wherever it appears, and reading stops at a
/// string literal, quoted identifier, dollar quote or `;`, so no data is read
/// as a keyword.
pub fn leading_keywords(query: &str, n: usize) -> Vec<String> {
    let mut words = Vec::new();
    let mut word = String::new();
    let mut chars = query.chars().peekable();
    let mut comment_depth = 0usize;
    while let Some(c) = chars.next() {
        if comment_depth > 0 {
            if c == '*' && chars.peek() == Some(&'/') {
                chars.next();
                comment_depth -= 1;
            } else if c == '/' && chars.peek() == Some(&'*') {
                chars.next();
                comment_depth += 1;
            }
            continue;
        }
        if c.is_ascii_alphanumeric() || c == '_' {
            word.push(c.to_ascii_uppercase());
            continue;
        }
        if !word.is_empty() {
            words.push(std::mem::take(&mut word));
            if words.len() == n {
                return words;
            }
        }
        match (c, chars.peek()) {
            ('-', Some('-')) => {
                for c in chars.by_ref() {
                    if c == '\n' {
                        break;
                    }
                }
            }
            ('/', Some('*')) => {
                chars.next();
                comment_depth = 1;
            }
            ('\'' | '"' | '$' | ';', _) => return words,
            _ => {}
        }
    }
    if !word.is_empty() && words.len() < n {
        words.push(word);
    }
    words
}

fn first_keyword(query: &str) -> Option<String> {
    leading_keywords(query, 1).into_iter().next()
}

/// A token of a SQL statement, reduced to what is needed to find the main
/// statement of a `WITH` query. Strings, quoted identifiers, dollar quotes and
/// comments are skipped as opaque tokens (see `sqlscan`), so their contents
/// (parentheses, `INSERT`, `;`) can never be mistaken for syntax.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Tok {
    /// A bare word, uppercased (`SELECT`, `src`, `AS`, ...).
    Word(String),
    /// A double-quoted identifier.
    Quoted,
    /// A string literal or dollar-quoted body.
    Str,
    Open,
    Close,
    Comma,
    Other,
}

fn tokenize(sql: &str) -> Vec<Tok> {
    use crate::sqlscan::{scan, Kind};
    scan(sql)
        .tokens
        .into_iter()
        .filter_map(|t| match t.kind {
            Kind::Comment => None,
            Kind::Word(w) => Some(Tok::Word(w)),
            Kind::Quoted => Some(Tok::Quoted),
            Kind::Str => Some(Tok::Str),
            Kind::Open => Some(Tok::Open),
            Kind::Close => Some(Tok::Close),
            Kind::Comma => Some(Tok::Comma),
            Kind::Semi | Kind::Other => Some(Tok::Other),
        })
        .collect()
}

/// Index just past the balanced `( ... )` group that opens at `toks[at]`.
fn skip_group(toks: &[Tok], at: usize) -> Option<usize> {
    if toks.get(at) != Some(&Tok::Open) {
        return None;
    }
    let mut depth = 0usize;
    for (i, t) in toks.iter().enumerate().skip(at) {
        match t {
            Tok::Open => depth += 1,
            Tok::Close => {
                depth -= 1;
                if depth == 0 {
                    return Some(i + 1);
                }
            }
            _ => {}
        }
    }
    None
}

/// The keyword of the statement that follows a `WITH` clause: `SELECT` for
/// `WITH a AS (...) SELECT ...`, `INSERT` for `WITH a AS (...) INSERT ...`.
///
/// `WITH [RECURSIVE] name [(cols)] AS [[NOT] MATERIALIZED] (query) [, ...] main`.
/// `None` when the clause cannot be parsed; callers must then treat the
/// statement as possibly writing.
pub fn with_main_keyword(query: &str) -> Option<String> {
    let toks = tokenize(query);
    let mut i = 0;
    if toks.get(i) != Some(&Tok::Word("WITH".into())) {
        return None;
    }
    i += 1;
    if toks.get(i) == Some(&Tok::Word("RECURSIVE".into())) {
        i += 1;
    }
    loop {
        // CTE name.
        if !matches!(toks.get(i), Some(Tok::Word(_) | Tok::Quoted)) {
            return None;
        }
        i += 1;
        // Optional column list.
        if toks.get(i) == Some(&Tok::Open) {
            i = skip_group(&toks, i)?;
        }
        if toks.get(i) != Some(&Tok::Word("AS".into())) {
            return None;
        }
        i += 1;
        if toks.get(i) == Some(&Tok::Word("NOT".into())) {
            i += 1;
        }
        if toks.get(i) == Some(&Tok::Word("MATERIALIZED".into())) {
            i += 1;
        }
        i = skip_group(&toks, i)?;
        if toks.get(i) == Some(&Tok::Comma) {
            i += 1;
            continue;
        }
        break;
    }
    match toks.get(i) {
        Some(Tok::Word(w)) => Some(w.clone()),
        _ => None,
    }
}

/// A statement that only reads, and whose grammar accepts a trailing
/// `LIMIT`/`OFFSET`: `SELECT`, `VALUES`, `TABLE`, and a `WITH` whose main
/// statement is one of those.
///
/// A host display limit must never change what a statement writes, so a
/// `WITH ... INSERT | UPDATE | DELETE | UPSERT` (with or without `RETURNING`)
/// is never paginated: a `LIMIT` appended to it would apply to the write's
/// source query and silently write fewer rows. `WITH ins AS (INSERT ...)
/// SELECT ...` is safe, because a data-modifying CTE always runs to
/// completion and the `LIMIT` only trims the outer `SELECT`.
///
/// `SHOW` and `CALL` reject `LIMIT` (`SHOW x LIMIT 5` is a syntax error), so
/// they, and anything that cannot be classified, are capped client-side.
pub fn supports_trailing_limit(query: &str) -> bool {
    match first_keyword(query).as_deref() {
        Some("SELECT" | "VALUES" | "TABLE") => true,
        Some("WITH") => matches!(
            with_main_keyword(query).as_deref(),
            Some("SELECT" | "VALUES" | "TABLE")
        ),
        _ => false,
    }
}

/// What a statement does to the surrounding transaction.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TransactionEffect {
    /// `BEGIN`, `START TRANSACTION`.
    Opens,
    /// `COMMIT`, `END`, `ROLLBACK`, `ABORT`. `ROLLBACK TO SAVEPOINT` is
    /// [`TransactionEffect::None`].
    Closes,
    /// `COMMIT AND CHAIN`: ends one transaction and opens the next.
    Chains,
    None,
}

impl TransactionEffect {
    /// Whether a transaction is open after a statement with this effect ran.
    pub fn in_transaction_after(self, succeeded: bool, before: bool) -> bool {
        match (self, succeeded) {
            (TransactionEffect::Opens | TransactionEffect::Chains, true) => true,
            // The transaction ends even when COMMIT itself fails.
            (TransactionEffect::Closes, _) | (TransactionEffect::Chains, false) => false,
            _ => before,
        }
    }
}

pub fn transaction_effect(query: &str) -> TransactionEffect {
    // Six cover the longest form, `ROLLBACK TRANSACTION AND NO CHAIN`.
    let mut words = leading_keywords(query, 6).into_iter();
    let Some(first) = words.next() else {
        return TransactionEffect::None;
    };
    let mut rest: Vec<String> = words.collect();
    if matches!(first.as_str(), "COMMIT" | "END" | "ROLLBACK" | "ABORT")
        && matches!(
            rest.first().map(String::as_str),
            Some("WORK" | "TRANSACTION")
        )
    {
        rest.remove(0);
    }
    let second = rest.first().map(String::as_str);
    let chains = rest.windows(2).any(|p| p[0] == "AND" && p[1] == "CHAIN");

    match first.as_str() {
        "BEGIN" => TransactionEffect::Opens,
        "START" if second == Some("TRANSACTION") => TransactionEffect::Opens,
        "ROLLBACK" if second == Some("TO") => TransactionEffect::None,
        "COMMIT" | "END" | "ROLLBACK" | "ABORT" if chains => TransactionEffect::Chains,
        "COMMIT" | "END" | "ROLLBACK" | "ABORT" => TransactionEffect::Closes,
        _ => TransactionEffect::None,
    }
}

/// Statements that change state living on the connection: a session `SET`
/// (not `SET LOCAL`, `SET TRANSACTION` or `SET CLUSTER SETTING`), `USE`, a
/// temporary table, or `set_config(...)` anywhere in the statement. The
/// connection must be reset before it goes back to the pool.
///
/// `set_config` is matched as a substring, so a literal that merely contains
/// the word costs one extra `RESET ALL`: a false positive is only a cleanup,
/// a false negative leaks a setting to the next tab.
pub fn sets_session_state(query: &str) -> bool {
    let words = leading_keywords(query, 4);
    let at = |i: usize| words.get(i).map(String::as_str);
    let by_keyword = match at(0) {
        Some("SET") => !matches!(
            at(1),
            Some("LOCAL" | "TRANSACTION" | "CONSTRAINTS" | "CLUSTER")
        ),
        Some("USE") => true,
        Some("CREATE") => {
            matches!(at(1), Some("TEMP" | "TEMPORARY"))
                || (matches!(at(1), Some("GLOBAL" | "LOCAL"))
                    && matches!(at(2), Some("TEMP" | "TEMPORARY")))
        }
        _ => false,
    };
    by_keyword || query.to_ascii_lowercase().contains("set_config")
}

/// Statements CockroachDB cannot run inside an explicit transaction, or that
/// should not be wrapped in one: schema changes, `SET CLUSTER SETTING`,
/// `BACKUP` / `RESTORE` / `IMPORT` unless `DETACHED`, and database creation
/// and removal. A host that asks the driver to wrap a batch in a transaction
/// runs these outside the wrap, in autocommit.
pub fn runs_outside_transaction(query: &str) -> bool {
    let words = leading_keywords(query, 3);
    let at = |i: usize| words.get(i).map(String::as_str);
    match at(0) {
        Some("CREATE" | "ALTER" | "DROP" | "TRUNCATE" | "COMMENT" | "REINDEX") => true,
        Some("SET") => at(1) == Some("CLUSTER"),
        Some("BACKUP" | "RESTORE" | "IMPORT") => !is_detached(query),
        _ => false,
    }
}

fn is_detached(query: &str) -> bool {
    let upper = query.to_ascii_uppercase();
    upper
        .split(|c: char| !c.is_ascii_alphanumeric() && c != '_')
        .any(|w| w == "DETACHED")
}

#[cfg(test)]
#[path = "sqlkind_tests.rs"]
mod tests;
