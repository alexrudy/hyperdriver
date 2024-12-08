//! Hyperdriver
//!
//! Building the missing middle for network services in Rust.
//!
//! This crate is an alternative to [hyper-util][], providing a more fully-featured
//! interface for building Clients and Servers on top of the [hyper][] crate. It
//! is developed more speadily, but this means that it has a less stable interface
//! between minor versions than [hyper-util][], which is trying to carefully consider
//! some of the available design choices.
//!
//! This crate supports HTTP/1.1 and HTTP/2, as well as a pluggable transport mechanism
//! so that you can implement your own transports. Out of the box, it supports TCP,
//! Unix Domain Sockets, and in-memory byte streams, all with TLS support.
//!
//! The TLS support here should be additionally general enough to wrap any byte stream
//! which implements [`AsyncRead + AsyncWrite`][tokio::io], so you can use it with other libraries.
//!
//! # A tour
//!
//! Hyperdriver provides [`Client`] and [`Server`] types, which are the main entry points for
//! most high level use cases. The [`Client`] type is designed for making requests to servers,
//! and is similar to the one found in [reqwest][], but with a bit more pluggability and flexibility.
//!
//! The server is modeled after the features provided by the [hyper][] pre-1.0 server, and the [axum][]
//! server, but again with a more pluggable and composable design.
//!
//! Both are heavily reliant on the [tower][] [`tower::Service`] trait, and should compose well with
//! other tower-based services, such as those from [tower-http][].
//!
//! When you don't need that much customization, the defaults and regular implementations of both
//! [`Client`] and `Server` should be sufficient, and will help to avoid the type complexity that
//! comes with using layers of [`tower::Layer`]s.
//!
//! # Using [`tower::Service`] based types
//!
//! The [`tower::Service`] based types implemented for this crate are almost exclucively provided
//! in the [`crate::service`] module.
//!
//! When assembling a [`tower::Service`], it is often useful to do so with a [`tower::ServiceBuilder`]
//! and a collection of [`tower::Layer`]s. This is a common pattern in the tower ecosystem, and
//! hyperdriver is no exception. It is important to note that typing for these kinds of services
//! can be a bit tricky and unweildy. For instance, you can't dynamically decide to apply a [`Layer`](tower::Layer)
//! to a builder, you have to use an `Option` so that they type of the resulting layer is effectively
//! `Option<Layer>`. Alternatively, you can use [`tower::ServiceBuilder::boxed`] to create a boxed
//! service.
//!
//! Hyperdriver provides a helpful [`crate::service::OptionLayer`] type which can be used to wrap
//! layers without box-ing the error type (as the one from
//! [`tower` does](tower::ServiceBuilder::option_layer)). See
//! [`crate::service::OptionLayerExt`] for an ergomic way to use this with the [`tower::ServiceBuilder`]
//! type.
//!
//! ## Building Servers
//!
//! A server ends up being a "double service". Each [`http::Request`] is handled by a service, and returns
//! a [`http::Response`]. The server itself requires a service which can create a new service for each request,
//! which is often called a `MakeService` in the tower ecosystem. This two-layer design can be confusing.
//!
//! If your server's service is clone-able, you can use [`SharedService`](crate::service::SharedService) to wrap it.
//! This will mean that the [`Server`] will clone the service for each incoming connection. Otherwise, you should
//! implement a [`MakeService`](tower::MakeService), which is a service that accepts a reference to a connection
//! type, and returns an appropriate service for handling the ensuing request.
//!
//! See the [`crate::server`] module for more information on building servers.
//!
//! ## Building Clients
//!
//! A client can also be composed of services. In this case, there is only one "service", it must accept
//! a [`http::Request`] and return a [`http::Response`]. At the inner-most level, there is a connection step,
//! where the [`http::Request`] is coupled with a connection, to be used to send the request. At this point,
//! the service will switch from accepting a [`http::Request`] to accepting an [`ExecuteRequest`]
//! type, which includes the request and the connection.
//!
//! The default way to do this in `hyperdriver` is to use the [`crate::client::ConnectionPoolService`] type,
//! which implements connections and connection pooling. Many middleware services can be applied both above
//! (when the service only has an [`http::Request`]) and below (when the service has an [`ExecuteRequest`]) this
//! type. Some middleware might have slightly different behavior. For example, the [`SetHostHeader`] middleware
//! will apply the `Host` header to the request based on the version of the request if the connection is not
//! available yet. Usually this is not desired, since an HTTP/1.1 request might be internally upgraded to
//! HTTP/2, and the `Host` header should be removed. In this case, the [`SetHostHeader`]
//! middleware should be applied after the [`ConnectionPoolService`](crate::client::ConnectionPoolService).
//!
//! # Bodies
//!
//! The `hyper` ecosystem relies on the [`http_body::Body`] trait to represent the body of a request or response.
//! This is implemented for many types, and so can be tricky to unify. Many ecosystem crates ([axum][], [reqwest][],
//! [tonic][], etc.) implement their own body type, which sometimes can be used to encapsulate other bodies.
//!
//! Hyperdriver provides a [`Body`] type which is a wrapper around a [`http_body::Body`] type, but it is intentionally
//! minimal. If you need a unifying body type, you can reach for [`http_body_util::combinators::UnsyncBoxBody`] or
//! [`http_body_util::combinators::BoxBody`], which provide dynamic dispact bodies over any implementor of the [`http_body::Body`]
//! type. The [`Body`] is most useful for simple cases where you want to unify a request and response
//! body. `hyper` provides the [`hyper::body::Incoming`] type, which is what all incoming bodies are provided
//! as by both `hyper` and this crate. [`Body`] is a wrapper around this type and known, in memory
//! bodies, which can be convenient for simple services, but notably do not support streaming or chunked-encoded
//! bodies.
//!
//! [hyper]: https://docs.rs/hyper
//! [hyper-util]: https://docs.rs/hyper-util
//! [http]: https://docs.rs/http
//! [http-body]: https://docs.rs/http-body
//! [reqwest]: https://docs.rs/reqwest
//! [axum]: https://docs.rs/axum
//! [tower]: https://docs.rs/tower
//! [tower-http]: https://docs.rs/tower-http
//! [tonic]: https://docs.rs/tonic
//! [tokio::io]: https://docs.rs/tokio/1/tokio/io/index.html
//!
//! [`ExecuteRequest`]: crate::service::ExecuteRequest
//! [`SetHostHeader`]: crate::service::SetHostHeader

#![cfg_attr(docsrs, feature(doc_auto_cfg))]

use std::{fmt, future::Future, pin::Pin};

use tracing::dispatcher;

pub mod body;
pub mod bridge;
#[cfg(feature = "client")]
pub mod client;
#[cfg(feature = "client")]
pub(crate) mod happy_eyeballs;
pub mod info;
#[cfg(feature = "server")]
mod rewind;
#[cfg(feature = "server")]
pub mod server;
pub mod service;
pub mod stream;

pub use body::Body;
#[cfg(feature = "client")]
pub use client::Client;
#[cfg(feature = "server")]
pub use server::Server;

type BoxFuture<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;
type BoxError = Box<dyn std::error::Error + Send + Sync + 'static>;

/// Utility struct for formatting a `Display` type in a `Debug` context.
#[allow(unused)]
pub(crate) struct DebugLiteral<T: fmt::Display>(T);

impl<T: fmt::Display> fmt::Debug for DebugLiteral<T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}

/// Utility function for attaching a `follows_from` relationship to the current span
/// in a polling context.
#[allow(unused)]
#[track_caller]
pub(crate) fn polled_span(span: &tracing::Span) {
    dispatcher::get_default(|dispatch| {
        let id = span.id().expect("Missing ID; this is a bug");
        if let Some(current) = dispatch.current_span().id() {
            dispatch.record_follows_from(&id, current)
        }
    });
}

pub(crate) mod private {

    #[allow(unused)]
    pub trait Sealed<T> {}
}

/// Test fixtures for the `hyperdriver` crate.
#[cfg(test)]
#[cfg(feature = "tls")]
pub(crate) mod fixtures {

    use rustls::ServerConfig;

    pub(crate) fn tls_server_config() -> rustls::ServerConfig {
        let (_, cert) = pem_rfc7468::decode_vec(include_bytes!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/tests/minica/example.com/cert.pem"
        )))
        .unwrap();
        let (label, key) = pem_rfc7468::decode_vec(include_bytes!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/tests/minica/example.com/key.pem"
        )))
        .unwrap();

        let cert = rustls::pki_types::CertificateDer::from(cert);
        let key = match label {
            "PRIVATE KEY" => rustls::pki_types::PrivateKeyDer::Pkcs8(key.into()),
            "RSA PRIVATE KEY" => rustls::pki_types::PrivateKeyDer::Pkcs1(key.into()),
            "EC PRIVATE KEY" => rustls::pki_types::PrivateKeyDer::Sec1(key.into()),
            _ => panic!("unknown key type"),
        };

        let mut cfg = ServerConfig::builder()
            .with_no_client_auth()
            .with_single_cert(vec![cert], key)
            .unwrap();

        cfg.alpn_protocols.push(b"h2".to_vec());
        cfg.alpn_protocols.push(b"http/1.1".to_vec());

        cfg
    }

    fn tls_root_store() -> rustls::RootCertStore {
        let mut root_store = rustls::RootCertStore::empty();
        let (_, cert) = pem_rfc7468::decode_vec(include_bytes!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/tests/minica/minica.pem"
        )))
        .unwrap();
        root_store
            .add(rustls::pki_types::CertificateDer::from(cert))
            .unwrap();
        root_store
    }

    pub(crate) fn tls_client_config() -> rustls::ClientConfig {
        let mut config = rustls::ClientConfig::builder()
            .with_root_certificates(tls_root_store())
            .with_no_client_auth();
        config.alpn_protocols.push(b"h2".to_vec());
        config.alpn_protocols.push(b"http/1.1".to_vec());
        config
    }

    pub(crate) fn tls_install_default() {
        #[cfg(feature = "tls-ring")]
        {
            let _ = rustls::crypto::ring::default_provider().install_default();
        }

        #[cfg(all(feature = "tls-aws-lc", not(feature = "tls-ring")))]
        {
            let _ = rustls::crypto::aws_lc_rs::default_provider().install_default();
        }

        #[cfg(not(any(feature = "tls-aws-lc", feature = "tls-ring")))]
        {
            panic!("No TLS backend enabled, please enable one of `tls-ring` or `tls-aws-lc`");
        }
    }
}
