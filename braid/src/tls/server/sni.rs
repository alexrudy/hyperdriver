use std::future::ready;

use address::Authority;
use axum::{body::Full, body::HttpBody};
use futures::{
    future::{Either, MapOk},
    TryFutureExt,
};
use hyper::{header, Request, Response, StatusCode};
use thiserror::Error;
use tower::{Layer, Service};
use tracing::{dispatcher, field};

use crate::info::ConnectionInfo;

// Clippy is unhappy here, but this is not mutable! It just might be backed by a Bytes,
// where the type system can't tell that the backing is static.
#[allow(clippy::declare_interior_mutable_const)]
const XOUTCOME: http::header::HeaderName = http::header::HeaderName::from_static("x-outcome");

#[derive(Debug, Error)]
pub enum ValidateSNIError {
    #[error("TLS SNI \"{sni}\" does not match HOST header \"{host}\"")]
    InvalidSNI { host: String, sni: String },

    #[error("TLS SNI was not provided when requesting \"{host}\"")]
    MissingSNI { host: String },
}

pub struct ValidateSNI;

impl<S> Layer<S> for ValidateSNI {
    type Service = ValidateSNIService<S>;

    fn layer(&self, inner: S) -> Self::Service {
        ValidateSNIService::new(inner)
    }
}

#[derive(Debug)]
pub struct ValidateSNIService<S> {
    inner: S,
}

impl<S> ValidateSNIService<S> {
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
    BOut: HttpBody + Into<arnold::Body> + Send + 'static,
{
    type Response = Response<arnold::Body>;
    type Error = S::Error;
    type Future = Either<
        std::future::Ready<Result<Self::Response, Self::Error>>,
        MapOk<S::Future, fn(Response<BOut>) -> Response<arnold::Body>>,
    >;

    fn poll_ready(
        &mut self,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Result<(), Self::Error>> {
        self.inner.poll_ready(cx)
    }

    fn call(&mut self, mut req: Request<BIn>) -> Self::Future {
        let span =
            tracing::debug_span!("SNI Verification", host = field::Empty, sni = field::Empty);
        dispatcher::get_default(|dispatch| {
            let id = span.id().expect("Missing ID; this is a bug");
            if let Some(current) = dispatch.current_span().id() {
                dispatch.record_follows_from(&id, current)
            }
        });
        match span.in_scope(|| handle(&mut req)) {
            Some(error) => span.in_scope(|| {
                let body = Full::from(error.to_string());
                let content_length = body.size_hint().exact();
                let mut res = Response::new(body.into());
                *res.status_mut() = StatusCode::BAD_REQUEST;
                let headers = res.headers_mut();
                headers.insert(
                    header::CONTENT_TYPE,
                    header::HeaderValue::from_static("text/plain"),
                );
                if let Some(content_length) = content_length {
                    headers.insert(
                        header::CONTENT_LENGTH,
                        header::HeaderValue::from(content_length),
                    );
                }
                headers.insert(XOUTCOME, header::HeaderValue::from_static("mark"));

                Either::Left(ready(Ok(res)))
            }),

            None => Either::Right(self.inner.call(req).map_ok(|r| r.map(|b| b.into()))),
        }
    }
}

fn handle<BIn>(req: &mut Request<BIn>) -> Option<ValidateSNIError> {
    let span = tracing::Span::current();

    // Grab (and own) the host header value
    let host: Option<Authority> = req
        .headers()
        .get(header::HOST)
        .and_then(|h| h.to_str().ok())
        .and_then(|s| s.parse().ok());

    // Grab the TLS connection info
    let info = req
        .extensions_mut()
        .get_mut::<ConnectionInfo>()
        .expect("Missing connection info extension - misconfiguration?");

    let tcp = match info {
        ConnectionInfo::Tcp(tcp) => tcp,
        _ => {
            panic!("SNI verification not supported for non-tcp connections")
        }
    };

    let tls = tcp.tls.as_mut().expect("SNI verification requires TLS");
    span.record("sni", tls.sni.as_deref().unwrap_or("-"));

    if let Some(sni) = tls.sni.as_ref().and_then(|s| s.parse::<Authority>().ok()) {
        if let Some(host) = host {
            span.record("host", host.to_string());
            if host.host() != sni.host() {
                tracing::warn!(header=%host, expected=%sni, "Rejecting request with mismatched SNI and Host");
                return Some(ValidateSNIError::InvalidSNI {
                    host: host.to_string(),
                    sni: sni.to_string(),
                });
            } else {
                tls.validated_sni();
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
