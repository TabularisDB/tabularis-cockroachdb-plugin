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

/// A statement whose grammar accepts a trailing `LIMIT`/`OFFSET`.
/// `SHOW` and `CALL` do not (`SHOW x LIMIT 5` is a syntax error), so they are
/// capped client-side instead.
pub fn supports_trailing_limit(query: &str) -> bool {
    matches!(
        first_keyword(query).as_deref(),
        Some("SELECT" | "WITH" | "VALUES" | "TABLE")
    )
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

/// A session `SET` (not `SET LOCAL`, not `SET TRANSACTION`), `USE`, or a
/// temporary table: state that lives on the connection, so the connection
/// must be reset before it goes back to the pool.
pub fn sets_session_state(query: &str) -> bool {
    let words = leading_keywords(query, 4);
    let at = |i: usize| words.get(i).map(String::as_str);
    match at(0) {
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
    }
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
