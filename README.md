# hyperdriver: Tools and libraries which help out [hyper](https://hyper.is/)

[![crate][crate-image]][crate-link]
[![Docs][docs-image]][docs-link]
[![Build Status][build-image]][build-link]
![MIT licensed][license-image]

This crate exists to fill the missing middle between `hyper` and full-fledged frameworks
like `axum`. Crates like `axum` provide servers, and crates like `reqwest` provide clients,
but both are specific to what they do. `hyperdriver` provides a set of services and tools
which can be used to build both servers and clients in a more flexible way.

If you want to control the protocol, or the transport (e.g. using something other than TCP)
then `hyperdriver` is for you.

## Features

- Server with graceful shutdown, HTTP/2 and TLS support.
- Client with HTTP/2 and TLS support.
- Streams which can dispatch between TCP, Unix domain, and in-process duplex sockets.
- A unifying Body type to make building small Clients and Servers easier.
- Bridge between Tokio and Hyper, similar to `hyper-utils`.

[crate-image]: https://img.shields.io/crates/v/hyperdriver
[crate-link]: https://crates.io/crates/hyperdriver
[docs-image]: https://docs.rs/hyperdriver/badge.svg
[docs-link]: https://docs.rs/hyperdriver/
[build-image]: https://github.com/alexrudy/hyperdriver/actions/workflows/ci.yml/badge.svg
[build-link]: https://github.com/alexrudy/hyperdriver/actions/workflows/ci.yml
[license-image]: https://img.shields.io/badge/license-MIT-blue.svg
