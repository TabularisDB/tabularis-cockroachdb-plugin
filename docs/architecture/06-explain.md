# 06 · EXPLAIN

## Variants

| Variant | SQL | Executes the statement |
| --- | --- | --- |
| Plan | `EXPLAIN <query>` | no |
| Analyze | `EXPLAIN ANALYZE <query>` | **yes** |

- Do not use `EXPLAIN (FORMAT JSON)`: CockroachDB does not support it.
- `EXPLAIN (VERBOSE)` adds columns and ordering per node. Use it for the plan
  variant if the extra detail is wanted; the parser handles both.
- `EXPLAIN (DISTSQL)` returns a diagram URL, `EXPLAIN (OPT)` an optimizer
  tree, `EXPLAIN ANALYZE (DEBUG)` a statement bundle. None of them map to the
  plan view; leave them to the SQL editor.
- `EXPLAIN ANALYZE` runs the statement. For anything other than `SELECT`,
  wrap it in `BEGIN; EXPLAIN ANALYZE ...; ROLLBACK;` so the user's data is
  not changed, unless the host already asked for that.

## Output format

`EXPLAIN` returns one text column (`info`), one row per line:

```text
distribution: local
vectorized: true

• filter
│ estimated row count: 33
│ filter: age > 30
│
└── • scan
      estimated row count: 100 (100% of the table; stats collected 3 minutes ago)
      table: users@users_pkey
      spans: FULL SCAN
```

`EXPLAIN ANALYZE` adds timings and actual counts:

```text
planning time: 412µs
execution time: 2ms
distribution: local
vectorized: true
rows decoded from KV: 100 (7.4 KiB, 1 gRPC calls)
maximum memory usage: 20 KiB
network usage: 0 B (0 messages)

• filter
│ nodes: n1
│ actual row count: 31
│ estimated row count: 33
│ filter: age > 30
│
└── • scan
      nodes: n1
      actual row count: 100
      KV time: 1ms
      KV rows decoded: 100
      estimated row count: 100 (100% of the table; stats collected 3 minutes ago)
      table: users@users_pkey
      spans: FULL SCAN
```

## Parsing

Parse in Rust and return Tabularis's **parsed plan shape**
(`root`, `planning_time_ms`, `execution_time_ms`, `has_analyze_data`, ...).
That works on every supported host without a frontend bundle. A plugin-owned
parser bundle (`explain_parsers` in the manifest, raw result shape) is an
option for later if the UI needs CockroachDB-specific rendering.

1. Join the rows with `\n`.
2. **Header lines**, before the first `•`, as `key: value`:
   - `planning time` → `planning_time_ms`
   - `execution time` → `execution_time_ms`
   - everything else (`distribution`, `vectorized`, `rows decoded from KV`,
     `maximum memory usage`, `network usage`, ...) → root node `extra`
3. **Node lines** contain `•`. The operation is the text after the `•` and its following space
   (`scan`, `filter`, `hash join`, `lookup join`, `render`, `sort`,
   `group (hash)`, `limit`, `index join`, `insert fast path`, ...).
   The depth is the character offset of the `•`. A node is the child of the
   nearest earlier node with a smaller offset.
4. **Attribute lines** follow a node. Strip the leading tree characters
   (` `, `│`, `├`, `└`, `─`) and split on the first colon followed by a space. Blank lines and
   lines that are only tree characters are skipped.

Attribute mapping onto the node:

| Attribute | Field |
| --- | --- |
| `estimated row count: 1,000 (...)` | `plan_rows` = 1000. Strip thousands separators; ignore the parenthesized part. |
| `actual row count: 31` | `actual_rows`; sets `has_analyze_data` |
| `execution time: 2ms` (per node, when present) | `actual_time_ms` |
| `table: users@users_pkey` | `relation` = `users`; `extra.index` = `users_pkey` |
| `filter: ...` | `filter` |
| `spans: ...`, `constraint: ...` | `index_condition` |
| `equality: (a) = (b)`, `left cols are key` | `hash_condition` / `extra` |
| `type: inner` (joins) | `join_type` |
| anything else | `extra[key] = value` |

Durations come as `412µs`, `412us`, `2ms`, `1.5s`, `1m2s`. Convert all of
them to milliseconds as `f64`.

Costs: CockroachDB does not print costs in `EXPLAIN`. `startup_cost` and
`total_cost` are `null`. Use `plan_rows` for the relative-width rendering.

Edge cases the parser must handle:

- An empty result, or headers with no node: return an error that names the
  query, not an empty tree.
- `• insert fast path` and other single-node plans.
- Subqueries and CTEs: separate top-level trees after a blank line, under a
  `• root` node, or prefixed with `subquery` / `postquery` lines. Attach them
  as children of a synthetic root so nothing is dropped. Verify on the CI
  matrix.
- Non-ASCII tree characters: offsets are character offsets, not byte
  offsets. `│` is three bytes in UTF-8.

Unit tests: fixtures for a scan, a filter over a scan, a hash join, an
`EXPLAIN ANALYZE` with timings, a plan with thousands separators, an empty
plan, headers only, and a subquery plan. Capture the fixtures from both CI
releases; the output gains fields between releases.
