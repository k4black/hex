# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

## [0.1.2](https://github.com/k4black/hex/compare/v0.1.1...v0.1.2)

### Fixed

- PyPI upload: no PEP 740 attestations, which PyPI rejects from a reusable workflow.

## [0.1.1](https://github.com/k4black/hex/compare/v0.1.0...v0.1.1)

### Fixed

- Publish to PyPI as `hex-agents-cli` (0.1.0 did not reach PyPI).
- Release workflow: grant the PyPI publish job `contents: read`.

## [0.1.0](https://github.com/k4black/hex/releases/tag/v0.1.0)

### Added

- First public release: the `hex` binary, built-in presets, and the Claude Code,
  Codex, Pi and opencode workers.
