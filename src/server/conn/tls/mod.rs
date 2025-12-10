//! Implementation of a server side TLS stream on top of tokio_rustls
//! which additionally provides connection information after the
//! handshake has been completed.

#[cfg(feature = "sni")]
pub mod sni;

pub use chateau::server::conn::tls::acceptor::TlsAcceptor;
pub use chateau::server::conn::tls::info::TlsConnectionInfoLayer;

#[cfg(all(test, feature = "client", feature = "stream"))]
mod tests {

    use std::convert::Infallible;

    use chateau::stream::tls::TlsHandshakeStream as _;
    use tower::make::Shared;
    use tower::Service;

    use tracing::Instrument as _;

    use crate::client::conn::HttpTlsTransport;
    use crate::fixtures;

    use chateau::client::conn::transport::duplex::DuplexTransport;
    use chateau::client::conn::Transport as _;

    use chateau::server::conn::AcceptExt as _;

    #[tokio::test]
    async fn tls_client_server() {
        let _ = tracing_subscriber::fmt::try_init();
        let _ = color_eyre::install();
        fixtures::tls_install_default();

        let _guard = tracing::info_span!("tls").entered();

        let service = tower::service_fn(|_: http::Request<crate::Body>| async {
            Ok::<_, Infallible>(http::Response::new(crate::Body::empty()))
        });

        let (client, incoming) = chateau::stream::duplex::pair();

        let acceptor = crate::server::conn::Acceptor::from(incoming)
            .with_tls(crate::fixtures::tls_server_config().into());

        let mut transport = HttpTlsTransport::new(
            DuplexTransport::new(1024, client),
            crate::fixtures::tls_client_config().into(),
        );

        let req = http::Request::get("https://example.com").body(()).unwrap();

        let client = async move {
            let mut stream = transport.connect(&req).await.unwrap();

            tracing::debug!("client connected");

            stream.finish_handshake().await.unwrap();

            tracing::debug!("client handshake finished");

            stream
        }
        .instrument(tracing::info_span!("client"));

        let server = async move {
            let mut conn = acceptor.accept().await.unwrap();

            tracing::debug!("server accepted");

            let mut make_service = Shared::new(service);

            let mut svc = Service::call(&mut make_service, &conn).await.unwrap();
            tracing::debug!("server connected");

            conn.finish_handshake().await.unwrap();
            tracing::debug!("server handshake finished");

            let _ = tower::Service::call(&mut svc, http::Request::new(crate::Body::empty()))
                .await
                .unwrap();

            tracing::debug!("server request handled");
            conn
        }
        .instrument(tracing::info_span!("server"));

        let (stream, conn) = tokio::join!(client, server);
        drop((stream, conn));
    }
}
