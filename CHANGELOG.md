# Changelog

All notable changes to GoodWe Status are listed here, newest first. The format follows [Keep a Changelog](https://keepachangelog.com/en/1.1.0/) and the project uses [Semantic Versioning](https://semver.org/).

## [Unreleased]

### Added
- While the battery charges, a green dot pulses before "Charging" in the panel, and the line under the battery shows about how long until it is full (for example "~1h 10m to full at 3.8 kW"). The dot stays still with Reduce motion on, or while the data is old.

## [0.2.2] - 2026-10-21

### Changed
- Your Tuya devices load once when the app starts, and then only when you click ↻ in Settings › Automation. Opening Settings or switching tabs no longer contacts Tuya.
- After you press ↻ or ⌘R in the panel and the new data arrives, the panel replays its opening motion once, so you can see it refreshed. Background updates stay still, and Reduce motion turns it off.
- Settings › Automation's intro line is reworded ("When the grid goes off or comes back, each device that's on switches after its delay.") and always keeps two lines of space, so the device list no longer jumps when you turn a device on or off.

### Fixed
- Settings › Automation's ↻ now visibly refreshes: it spins while it loads, and it re-reads the device list. New names show, devices added in Tuya appear in Add Device…, and a device removed from Tuya keeps its rules and says "Not in your Tuya account".
- Each device's dot now shows its online status: green when online, red when offline or when a change failed, and yellow while it waits out its delay. A failed change stays red until the next refresh finds the device online; it stays in the log.

## [0.2.1] - 2026-10-20

### Added
- Settings › General: a Reduce motion switch. It turns off the opening motion, the pane fade and the window glide.
- Settings › Automation: a Refresh device list button beside Add Device…, and a status dot before each device.

### Changed
- The panel, Settings and the sign-in box have a new look: a new colour palette in light and dark, and the panel is a solid card with rounded corners.
- In light mode, switches, buttons and focus rings follow your Mac's accent colour. If the colour is too pale to read, the app keeps its blue.
- The battery shows 20 small cells coloured by how full it is, and both temperatures sit on one line.
- When the grid is on and there is no back-on estimate, the panel says "Since 10:05 AM" under Grid on. The cut line is shorter: "Cut usually around 9:10 AM".
- The chart, battery and cut bars slide in when the panel opens.
- Settings panes are a switch at the top, and the window glides to each pane's height. The log folder moved to About.

## [0.2.0] - 2026-10-20

### Added
- Settings › About: the version, a link to the project on GitHub, and updates. The app looks for a new release on GitHub each time it opens (Check Now looks again). When there is one, the panel's gear and the About tab get a blue dot, and the gear opens About with what's new. Update and Relaunch downloads it, checks its signature, installs it in place and relaunches; nothing downloads before you click. This is the first version that can update itself; from 0.1.x, install it by hand once.

### Changed
- The bottom of the panel is one row: a gear for Settings (⌘,) on the left, the last update time in the middle, and a power button to quit (⌘Q) on the right. The separate "Updated" line is gone, and while a refresh runs the middle says "Refreshing…". The panel is shorter as a result.
- The panel's power-cut strip is now the way into Stats: click it (or Tab to it and press Return or Space) to open Settings › Stats on today. The ⓘ after "Usually back around" is gone.
- "Usually back around …" stays once its time has passed while the grid is still off, so you can see the grid is later than usual; it goes when the grid comes back. Before, it disappeared or jumped to a later time.
- Settings › Automation also says what turning a device on does while the grid is on ("The grid is on now: turning a device on sets its Grid on choice."), not only while it is off.
- Every part of the power-cut strip (in the panel and in Stats) now has a card like the red cuts: point at it, or Tab, then ← →, to see when the grid was on, or when SEMS had no data, with the times and how long. The hours after now have none.
- Settings › About: the Source code row now shows the GitHub mark beside its label, in the text colour in light and dark.
- When GoodWe or Tuya stops answering or changes its replies, the log now says which call failed, what came back and what the app did. It also says when a reading or today's history stops moving, and when it comes back. The README section "When GoodWe changes their API" lists these lines.

### Fixed
- Power cuts show again in the panel and in Stats. Since 19 October SEMS sends no grid reading during a cut, so the app saw no cut and history looked stuck; it now reads cuts without that reading, and fetches days saved without their cuts once more. A cut is also drawn when the app starts while the grid is already off.
- When the grid goes off, "Usually back around …" shows at once, and the cut shows on the strip and in Stats › today. Before, all three waited a few minutes until the inverter's minute history had the cut.
- Device automation: a change in Settings › Automation now reaches the device at once while the grid is on too. Turning a device on, adding it, or changing its Grid on choice sets it right away (before, only grid-off changes did, so testing with the grid on looked like nothing worked).

## [0.1.2] - 2026-10-19

### Added
- Today's power cuts as a 24-hour strip at the bottom of the panel's Grid section (the same strip as in Stats; point at a cut, or Tab, then ← →, to read it). Turn it off in Settings › General › Show today's power cuts.
- An ⓘ at the end of "Usually back around …" opens Settings › Stats on today.

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

[Unreleased]: https://github.com/asifshirazi/goodwe-status/compare/v0.2.0...HEAD
[0.2.0]: https://github.com/asifshirazi/goodwe-status/compare/v0.1.2...v0.2.0
[0.1.2]: https://github.com/asifshirazi/goodwe-status/compare/v0.1.1...v0.1.2
[0.1.1]: https://github.com/asifshirazi/goodwe-status/compare/v0.1.0...v0.1.1
[0.1.0]: https://github.com/asifshirazi/goodwe-status/releases/tag/v0.1.0
