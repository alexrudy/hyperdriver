# Changelog
All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

## [0.6.1](https://github.com/alexrudy/hyperdriver/compare/v0.6.0...v0.6.1) - 2024-08-13

### Other
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
