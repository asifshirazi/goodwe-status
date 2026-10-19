# Changelog

All notable changes to GoodWe Status are listed here, newest first. The format follows [Keep a Changelog](https://keepachangelog.com/en/1.1.0/) and the project uses [Semantic Versioning](https://semver.org/).

## [Unreleased]

## [0.1.0] - 2026-10-19

First release.

### Added
- Your battery % and grid state in the menu bar. Click it to open a panel with today's battery chart.
- An alert with a sound when the grid goes off or comes back.
- Sign in through a small box. Your login is kept in your Keychain. The app starts when you log in.
- The panel shows when the grid went off, when power usually comes back, or when the next cut usually starts (from the last 5 days).
- Point at the chart (or Tab, then the arrow keys) to read the time, battery % and kW at any minute.
- A Refresh button at the top right that keeps the panel open (also ⌘R).
- Settings (⌘,) with a General tab (your account and Sign Out, Open at login, the log folder, the low battery level) and an Automation tab. The panel footer is just Settings… and Quit (plus Sign In… when signed out).
- Device automation: Tuya / Smart Life devices (lights, fan switches and more) can change by themselves when the grid goes off or comes back, each after its own delay, with its own on/off switch.
- A low battery alert while the grid is off (20% and 10% by default, set in Settings › General).

### Changed
- Times show in 12-hour format with AM/PM, for example "Since 1:36 PM" and chart labels "6 AM · 12 PM".
- "Grid on" is shown in green, and the chart no longer has an explanation line under it.
- The battery line no longer repeats the "as of" time while data is old.
- Clearer placeholder text in the sign-in and settings boxes.

### Fixed
- The grid-off time is now right even after a restart, because it comes from the inverter's own minute history.
- Past days' history loads more reliably and is kept across restarts.
- Alerts and device changes no longer wait for the past days' history to load.
- The panel no longer opens with Settings… highlighted.
- The Settings window no longer jumps on the first click.

[Unreleased]: https://github.com/asifshirazi/goodwe-status/compare/v0.1.0...HEAD
[0.1.0]: https://github.com/asifshirazi/goodwe-status/releases/tag/v0.1.0
