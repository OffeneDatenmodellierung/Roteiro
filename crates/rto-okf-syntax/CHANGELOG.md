# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

## [0.1.1](https://github.com/OffeneDatenmodellierung/Roteiro/compare/rto-okf-syntax-v0.1.0...rto-okf-syntax-v0.1.1) - 2026-09-07

### Other

- Merge pull request #763 from OffeneDatenmodellierung/dependabot/cargo/sqlparser-0.62.0
- *(deps)* bump sqlparser from 0.58.0 to 0.62.0

## [0.1.0](https://github.com/OffeneDatenmodellierung/Roteiro/releases/tag/rto-okf-syntax-v0.1.0) - 2026-09-02

### Added

- *(okf)* add `roteiro okf syntax`, and give rto-okf-syntax a consumer
- *(okf)* add rto-okf-syntax, a pure-Rust core with optional parsers

### Fixed

- *(okf-syntax)* a real line, a real bug, and two false rejections
- *(okf-syntax)* SQL is sqlparser's, because the grammar fails the real corpus
