//! Unit tests for `pagination.rs`.

use crate::utils::pagination::{
    build_paginated_query, limit_offset, paginate_query, wrap_paginated_query,
};

fn page(q: &str, size: u32, page: u32) -> String {
    build_paginated_query(q, size, page).unwrap_or_else(|| panic!("not paginated: {q}"))
}

#[test]
fn limit_offset_computes_zero_based_offset_from_one_indexed_page() {
    assert_eq!(limit_offset(1, 10), (10, 0));
    assert_eq!(limit_offset(2, 10), (10, 10));
    assert_eq!(limit_offset(3, 5), (5, 10));
    assert_eq!(limit_offset(0, 10), (10, 0), "page 0 reads as page 1");
}

#[test]
fn limit_offset_does_not_overflow() {
    assert_eq!(limit_offset(u32::MAX, u32::MAX).1, u32::MAX);
}

#[test]
fn appends_limit_offset_when_none_present() {
    assert_eq!(
        page("SELECT * FROM t ORDER BY id", 10, 1),
        "SELECT * FROM t ORDER BY id LIMIT 11 OFFSET 0"
    );
    assert_eq!(
        page("SELECT * FROM t ORDER BY id", 10, 2),
        "SELECT * FROM t ORDER BY id LIMIT 11 OFFSET 10"
    );
}

#[test]
fn replaces_an_existing_trailing_limit() {
    // Without removing it there would be two LIMIT clauses, a syntax error.
    let sql = page("SELECT * FROM t ORDER BY id LIMIT 5", 100, 1);
    assert_eq!(sql.matches("LIMIT").count(), 1, "{sql}");
}

#[test]
fn a_user_limit_caps_the_total_across_pages() {
    assert_eq!(
        page("SELECT * FROM t ORDER BY id LIMIT 5", 100, 1),
        "SELECT * FROM t ORDER BY id LIMIT 5 OFFSET 0"
    );
    // LIMIT 5, page size 2, page 3: offset 4, one row left.
    assert_eq!(
        page("SELECT * FROM t ORDER BY id LIMIT 5", 2, 3),
        "SELECT * FROM t ORDER BY id LIMIT 1 OFFSET 4"
    );
    // Past the cap: nothing left.
    assert_eq!(
        page("SELECT * FROM t ORDER BY id LIMIT 5", 2, 4),
        "SELECT * FROM t ORDER BY id LIMIT 0 OFFSET 6"
    );
}

#[test]
fn user_offset_is_kept_and_added_to_the_page_offset() {
    assert_eq!(
        page("SELECT * FROM t ORDER BY id LIMIT 5 OFFSET 3", 100, 1),
        "SELECT * FROM t ORDER BY id LIMIT 5 OFFSET 3"
    );
    assert_eq!(
        page("SELECT * FROM t ORDER BY id OFFSET 3", 10, 2),
        "SELECT * FROM t ORDER BY id LIMIT 11 OFFSET 13"
    );
}

#[test]
fn offset_before_limit_is_understood_too() {
    assert_eq!(
        page("SELECT * FROM t OFFSET 3 LIMIT 5", 100, 1),
        "SELECT * FROM t LIMIT 5 OFFSET 3"
    );
}

#[test]
fn trailing_semicolons_are_dropped() {
    assert_eq!(
        page("SELECT * FROM t;", 10, 1),
        "SELECT * FROM t LIMIT 11 OFFSET 0"
    );
    assert_eq!(
        page("SELECT * FROM t ;; ", 10, 1),
        "SELECT * FROM t LIMIT 11 OFFSET 0"
    );
    assert_eq!(
        page("SELECT * FROM t LIMIT 5;", 10, 1),
        "SELECT * FROM t LIMIT 5 OFFSET 0"
    );
}

// ---- Issue #9: comments --------------------------------------------------

#[test]
fn a_trailing_line_comment_cannot_swallow_the_limit_clause() {
    // Reported case 1: `-- note` used to comment out the appended LIMIT, so
    // page 2 repeated page 1.
    let q = "SELECT generate_series(1,25) AS n ORDER BY 1 -- note";
    assert_eq!(
        page(q, 10, 1),
        "SELECT generate_series(1,25) AS n ORDER BY 1 LIMIT 11 OFFSET 0"
    );
    assert_eq!(
        page(q, 10, 2),
        "SELECT generate_series(1,25) AS n ORDER BY 1 LIMIT 11 OFFSET 10"
    );
    assert!(!page(q, 10, 2).contains("note"));
}

#[test]
fn a_block_comment_after_a_user_limit_does_not_hide_it() {
    // Reported case 2: the second LIMIT made the server reject the SQL.
    let sql = page(
        "SELECT generate_series(1,25) AS n ORDER BY 1 LIMIT 2 /* note */",
        10,
        1,
    );
    assert_eq!(
        sql,
        "SELECT generate_series(1,25) AS n ORDER BY 1 LIMIT 2 OFFSET 0"
    );
    assert_eq!(sql.matches("LIMIT").count(), 1);
}

#[test]
fn comments_around_and_between_the_clauses_are_handled() {
    assert_eq!(
        page("SELECT * FROM t LIMIT 5 -- cap", 10, 1),
        "SELECT * FROM t LIMIT 5 OFFSET 0"
    );
    assert_eq!(
        page("SELECT * FROM t LIMIT 5 OFFSET 3 -- skip\n", 10, 1),
        "SELECT * FROM t LIMIT 5 OFFSET 3"
    );
    assert_eq!(
        page("SELECT * FROM t LIMIT /* c */ 5", 10, 1),
        "SELECT * FROM t LIMIT 5 OFFSET 0"
    );
    assert_eq!(
        page(
            "SELECT * FROM t /* a */ LIMIT 5 /* b */ OFFSET 3 /* c */",
            10,
            1
        ),
        "SELECT * FROM t LIMIT 5 OFFSET 3"
    );
    assert_eq!(
        page("SELECT * FROM t -- a\n -- b\n", 10, 2),
        "SELECT * FROM t LIMIT 11 OFFSET 10"
    );
    assert_eq!(
        page("SELECT * FROM t -- note\n;", 10, 1),
        "SELECT * FROM t LIMIT 11 OFFSET 0"
    );
    assert_eq!(
        page("SELECT * FROM t; -- note", 10, 1),
        "SELECT * FROM t LIMIT 11 OFFSET 0"
    );
}

#[test]
fn comments_inside_the_statement_are_kept() {
    let sql = page(
        "SELECT a, -- first\n b FROM t /* hint */ WHERE a > 1 ORDER BY a -- end",
        10,
        1,
    );
    assert_eq!(
        sql,
        "SELECT a, -- first\n b FROM t /* hint */ WHERE a > 1 ORDER BY a LIMIT 11 OFFSET 0"
    );
}

#[test]
fn a_leading_comment_is_kept() {
    assert_eq!(
        page("-- header\nSELECT 1", 10, 1),
        "-- header\nSELECT 1 LIMIT 11 OFFSET 0"
    );
}

#[test]
fn limit_inside_a_comment_is_not_a_limit() {
    assert_eq!(
        page("SELECT * FROM t /* LIMIT 5 */", 10, 1),
        "SELECT * FROM t LIMIT 11 OFFSET 0"
    );
    assert_eq!(
        page("SELECT * FROM t -- LIMIT 5", 10, 1),
        "SELECT * FROM t LIMIT 11 OFFSET 0"
    );
}

// ---- Strings and identifiers --------------------------------------------

#[test]
fn limit_words_inside_strings_and_identifiers_are_preserved() {
    assert_eq!(
        page("SELECT 'LIMIT 5' AS \"limit\" FROM t", 10, 1),
        "SELECT 'LIMIT 5' AS \"limit\" FROM t LIMIT 11 OFFSET 0"
    );
    assert_eq!(
        page("SELECT * FROM t WHERE note = 'x LIMIT 3'", 10, 1),
        "SELECT * FROM t WHERE note = 'x LIMIT 3' LIMIT 11 OFFSET 0"
    );
    assert_eq!(
        page("SELECT * FROM t WHERE a = 'it''s -- not a comment'", 10, 1),
        "SELECT * FROM t WHERE a = 'it''s -- not a comment' LIMIT 11 OFFSET 0"
    );
    assert_eq!(
        page("SELECT $$ LIMIT 3 -- $$ FROM t", 10, 1),
        "SELECT $$ LIMIT 3 -- $$ FROM t LIMIT 11 OFFSET 0"
    );
    assert_eq!(
        page("SELECT * FROM \"offset\" LIMIT 4", 10, 1),
        "SELECT * FROM \"offset\" LIMIT 4 OFFSET 0"
    );
}

#[test]
fn a_trailing_string_ending_the_statement_is_not_mistaken_for_a_limit() {
    // The last tokens are `LIMIT` and a string, not `LIMIT` and a number.
    let q = "SELECT * FROM t WHERE a = 'LIMIT 5'";
    assert_eq!(page(q, 10, 1), format!("{q} LIMIT 11 OFFSET 0"));
}

#[test]
fn limits_in_subqueries_are_left_alone() {
    assert_eq!(
        page("SELECT * FROM (SELECT * FROM t LIMIT 3) AS s", 10, 1),
        "SELECT * FROM (SELECT * FROM t LIMIT 3) AS s LIMIT 11 OFFSET 0"
    );
    assert_eq!(
        page("(SELECT 1 LIMIT 1) UNION ALL (SELECT 2 LIMIT 1)", 10, 1),
        "(SELECT 1 LIMIT 1) UNION ALL (SELECT 2 LIMIT 1) LIMIT 11 OFFSET 0"
    );
}

// ---- Forms that cannot be read, and statements that cannot be rewritten -----

#[test]
fn unreadable_limit_clauses_fall_back_to_wrapping() {
    for q in [
        "SELECT * FROM t ORDER BY id LIMIT ALL",
        "SELECT * FROM t ORDER BY id LIMIT $1",
        "SELECT * FROM t ORDER BY id LIMIT 1+1",
        "SELECT * FROM t ORDER BY id OFFSET 2 ROWS",
        "SELECT * FROM t ORDER BY id FETCH FIRST 3 ROWS ONLY",
    ] {
        assert_eq!(build_paginated_query(q, 10, 2), None, "{q}");
        let sql = paginate_query(q, 10, 2).unwrap();
        assert!(sql.starts_with("SELECT * FROM (\n"), "{sql}");
        assert!(
            sql.ends_with("\n) AS tabularis_page LIMIT 11 OFFSET 10"),
            "{sql}"
        );
        assert!(sql.contains(q), "{sql}");
    }
}

#[test]
fn a_wrapped_statement_cannot_leak_a_comment_into_the_clause() {
    let sql = wrap_paginated_query("SELECT 1 LIMIT ALL -- note", 10, 1).unwrap();
    assert_eq!(
        sql,
        "SELECT * FROM (\nSELECT 1 LIMIT ALL\n) AS tabularis_page LIMIT 11 OFFSET 0"
    );
}

#[test]
fn a_limit_too_large_for_u32_is_wrapped_not_misread() {
    let q = "SELECT * FROM t LIMIT 99999999999999";
    assert_eq!(build_paginated_query(q, 10, 1), None);
    assert!(paginate_query(q, 10, 1).unwrap().contains("tabularis_page"));
}

#[test]
fn statements_that_cannot_be_rewritten_safely_are_left_alone() {
    for q in [
        "SELECT 'unterminated",
        "SELECT 1 /* never closed",
        "SELECT \"never closed",
        "SELECT 1; SELECT 2",
        "",
        "   ",
        "-- only a comment",
        ";",
    ] {
        assert_eq!(paginate_query(q, 10, 1), None, "{q:?}");
    }
}

#[test]
fn multibyte_text_is_cut_on_character_boundaries() {
    assert_eq!(
        page("SELECT 'héllo wörld' AS ñ ORDER BY 1 -- ünï", 10, 1),
        "SELECT 'héllo wörld' AS ñ ORDER BY 1 LIMIT 11 OFFSET 0"
    );
}
