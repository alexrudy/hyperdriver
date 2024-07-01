//! Middleware to validate that SNI matches the HOST header using Braid's connection info

use std::future::ready;

use futures_util::{
    future::{Either, MapErr},
    TryFutureExt,
};
use http::uri::Authority;
use hyper::{header, Request, Response};
use thiserror::Error;
use tower::{Layer, Service};

use crate::info::TlsConnectionInfo;

/// Error returned by the SNI Middleware.
#[derive(Debug, Error)]
pub enum SNIMiddlewareError<E>
where
    E: std::error::Error,
{
    /// An error occurred in the inner service.
    #[error(transparent)]
    Inner(E),

    /// An error occurred while validating the SNI.
    #[error(transparent)]
    SNI(#[from] ValidateSNIError),
}

impl<E> SNIMiddlewareError<E>
where
    E: std::error::Error,
{
    fn inner(error: E) -> Self {
        Self::Inner(error)
    }
}

/// Error returned when validating the SNI from TLS connection information.
#[derive(Debug, Error, PartialEq, Eq)]
pub enum ValidateSNIError {
    /// The SNI did not match the host header.
    #[error("TLS SNI \"{sni}\" does not match HOST header \"{host}\"")]
    InvalidSNI {
        /// The value of the HOST header
        host: String,

        /// The value of the SNI extension
        sni: String,
    },

    /// The SNI was not provided.
    #[error("TLS SNI was not provided when requesting \"{host}\"")]
    MissingSNI {
        /// The value of the Host header
        host: String,
    },
}

/// Middleware layer to validate that SNI matches the HOST header
///
/// This helps to prevent a client from connecting to the wrong host by
/// validating a certificate against the SNI extension of a different host.
#[derive(Debug, Clone, Default)]
pub struct ValidateSNI;

impl<S> Layer<S> for ValidateSNI {
    type Service = ValidateSNIService<S>;

    fn layer(&self, inner: S) -> Self::Service {
        ValidateSNIService::new(inner)
    }
}

/// Middleware service to validate that SNI matches the HOST header
///
/// This helps to prevent a client from connecting to the wrong host by
/// validating a certificate against the SNI extension of a different host.
#[derive(Debug)]
pub struct ValidateSNIService<S> {
    inner: S,
}

impl<S> ValidateSNIService<S> {
    /// Create a new `ValidateSNIService` wrapping `inner` service.
    pub fn new(inner: S) -> Self {
        Self { inner }
    }
}

impl<S> Clone for ValidateSNIService<S>
where
    S: Clone,
{
    fn clone(&self) -> Self {
        Self {
            inner: self.inner.clone(),
        }
    }
}

impl<S, BIn, BOut> Service<Request<BIn>> for ValidateSNIService<S>
where
    S: Service<Request<BIn>, Response = Response<BOut>>,
    S::Error: std::error::Error + Send + Sync + 'static,
{
    type Response = Response<BOut>;
    type Error = SNIMiddlewareError<S::Error>;
    type Future = Either<
        std::future::Ready<Result<Self::Response, Self::Error>>,
        MapErr<S::Future, fn(S::Error) -> Self::Error>,
    >;

    fn poll_ready(
        &mut self,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Result<(), Self::Error>> {
        self.inner.poll_ready(cx).map_err(SNIMiddlewareError::inner)
    }

    fn call(&mut self, mut req: Request<BIn>) -> Self::Future {
        match handle(&mut req) {
            Some(error) => Either::Left(ready(Err(SNIMiddlewareError::SNI(error)))),
            None => Either::Right(self.inner.call(req).map_err(SNIMiddlewareError::inner)),
        }
    }
}

fn handle<BIn>(req: &mut Request<BIn>) -> Option<ValidateSNIError> {
    let span = tracing::Span::current();

    // Grab (and own) the host header value
    let host: Option<Authority> = if req.version() == http::Version::HTTP_2 {
        req.uri().authority().cloned()
    } else {
        req.headers()
            .get(header::HOST)
            .and_then(|h| h.to_str().ok())
            .and_then(|s| s.parse().ok())
    };

    // Grab the TLS connection info
    let Some(tls) = req.extensions_mut().get_mut::<TlsConnectionInfo>() else {
        tracing::warn!("Request had no TLS connection info, inferring not a TLS request");
        return None;
    };

    if let Some(sni) = tls
        .server_name
        .as_ref()
        .and_then(|s| s.parse::<Authority>().ok())
    {
        if let Some(host) = host {
            span.record("host", host.to_string());
            if host.host() != sni.host() {
                tracing::warn!(header=%host, expected=%sni, "Rejecting request with mismatched SNI and Host");
                return Some(ValidateSNIError::InvalidSNI {
                    host: host.to_string(),
                    sni: sni.to_string(),
                });
            } else {
                tls.validated();
            }
        }
    } else {
        tracing::warn!(header=?host, "Rejecting request with missing SNI");
        return Some(ValidateSNIError::MissingSNI {
            host: host.map(|h| h.to_string()).unwrap_or_else(|| "-".into()),
        });
    }

    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_handle_no_sni() {
        let mut req = Request::new(());
        req.headers_mut()
            .insert(header::HOST, "example.com".parse().unwrap());
        req.extensions_mut().insert(TlsConnectionInfo {
            server_name: None,
            ..TlsConnectionInfo::default()
        });

        let error = handle(&mut req).unwrap();

        assert_eq!(
            error,
            ValidateSNIError::MissingSNI {
                host: "example.com".into()
            }
        );
    }

    #[test]
    fn test_handle_no_host() {
        let mut req = Request::new(());
        req.extensions_mut().insert(TlsConnectionInfo {
            server_name: Some("example.com".into()),
            ..TlsConnectionInfo::default()
        });

        assert!(handle(&mut req).is_none(), "Missing HOST should not error");
    }

    #[test]
    fn test_handle_mismatched_sni() {
        let mut req = Request::new(());
        req.headers_mut()
            .insert(header::HOST, "example.com".parse().unwrap());
        req.extensions_mut().insert(TlsConnectionInfo {
            server_name: Some("example.org".into()),
            ..TlsConnectionInfo::default()
        });

        let error = handle(&mut req).unwrap();

        assert_eq!(
            error,
            ValidateSNIError::InvalidSNI {
                host: "example.com".into(),
                sni: "example.org".into()
            }
        );
    }

    #[test]
    fn test_handle_matching_sni() {
        let mut req = Request::new(());
        req.headers_mut()
            .insert(header::HOST, "example.com".parse().unwrap());
        req.extensions_mut().insert(TlsConnectionInfo {
            server_name: Some("example.com".into()),
            ..TlsConnectionInfo::default()
        });

        assert!(handle(&mut req).is_none());
    }

    #[test]
    fn test_handle_http2_matches_authority() {
        let mut req = Request::builder()
            .uri("https://example.com")
            .body(())
            .unwrap();
        req.extensions_mut().insert(TlsConnectionInfo {
            server_name: Some("example.com".into()),
            ..TlsConnectionInfo::default()
        });

        assert!(handle(&mut req).is_none());
    }

    #[test]
    fn test_handle_missing_tls_info() {
        let mut req = Request::new(());
        req.headers_mut()
            .insert(header::HOST, "example.com".parse().unwrap());

        assert!(handle(&mut req).is_none());
    }
}
