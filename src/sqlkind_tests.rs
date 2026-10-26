use super::*;

fn effect(q: &str) -> TransactionEffect {
    transaction_effect(q)
}

#[test]
fn keywords_skip_comments_and_stop_at_literals() {
    assert_eq!(leading_keywords("  select 1", 2), ["SELECT", "1"]);
    assert_eq!(leading_keywords("-- hi\nSELECT 1", 1), ["SELECT"]);
    assert_eq!(
        leading_keywords("/* a /* nested */ b */ begin", 1),
        ["BEGIN"]
    );
    assert_eq!(leading_keywords("SELECT 'BEGIN'", 3), ["SELECT"]);
    assert_eq!(leading_keywords("\"begin\"", 1), Vec::<String>::new());
    assert_eq!(leading_keywords("", 2), Vec::<String>::new());
    assert_eq!(leading_keywords("begin;", 3), ["BEGIN"]);
}

#[test]
fn limit_is_only_appended_where_the_grammar_allows() {
    for q in [
        "SELECT 1",
        "select * from t",
        "WITH a AS (SELECT 1) SELECT * FROM a",
        "VALUES (1)",
        "TABLE t",
        "-- c\nSELECT 1",
        "/* c */ select 1",
    ] {
        assert!(supports_trailing_limit(q), "{q}");
    }
    for q in [
        "SHOW TABLES",
        "CALL p()",
        "EXPLAIN SELECT 1",
        "INSERT INTO t VALUES (1)",
        "UPDATE t SET a = 1",
        "DROP TABLE t",
        "",
    ] {
        assert!(!supports_trailing_limit(q), "{q}");
    }
}

#[test]
fn opening_statements() {
    for q in [
        "BEGIN",
        "begin",
        "BEGIN TRANSACTION",
        "BEGIN READ WRITE",
        "BEGIN ISOLATION LEVEL SERIALIZABLE",
        "START TRANSACTION",
        "/* x */ BEGIN",
        "-- x\nBEGIN;",
    ] {
        assert_eq!(effect(q), TransactionEffect::Opens, "{q}");
    }
}

#[test]
fn closing_statements() {
    for q in [
        "COMMIT",
        "commit",
        "END",
        "ROLLBACK",
        "ABORT",
        "COMMIT TRANSACTION",
        "ROLLBACK WORK",
        "END TRANSACTION",
    ] {
        assert_eq!(effect(q), TransactionEffect::Closes, "{q}");
    }
}

#[test]
fn chaining_and_savepoints() {
    assert_eq!(effect("COMMIT AND CHAIN"), TransactionEffect::Chains);
    assert_eq!(effect("ROLLBACK AND CHAIN"), TransactionEffect::Chains);
    assert_eq!(effect("COMMIT AND NO CHAIN"), TransactionEffect::Closes);
    assert_eq!(
        effect("ROLLBACK TO SAVEPOINT cockroach_restart"),
        TransactionEffect::None
    );
    assert_eq!(
        effect("ROLLBACK TO cockroach_restart"),
        TransactionEffect::None
    );
    assert_eq!(effect("SAVEPOINT a"), TransactionEffect::None);
    assert_eq!(effect("RELEASE SAVEPOINT a"), TransactionEffect::None);
}

#[test]
fn transaction_words_in_data_do_not_count() {
    assert_eq!(effect("SELECT 'BEGIN'"), TransactionEffect::None);
    assert_eq!(
        effect("INSERT INTO t VALUES ('COMMIT')"),
        TransactionEffect::None
    );
    assert_eq!(effect("SELECT 1 -- BEGIN"), TransactionEffect::None);
    assert_eq!(
        effect("CREATE FUNCTION f() RETURNS INT LANGUAGE SQL AS $$ SELECT 1 $$"),
        TransactionEffect::None
    );
    assert_eq!(effect(""), TransactionEffect::None);
}

#[test]
fn state_after_a_statement() {
    use TransactionEffect::*;
    assert!(Opens.in_transaction_after(true, false));
    assert!(
        !Opens.in_transaction_after(false, false),
        "failed BEGIN opened nothing"
    );
    assert!(!Closes.in_transaction_after(true, true));
    assert!(
        !Closes.in_transaction_after(false, true),
        "a failed COMMIT still ends it"
    );
    assert!(Chains.in_transaction_after(true, true));
    assert!(!Chains.in_transaction_after(false, true));
    assert!(None.in_transaction_after(true, true));
    assert!(!None.in_transaction_after(true, false));
    assert!(
        None.in_transaction_after(false, true),
        "an error leaves it open (aborted)"
    );
}

#[test]
fn session_state_statements() {
    for q in [
        "SET application_name = 'x'",
        "set search_path = a",
        "SET SESSION x = 1",
        "SET ROLE admin",
        "USE db",
        "CREATE TEMP TABLE t (a INT)",
        "CREATE TEMPORARY TABLE t (a INT)",
        "CREATE LOCAL TEMP TABLE t (a INT)",
    ] {
        assert!(sets_session_state(q), "{q}");
    }
    for q in [
        "SET LOCAL x = 1",
        "SET TRANSACTION READ ONLY",
        "SET CLUSTER SETTING a = 1",
        "SELECT 1",
        "CREATE TABLE t (a INT)",
        "RESET ALL",
    ] {
        assert!(!sets_session_state(q), "{q}");
    }
}

#[test]
fn autocommit_only_statements() {
    for q in [
        "CREATE TABLE t (a INT)",
        "create table t (a int)",
        "ALTER TABLE t ADD COLUMN b INT",
        "DROP TABLE t",
        "DROP INDEX t@i",
        "CREATE INDEX CONCURRENTLY i ON t (a)",
        "TRUNCATE t",
        "COMMENT ON TABLE t IS 'x'",
        "CREATE DATABASE d",
        "DROP DATABASE d",
        "SET CLUSTER SETTING sql.defaults.x = true",
        "BACKUP INTO 'nodelocal://1/b'",
        "RESTORE FROM LATEST IN 'x'",
        "IMPORT INTO t CSV DATA ('x')",
        "/* c */ ALTER TABLE t DROP COLUMN b",
        "-- c\nDROP TABLE t",
    ] {
        assert!(runs_outside_transaction(q), "{q}");
    }
    for q in [
        "SELECT 1",
        "INSERT INTO t VALUES (1)",
        "UPDATE t SET a = 1",
        "DELETE FROM t",
        "SET search_path = a",
        "BEGIN",
        "COMMIT",
        "BACKUP INTO 'x' WITH detached",
        "RESTORE FROM LATEST IN 'x' WITH DETACHED",
        "SHOW TABLES",
    ] {
        assert!(!runs_outside_transaction(q), "{q}");
    }
}

fn main_kw(q: &str) -> Option<String> {
    with_main_keyword(q)
}

#[test]
fn with_main_keyword_finds_the_statement_after_the_ctes() {
    assert_eq!(
        main_kw("WITH s AS (SELECT 1) SELECT * FROM s").as_deref(),
        Some("SELECT")
    );
    assert_eq!(
        main_kw("with s as (select 1) insert into t select * from s").as_deref(),
        Some("INSERT")
    );
    assert_eq!(
        main_kw("WITH s AS (SELECT 1) UPDATE t SET a = 1").as_deref(),
        Some("UPDATE")
    );
    assert_eq!(
        main_kw("WITH s AS (SELECT 1) DELETE FROM t").as_deref(),
        Some("DELETE")
    );
    assert_eq!(
        main_kw("WITH s AS (SELECT 1) UPSERT INTO t SELECT * FROM s").as_deref(),
        Some("UPSERT")
    );
    assert_eq!(
        main_kw("WITH s AS (SELECT 1) VALUES (1)").as_deref(),
        Some("VALUES")
    );
}

#[test]
fn with_main_keyword_handles_every_cte_form() {
    // several CTEs, column lists, RECURSIVE, [NOT] MATERIALIZED, quoted names
    assert_eq!(
        main_kw("WITH a AS (SELECT 1), b AS (SELECT 2) SELECT 1").as_deref(),
        Some("SELECT")
    );
    assert_eq!(
        main_kw("WITH a (x, y) AS (SELECT 1, 2) INSERT INTO t SELECT x FROM a").as_deref(),
        Some("INSERT")
    );
    assert_eq!(main_kw("WITH RECURSIVE r(n) AS (SELECT 1 UNION ALL SELECT n + 1 FROM r WHERE n < 5) INSERT INTO t SELECT n FROM r").as_deref(), Some("INSERT"));
    assert_eq!(
        main_kw("WITH a AS MATERIALIZED (SELECT 1) DELETE FROM t").as_deref(),
        Some("DELETE")
    );
    assert_eq!(
        main_kw("WITH a AS NOT MATERIALIZED (SELECT 1) SELECT 1").as_deref(),
        Some("SELECT")
    );
    assert_eq!(
        main_kw("WITH \"my cte\" AS (SELECT 1) INSERT INTO t VALUES (1)").as_deref(),
        Some("INSERT")
    );
}

#[test]
fn with_main_keyword_is_not_fooled_by_literals_comments_or_nesting() {
    // parentheses and keywords inside strings, comments and nested subqueries
    assert_eq!(
        main_kw("WITH s AS (SELECT ')' AS p) INSERT INTO t SELECT 1").as_deref(),
        Some("INSERT")
    );
    assert_eq!(
        main_kw("WITH s AS (SELECT '(' ) INSERT INTO t SELECT 1").as_deref(),
        Some("INSERT")
    );
    assert_eq!(
        main_kw("WITH s AS (SELECT 'it''s )' ) DELETE FROM t").as_deref(),
        Some("DELETE")
    );
    assert_eq!(
        main_kw("WITH s AS (SELECT E'a\\')b') UPDATE t SET a = 1").as_deref(),
        Some("UPDATE")
    );
    assert_eq!(
        main_kw("WITH s AS (SELECT $$ ) INSERT $$) SELECT 1").as_deref(),
        Some("SELECT")
    );
    assert_eq!(
        main_kw("WITH s AS (SELECT $q$ ) $$ $q$) DELETE FROM t").as_deref(),
        Some("DELETE")
    );
    assert_eq!(
        main_kw("WITH s AS (SELECT 1 /* ) INSERT */) SELECT 1").as_deref(),
        Some("SELECT")
    );
    assert_eq!(
        main_kw("WITH s AS (SELECT 1 -- ) INSERT\n) SELECT 1").as_deref(),
        Some("SELECT")
    );
    assert_eq!(
        main_kw("WITH s AS (SELECT (SELECT (1))) INSERT INTO t SELECT 1").as_deref(),
        Some("INSERT")
    );
    assert_eq!(
        main_kw("/* c */ WITH s AS (SELECT \"a)\" FROM t) DELETE FROM t").as_deref(),
        Some("DELETE")
    );
    assert_eq!(
        main_kw("WITH s AS (SELECT $1) SELECT 1").as_deref(),
        Some("SELECT"),
        "placeholder, not a dollar quote"
    );
}

#[test]
fn with_main_keyword_gives_up_on_anything_it_cannot_parse() {
    for q in [
        "SELECT 1",
        "WITH",
        "WITH a",
        "WITH a AS",
        "WITH a AS (SELECT 1",
        "WITH a AS SELECT 1",
        "WITH a AS (SELECT 1) ",
        "WITH (SELECT 1) SELECT 1",
        "WITH a AS (SELECT 1),",
    ] {
        assert_eq!(main_kw(q), None, "{q}");
    }
}

#[test]
fn a_cte_write_is_never_paginated() {
    // The reported bug: `WITH ... INSERT` got `LIMIT n` appended to its source
    // query and silently wrote fewer rows.
    for q in [
        "WITH src AS (SELECT generate_series(1,5) AS n) INSERT INTO t SELECT n FROM src",
        "WITH src AS (SELECT 1) INSERT INTO t SELECT * FROM src RETURNING *",
        "WITH src AS (SELECT 1) UPDATE t SET a = 1 FROM src",
        "WITH src AS (SELECT 1) UPDATE t SET a = 1 RETURNING a",
        "WITH src AS (SELECT 1) DELETE FROM t USING src",
        "WITH src AS (SELECT 1) DELETE FROM t RETURNING id",
        "WITH src AS (SELECT 1) UPSERT INTO t SELECT * FROM src",
        "with src as (select 1) insert into t select * from src",
        "-- c\nWITH src AS (SELECT 1) INSERT INTO t SELECT * FROM src",
        "WITH a AS (SELECT 1), b AS (SELECT 2) INSERT INTO t SELECT * FROM a, b",
        "WITH RECURSIVE r(n) AS (SELECT 1 UNION ALL SELECT n + 1 FROM r WHERE n < 5) INSERT INTO t SELECT n FROM r",
    ] {
        assert!(!supports_trailing_limit(q), "{q}");
    }
}

#[test]
fn unparseable_with_statements_are_not_paginated() {
    for q in [
        "WITH",
        "WITH a AS (SELECT 1",
        "WITH a AS SELECT 1 INSERT INTO t VALUES (1)",
    ] {
        assert!(!supports_trailing_limit(q), "{q}");
    }
}

#[test]
fn a_read_only_cte_is_still_paginated() {
    for q in [
        "WITH a AS (SELECT 1) SELECT * FROM a",
        "WITH a AS (SELECT 1), b AS (SELECT 2) SELECT * FROM a, b ORDER BY 1",
        "WITH RECURSIVE r(n) AS (SELECT 1 UNION ALL SELECT n + 1 FROM r WHERE n < 5) SELECT n FROM r",
        "WITH a AS (SELECT 1) VALUES (1)",
        "/* c */ with a as (select 1) select * from a",
        // A data-modifying CTE always runs to completion; the LIMIT only trims the outer SELECT.
        "WITH ins AS (INSERT INTO t VALUES (1) RETURNING *) SELECT * FROM ins",
        "WITH del AS (DELETE FROM t RETURNING id) SELECT id FROM del",
    ] {
        assert!(supports_trailing_limit(q), "{q}");
    }
}
