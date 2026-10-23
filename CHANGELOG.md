# Changelog

All notable changes to this project are documented in this file. The format
is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and this
project adheres to [Semantic Versioning](https://semver.org/).

Each release needs a `## [X.Y.Z]` section before its tag is pushed: the
release workflow publishes that section as the GitHub release notes.

## [Unreleased]

### Added

- Project scaffold generated with `@tabularis/create-plugin`: JSON-RPC stdio
  loop, stub handlers, local REPL, and `justfile`.
- `.tabularium` manifest with CockroachDB metadata, icon, capabilities, data
  types, type mappings, and plugin settings.
- CI (tests, clippy, rustfmt, manifest validation, markdownlint, cargo-audit,
  live CockroachDB job), release, and version-suggestion workflows.
- Architecture documents in `docs/architecture`.
