# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Fixed

- Stop dropping terminal events (`PtyWrite`, `Exit`, ...) when the backend event channel is full, without blocking the GUI thread

## [0.8.0]

### Changed

- Upgrade `tokio` to `1.50.0`
- Upgrade `anyhow` to `1.0.102`
- Revise CI workflows and release pipeline
- Synchronize terminal focus state with mouse clicks inside and outside the widget and gate input handling on the current focus state

### Added

- Expose backend `Command` as `iced_term::BackendCommand` together with required public types for external usage (thanks [@clicman](https://github.com/clicman))
- Allow scrolling a terminal on hover (thanks [@SarthakSingh31](https://github.com/SarthakSingh31))
- Add support for macOS backspace emulation (thanks [@SarthakSingh31](https://github.com/SarthakSingh31))
- Add a `focus` example that demonstrates `TerminalView` working alongside the iced text editor

## [0.7.0]

### Changed

- (**breaking changes**) Now the Terminal's method `subscription` returns `Subscription<Event>` instead of `impl Stream<Item = Event>`
- Upgrade `iced` to `0.14.0`
- Upgrade `iced_graphics` to `0.14.0`
- Upgrade `iced_core` to `0.14.0`
- Upgrade `tokio` to `1.49.0`
- Satisfy clippy lint about implicit lifetimes (thanks [@2bndy5](https://github.com/2bndy5))
- revise CI (thanks [@2bndy5](https://github.com/2bndy5))

### Added

- Allow to specify environment variables and working directory in `BackendSettings` (thanks [@ben-hansske](https://github.com/ben-hansske))
- Add badges in `README.md` (thanks [@2bndy5](https://github.com/2bndy5))
- Enable dependabot (thanks [@2bndy5](https://github.com/2bndy5))

### Fixed

- Get examples working on Windows (thanks [@2bndy5](https://github.com/2bndy5))

## [0.6.0]

### Added

- Improvements of text rendering (bold, italic, underlined, etc).
- Batch background rendering
- Shell arguments (thanks [@fdavid-spk](https://github.com/fdavid-spk))
- CHANGELOG.md
- `msgcat --color=test` [results](./docs/colortest)

### Changed

- (**breaking changes**) `iced_term::Event::BackendCommandReceived` -> `iced_term::Event::BackendCall`
- (**breaking changes**) `iced_term::Subscription` -> `Terminal's` method `subscription`
- (**breaking changes**) `iced_term::Command::ProcessBackendCommand` -> `iced_term::Command::ProxyToBackend`
- Upgrade `alacritty_terminal` to `0.25.0`
- Upgrade `tokio` to `1.47.1`
- Upgrade `anyhow` to `1.0.99`
- Upgrade `open` to `5.3.2`

### Fixed

- Initial font loading
- Showing cursor only if terminal mode is `SHOW_CURSOR`
- Examples using the new widget api
- Typos in README.md (thanks [@kxxt](https://github.com/kxxt))

### Removed

- (**breaking changes**) `iced_term::Subscription`

[unreleased]: https://github.com/Harzu/iced_term/compare/0.8.0...HEAD
[0.8.0]: https://github.com/Harzu/iced_term/compare/0.7.0...0.8.0
[0.7.0]: https://github.com/Harzu/iced_term/compare/0.6.0...0.7.0
[0.6.0]: https://github.com/Harzu/iced_term/compare/0.5.1...0.6.0
