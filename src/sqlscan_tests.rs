use super::*;

fn kinds(sql: &str) -> Vec<Kind> {
    scan(sql).tokens.into_iter().map(|t| t.kind).collect()
}

fn w(s: &str) -> Kind {
    Kind::Word(s.to_string())
}

#[test]
fn separates_words_and_punctuation() {
    assert_eq!(
        kinds("select a, b from (t);"),
        [
            w("SELECT"),
            w("A"),
            Kind::Comma,
            w("B"),
            w("FROM"),
            Kind::Open,
            w("T"),
            Kind::Close,
            Kind::Semi
        ]
    );
}

#[test]
fn records_byte_spans() {
    let sql = "SELECT é FROM t";
    let s = scan(sql);
    let from = s.tokens.iter().find(|t| t.kind == w("FROM")).unwrap();
    assert_eq!(&sql[from.start..from.end], "FROM");
    let last = s.tokens.last().unwrap();
    assert_eq!((last.start, last.end), (sql.len() - 1, sql.len()));
}

#[test]
fn line_comments_run_to_the_end_of_the_line() {
    let sql = "SELECT 1 -- note\nFROM t";
    let s = scan(sql);
    let c = s.tokens.iter().find(|t| t.kind == Kind::Comment).unwrap();
    assert_eq!(&sql[c.start..c.end], "-- note");
    assert!(s.complete);
    assert_eq!(s.tokens.last().unwrap().kind, w("T"));
}

#[test]
fn a_trailing_line_comment_with_no_newline_is_a_comment() {
    let s = scan("SELECT 1 -- note");
    assert!(s.complete);
    assert_eq!(s.tokens.last().unwrap().kind, Kind::Comment);
}

#[test]
fn block_comments_nest() {
    let sql = "SELECT /* a /* b */ still */ 1";
    let s = scan(sql);
    assert!(s.complete);
    assert_eq!(
        s.tokens.iter().filter(|t| t.kind == Kind::Comment).count(),
        1
    );
    assert_eq!(s.tokens.last().unwrap().kind, w("1"));
}

#[test]
fn keywords_inside_comments_are_not_words() {
    assert_eq!(
        kinds("SELECT 1 /* LIMIT 5 */"),
        [w("SELECT"), w("1"), Kind::Comment]
    );
    assert_eq!(
        kinds("SELECT 1 -- LIMIT 5"),
        [w("SELECT"), w("1"), Kind::Comment]
    );
}

#[test]
fn strings_and_quoted_identifiers_are_opaque() {
    assert_eq!(
        kinds("SELECT 'LIMIT 5; --' AS \"limit\""),
        [w("SELECT"), Kind::Str, w("AS"), Kind::Quoted]
    );
    assert_eq!(kinds("SELECT 'it''s )'"), [w("SELECT"), Kind::Str]);
    assert_eq!(kinds("SELECT \"a\"\"b\""), [w("SELECT"), Kind::Quoted]);
}

#[test]
fn escape_strings_honour_backslashes() {
    assert_eq!(
        kinds("SELECT E'a\\'b' , 1"),
        [w("SELECT"), Kind::Str, Kind::Comma, w("1")]
    );
    // The E prefix is part of the literal, not a word of its own.
    let sql = "SELECT E'x'";
    let s = scan(sql);
    assert_eq!(s.tokens.len(), 2);
    assert_eq!(&sql[s.tokens[1].start..s.tokens[1].end], "E'x'");
    // A plain string ending in a backslash is complete.
    assert!(scan("SELECT 'a\\'").complete);
    // `E` as the tail of a longer word is not an escape prefix.
    assert!(scan("SELECT name'x'").complete);
}

#[test]
fn dollar_quotes_hide_their_contents_and_placeholders_are_not_quotes() {
    assert_eq!(
        kinds("SELECT $$ ) ; -- $$ , 1"),
        [w("SELECT"), Kind::Str, Kind::Comma, w("1")]
    );
    assert_eq!(kinds("SELECT $q$ $$ ; $q$"), [w("SELECT"), Kind::Str]);
    assert_eq!(kinds("SELECT $1"), [w("SELECT"), Kind::Other, w("1")]);
    assert!(scan("SELECT $1, $2").complete);
}

#[test]
fn reports_unterminated_text() {
    for sql in [
        "SELECT 'abc",
        "SELECT \"abc",
        "SELECT 1 /* never closed",
        "SELECT $$ never",
        "SELECT E'a\\'",
    ] {
        assert!(!scan(sql).complete, "{sql}");
    }
    for sql in [
        "SELECT 'abc'",
        "SELECT \"abc\"",
        "SELECT 1 /* closed */",
        "SELECT $$ x $$",
        "",
    ] {
        assert!(scan(sql).complete, "{sql}");
    }
}

#[test]
fn empty_and_whitespace_only_input() {
    assert!(scan("").tokens.is_empty());
    assert!(scan("  \n\t ").tokens.is_empty());
}
