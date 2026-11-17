# Changelog

All notable changes to this project are documented here. The format is based on
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and this project uses [Semantic Versioning](https://semver.org/).

## [Unreleased]

## [0.2.0] - 2026-11-17

### Added
- Ajazz AJ179 / AJ179P and AJ159P (PAW3950 version) mice, over the 2.4 GHz receiver or the cable. Shown faded with
  their last known level while the mouse sleeps. Other Ajazz models use different protocols and aren't supported.

## [0.1.0] - 2026-11-17

First public release.

### Added
- Tray icon showing the battery of your wireless devices: by default the device with the lowest battery, as a device
  symbol with a coloured level bar. Hover for every device's exact percentage.
- Menu: pick which device the icon shows, icon style (device + level bar, device filled by level, device + percentage,
  percentage only), one icon per device, refresh, start with Windows.
- Low-battery notification at 15%.
- Device support:
  - Razer wireless mice and keyboards (tested: Viper V3 Pro)
  - Xbox controllers via XInput (tested: Xbox One S controller over Bluetooth)
  - Sony DualSense / DualSense Edge
  - Logitech HID++ 2.0 mice and keyboards (Lightspeed, Unifying, Bolt) and G-series headsets, including
    G PRO X 2 / G522 / G PRO X 3 LIGHTSPEED
  - SteelSeries Arctis and Arctis Nova headsets
  - HyperX Cloud II Wireless, Cloud Alpha (2) Wireless, Cloud Flight
  - Corsair VOID-family and Bragi headsets (Virtuoso, VOID V2, HS80 MAX)
  - Any Bluetooth device whose battery Windows shows in Settings
- Diagnostics for bug reports: `--probe --verbose`, `--list-hid`.

[Unreleased]: https://github.com/RobertMRR/Battery-Monitor/compare/v0.2.0...HEAD
[0.2.0]: https://github.com/RobertMRR/Battery-Monitor/compare/v0.1.0...v0.2.0
[0.1.0]: https://github.com/RobertMRR/Battery-Monitor/releases/tag/v0.1.0
