# Changelog
All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

## [0.8.0](https://github.com/alexrudy/hyperdriver/compare/v0.7.0...v0.8.0) - 2024-10-09

### <!-- 0 -->⛰️ Features

- Client pool can delay drop for checkout
- Client now uses Body type instead of Incoming for response bodies

### <!-- 7 -->⚙️ Miscellaneous Tasks

- Make deprecated aliases visible

## [0.7.0](https://github.com/alexrudy/hyperdriver/compare/v0.6.0...v0.7.0) - 2024-10-01

### <!-- 0 -->⛰️ Features

- expose tcp and unix listeners in stream module
- Improved span tracing
- improved tracing for checkouts
- refactor connector
- Make the happy eyeballs algorithm default timeout 30s
- [**breaking**] remove `TransportStream` type.
- [**breaking**] remove PID file module, publish as separate crate
- feat!(discovery): remove discovery support
- *(body)* [**breaking**] Remove the TryCloneRequest trait from the body module.
- *(client)* [**breaking**] Remove support for retries from the client

### <!-- 1 -->🐛 Bug Fixes

- Ensure that services polled to readiness are used directly

### <!-- 7 -->⚙️ Miscellaneous Tasks

- update dependencies
- cargo-deny configuration tweaks
- Add cargo-deny to CI
- improve checkout docs
- Bump rustls-native-certs from 0.7.2 to 0.8.0
- fix docstrings so clippy in rust-1.82 is happy
- Bump webpki-roots from 0.26.3 to 0.26.5
- Bump tokio from 1.39.3 to 1.40.0
- Bump tower from 0.5.0 to 0.5.1
- Bump clap from 4.5.16 to 4.5.17
- remote httpbin tests, make them examples as a script
- make dependabot go in proper section in changelog
- add release-plz config to customize changelog
- cargo-machete ignore target/ directory
- Bump rustls-native-certs from 0.7.1 to 0.7.2
- Bump serde from 1.0.204 to 1.0.207
- Bump tempfile from 3.11.0 to 3.12.0
- Bump clap from 4.5.13 to 4.5.15

## [0.6.0](https://github.com/alexrudy/hyperdriver/compare/v0.5.6...v0.6.0) - 2024-08-11

### Other
- Improved main module documentation
- regularize imports for http:: crate and Body
- remove unused sevice.rs
- make the release CI pipeline named sensibly
- Adopt the release-plz action
- Merge pull request [#117](https://github.com/alexrudy/hyperdriver/pull/117) from alexrudy/feature/checkout-delayed-drop
- Merge pull request [#118](https://github.com/alexrudy/hyperdriver/pull/118) from alexrudy/feature/client-layers
- Split the client into service layers
- Remove BOut from Client Builder struct
- Rename generic parameters for HttpService impl
- Make client generic over body types
- Refine what Body does to a simpler subset
