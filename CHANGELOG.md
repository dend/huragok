# Changelog

All notable changes to Huragok are documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

## [0.1.0] - 2026-07-30

### Added

- Free-flying camera with adjustable speed and field of view, toggled with `INSERT`.
- Keyframe camera paths: record points along a shot and play them back as a smooth move.
- In-game control panel built on Dear ImGui, opened with `Ctrl+B`.
- Skull checklist covering the campaign skulls, including third-person view and a night
  vision toggle that turns fully back off.
- Time controls for slow motion, speed up, and pause.
- Live mission readout in the panel: current mission, difficulty, checkpoint, and framerate.
- In-panel console with a command input line and scrollable history.
- `Ctrl`-based hotkeys throughout, so shortcuts never fire while you type.
- `install.ps1` installer that finds a Steam install of the game, places the proxy, and
  copies the mod into the `mods` folder.
- GitHub Actions workflows that build the DLL on every push and publish a packaged
  release from a version tag.

[Unreleased]: https://github.com/dend/huragok/compare/0.1.0...HEAD
[0.1.0]: https://github.com/dend/huragok/releases/tag/0.1.0
