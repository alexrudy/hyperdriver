//! Middleware for setting the Host header of a request.
use http;
use http::uri::Port;
use http::HeaderValue;
use http::Uri;

use crate::client::conn::Connection;
use crate::client::pool::PoolableConnection;

use super::ExecuteRequest;

/// Returns true if the URI scheme is presumed secure.
fn is_schema_secure(uri: &Uri) -> bool {
    uri.scheme_str()
        .map(|scheme_str| matches!(scheme_str, "wss" | "https"))
        .unwrap_or_default()
}

/// Returns the port if it is not the default port for the scheme.
fn get_non_default_port(uri: &Uri) -> Option<Port<&str>> {
    match (uri.port().map(|p| p.as_u16()), is_schema_secure(uri)) {
        (Some(443), true) => None,
        (Some(80), false) => None,
        _ => uri.port(),
    }
}

/// Set the Host header on the request if it is not already set,
/// using the authority from the URI.
fn set_host_header<B>(request: &mut http::Request<B>) {
    if request.uri().host().is_none() {
        tracing::debug!(uri=%request.uri(), "request uri has no host");
        return;
    }

    let uri = request.uri().clone();

    request
        .headers_mut()
        .entry(http::header::HOST)
        .or_insert_with(|| {
            let hostname = uri.host().expect("authority implies host");
            if let Some(port) = get_non_default_port(&uri) {
                let s = format!("{}:{}", hostname, port);
                HeaderValue::from_str(&s)
            } else {
                HeaderValue::from_str(hostname)
            }
            .expect("uri host is valid header value")
        });
}

/// Middleware which sets the Host header on requests.
///
/// This middleware applies the host header when it is not already set
/// to requests with versions lower than HTTP/2. If a connection is also
/// passed (because this layer is used below the connection pool), it will
/// defer to the connection version rather than the request version to
/// determine if the host header should be set.
///
/// The `HOST` header is not set on HTTP/2 requests because the authority
/// is sent in the `:authority` pseudo-header field.
#[derive(Debug, Default, Clone)]
pub struct SetHostHeader<S> {
    inner: S,
}

impl<S, B> tower::Service<http::Request<B>> for SetHostHeader<S>
where
    S: tower::Service<http::Request<B>>,
{
    type Response = S::Response;

    type Error = S::Error;

    type Future = S::Future;

    fn poll_ready(
        &mut self,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Result<(), Self::Error>> {
        self.inner.poll_ready(cx)
    }

    fn call(&mut self, mut req: http::Request<B>) -> Self::Future {
        if req.version() < http::Version::HTTP_2 {
            set_host_header(&mut req);
        }

        self.inner.call(req)
    }
}

impl<S, B, C, K> tower::Service<ExecuteRequest<C, B, K>> for SetHostHeader<S>
where
    S: tower::Service<ExecuteRequest<C, B, K>>,
    C: Connection<B> + PoolableConnection,
    K: crate::client::pool::Key,
{
    type Response = S::Response;

    type Error = S::Error;

    type Future = S::Future;

    fn poll_ready(
        &mut self,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Result<(), Self::Error>> {
        self.inner.poll_ready(cx)
    }

    fn call(&mut self, mut req: ExecuteRequest<C, B, K>) -> Self::Future {
        if req.connection().version() < http::Version::HTTP_2 {
            set_host_header(req.request_mut());
        }

        self.inner.call(req)
    }
}

/// Layer for setting the Host header on requests.
#[derive(Debug, Default, Clone)]
pub struct SetHostHeaderLayer {
    _priv: (),
}

impl SetHostHeaderLayer {
    /// Create a new SetHostHeaderLayer.
    pub fn new() -> Self {
        Self { _priv: () }
    }
}

impl<S> tower::layer::Layer<S> for SetHostHeaderLayer {
    type Service = SetHostHeader<S>;

    fn layer(&self, inner: S) -> Self::Service {
        SetHostHeader { inner }
    }
}

#[cfg(test)]
mod tests {

    use super::*;

    #[test]
    fn test_set_host_header() {
        let mut request = http::Request::new(());
        *request.uri_mut() = "http://example.com".parse().unwrap();
        set_host_header(&mut request);
        assert_eq!(
            request.headers().get(http::header::HOST).unwrap(),
            "example.com"
        );

        let mut request = http::Request::new(());
        *request.uri_mut() = "http://example.com:8080".parse().unwrap();
        set_host_header(&mut request);
        assert_eq!(
            request.headers().get(http::header::HOST).unwrap(),
            "example.com:8080"
        );

        let mut request = http::Request::new(());
        *request.uri_mut() = "https://example.com".parse().unwrap();
        set_host_header(&mut request);
        assert_eq!(
            request.headers().get(http::header::HOST).unwrap(),
            "example.com"
        );

        let mut request = http::Request::new(());
        *request.uri_mut() = "https://example.com:8443".parse().unwrap();
        set_host_header(&mut request);
        assert_eq!(
            request.headers().get(http::header::HOST).unwrap(),
            "example.com:8443"
        );
    }

    #[test]
    fn test_is_schema_secure() {
        let uri = "http://example.com".parse().unwrap();
        assert!(!is_schema_secure(&uri));

        let uri = "https://example.com".parse().unwrap();
        assert!(is_schema_secure(&uri));

        let uri = "ws://example.com".parse().unwrap();
        assert!(!is_schema_secure(&uri));

        let uri = "wss://example.com".parse().unwrap();
        assert!(is_schema_secure(&uri));
    }

    #[test]
    fn test_get_non_default_port() {
        let uri = "http://example.com".parse().unwrap();
        assert_eq!(get_non_default_port(&uri).map(|p| p.as_u16()), None);

        let uri = "http://example.com:8080".parse().unwrap();
        assert_eq!(get_non_default_port(&uri).map(|p| p.as_u16()), Some(8080));

        let uri = "https://example.com".parse().unwrap();
        assert_eq!(get_non_default_port(&uri).map(|p| p.as_u16()), None);

        let uri = "https://example.com:8443".parse().unwrap();
        assert_eq!(get_non_default_port(&uri).map(|p| p.as_u16()), Some(8443));
    }
}
