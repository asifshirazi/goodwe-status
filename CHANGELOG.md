# Changelog

All notable changes to GoodWe Status are listed here, newest first. The format follows [Keep a Changelog](https://keepachangelog.com/en/1.1.0/) and the project uses [Semantic Versioning](https://semver.org/).

## [Unreleased]

### Changed
- "Usually back around" and "Next cut" now look at the last 14 days instead of 5, so they show up more often, also after a few unusual days.

## [0.1.1] - 2026-10-19

### Added
- The panel shows the inverter and battery temperature, with Warm, Hot or Cold when a reading isn't fine and an arrow when it is rising or falling. A switch in Settings › General hides them.
- A Stats tab in Settings: one day's power cuts in a table (grid off, grid on, how long) under a strip of the day. Step days with ← / →, jump to today with ⌘T, or pick a date in the calendar.
- Every day's power-cut history is kept on this Mac, and the days since the station was set up load from SEMS once, in the background.

### Changed
- Settings › General: Account moved to the bottom, under a new Menu Bar group.
- The grid-off red is a little deeper, so it stays readable over any desktop picture.

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

[Unreleased]: https://github.com/asifshirazi/goodwe-status/compare/v0.1.1...HEAD
[0.1.1]: https://github.com/asifshirazi/goodwe-status/compare/v0.1.0...v0.1.1
[0.1.0]: https://github.com/asifshirazi/goodwe-status/releases/tag/v0.1.0
