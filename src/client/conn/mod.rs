//! Client connection types.
//!
//! A client connection is composed of a transport, a protocol, and a connection, which each serve a
//! different purpose in the client connection lifecycle.
//!
//! ## Transport
//!
//! The transport is responsible for establishing a connection to a remote server, shuffling bytes back
//! and forth, and handling the low-level details of the connection. Transports implement the [`Transport`]
//! trait, effecitively making them a service which accepts a URI and returns a bidirectional stream.
//!
//! Two builtin transports are provided:
//! - [`transport::tcp::TcpTransport`]: Connects to a remote server over TCP/IP. This is the default transport, and what
//!     usually powers HTTP connections.
//! - [`transport::duplex::DuplexTransport`]: Connects to a remote server over a duplex stream, which
//!     is an in-memory stream that can be used for testing or other purposes.
//!
//! ## Stream
//!
//! The stream is a bidirectional stream of bytes, which is used to send and receive data over the connection.
//! The stream is a low-level abstraction, and is used by the transport to send and receive data.
//!
//! ## Protocol
//!
//! The protocol is responsible for encoding and decoding request and response objects. Usually, this means
//! HTTP/1.1 or HTTP/2, but it could be any protocol which sends and receives data over a connection.
//!
//! Protocols implement the [`Protocol`] trait, which is a service that accepts a [`ProtocolRequest`] and
//! returns a connection. The connection is responsible for sending and receiving HTTP requests and responses.
//!
//! ## Connection
//!
//! The connection is responsible for sending and receiving HTTP requests and responses. It is the highest
//! level of abstraction in the client connection stack, and is the  part of the stack which accepts requests.
//!
//! Connections implement the [`Connection`] trait, which is a service that accepts a request and returns a
//! future which resolves to a response. The connection is responsible for encoding and decoding the request
//! and response objects, and for sending and receiving the data over the transport.

pub mod connection;
pub mod dns;
pub mod protocol;
pub mod stream;
pub mod transport;

pub use self::connection::Connection;
pub use self::protocol::{Protocol, ProtocolRequest};
pub use self::stream::Stream;
pub use self::transport::{TlsTransport, TransportExt as _};
pub use self::transport::{Transport, TransportStream};
