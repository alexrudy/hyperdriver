use std::{
    ops::{Deref, DerefMut},
    sync::Arc,
};
use tokio::sync::RwLock;

use crate::stream::info::{ConnectionInfo, SocketAddr, TLSConnectionInfo};

#[derive(Debug)]
enum State {
    Pending(tokio::sync::oneshot::Receiver<TLSConnectionInfo>),
    Received(ConnectionInfo),
}

#[derive(Debug, Clone)]
pub(crate) struct TlsConnectionInfoReciever {
    state: Arc<RwLock<State>>,
    info: ConnectionInfo,
}

impl TlsConnectionInfoReciever {
    pub(crate) fn new(
        inner: tokio::sync::oneshot::Receiver<TLSConnectionInfo>,
        info: ConnectionInfo,
    ) -> Self {
        Self {
            state: Arc::new(RwLock::new(State::Pending(inner))),
            info,
        }
    }

    pub(crate) fn local_addr(&self) -> Option<SocketAddr> {
        self.info.local_addr()
    }

    pub(crate) fn remote_addr(&self) -> Option<&SocketAddr> {
        self.info.remote_addr()
    }

    pub(crate) async fn recv(&self) -> ConnectionInfo {
        {
            let state = self.state.read().await;

            match state.deref() {
                State::Pending(_) => {}
                State::Received(info) => return info.clone(),
            };
        }

        let mut state = self.state.write().await;

        let rx = match state.deref_mut() {
            State::Pending(rx) => rx,
            State::Received(info) => {
                return info.clone();
            }
        };

        let tls = rx
            .await
            .expect("connection info was never sent and is not available");

        let info = self.info.clone().with_tls(tls);

        *state = State::Received(info.clone());
        info
    }
}
