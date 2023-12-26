use std::{
    ops::{Deref, DerefMut},
    sync::Arc,
};
use tokio::sync::RwLock;

use crate::info::{ConnectionInfo, SocketAddr};

#[derive(Debug)]
enum State {
    Pending(tokio::sync::oneshot::Receiver<ConnectionInfo>),
    Received(ConnectionInfo),
}

#[derive(Debug, Clone)]
pub struct TlsConnectionInfoReciever {
    state: Arc<RwLock<State>>,
    peer_addr: SocketAddr,
    local_addr: SocketAddr,
}

impl TlsConnectionInfoReciever {
    pub fn new(
        inner: tokio::sync::oneshot::Receiver<ConnectionInfo>,
        peer_addr: SocketAddr,
        local_addr: SocketAddr,
    ) -> Self {
        Self {
            state: Arc::new(RwLock::new(State::Pending(inner))),
            peer_addr,
            local_addr,
        }
    }

    pub fn local_addr(&self) -> &SocketAddr {
        &self.local_addr
    }

    pub fn remote_addr(&self) -> &SocketAddr {
        &self.peer_addr
    }

    pub async fn recv(&self) -> ConnectionInfo {
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

        let info = rx
            .await
            .expect("connection info was never sent and is not available");
        *state = State::Received(info.clone());
        info
    }
}
