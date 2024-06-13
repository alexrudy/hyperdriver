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
//! - [`TcpConnector`]: Connects to a remote server over TCP/IP. This is the default transport, and what
//!     usually powers HTTP connections.
//! - [`DuplexTransport`]: Connects to a remote server over a duplex stream, which
//!     is an in-memory stream that can be used for testing or other purposes.
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

pub mod dns;
pub(crate) mod protocol;
pub(crate) mod transport;

pub use self::protocol::http::{ConnectionError, HttpConnectionBuilder};
pub use self::protocol::{Connection, Protocol, ProtocolRequest};
#[cfg(feature = "stream")]
pub use self::transport::duplex::DuplexTransport;
#[cfg(feature = "stream")]
pub use self::transport::stream::{IntoStream, TransportExt};
#[cfg(feature = "tls")]
pub use self::transport::tls::TlsTransportWrapper;
pub use self::transport::{TlsTransport, TransportTlsExt};

pub use self::transport::tcp::{TcpConnectionConfig, TcpConnector};
pub use self::transport::{Transport, TransportStream};
