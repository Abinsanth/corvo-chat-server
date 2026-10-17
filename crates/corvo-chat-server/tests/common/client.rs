//! A headless client for the integration tests: a server on a free port, a
//! WebSocket client that speaks the frames, and the client side of OPAQUE.

use std::{net::SocketAddr, time::Duration};

use corvo_chat_proto::{
    B64, PROTOCOL_VERSION,
    frames::{ClientFrame, ServerFrame},
    opaque::{CredFinal, CredRequest, CredResponse, RegRequest, RegResponse, RegUpload, Suite, recovery_ksf, wrap_key},
};
use corvo_chat_server::{AppState, app, limits::Limits};
use futures::{SinkExt, StreamExt};
use opaque_ke::{
    CipherSuite, ClientLogin, ClientLoginFinishParameters, ClientRegistration, ClientRegistrationFinishParameters,
    CredentialResponse, Identifiers, RegistrationResponse,
};
use rand_core::OsRng;
use rustls::pki_types::CertificateDer;
use tempfile::TempDir;
use tokio::{net::TcpStream, time::timeout};
use tokio_rustls::client::TlsStream;
use tokio_tungstenite::{WebSocketStream, tungstenite::Message};

use super::{connect, start, state};

/// How long `recv` and `closed` wait.
const WAIT: Duration = Duration::from_secs(5);

/// A running server and its state. Keeps the data directory alive.
pub struct TestServer {
    pub addr: SocketAddr,
    pub der: CertificateDer<'static>,
    pub state: AppState,
    _dir: TempDir,
}

/// A server with the default limits, except signups per IP: every test client
/// comes from 127.0.0.1.
pub async fn start_server() -> TestServer {
    start_server_with(Limits { signups_per_ip_day: 1_000, ..Limits::default() }).await
}

pub async fn start_server_with(limits: Limits) -> TestServer {
    let (state, dir) = state(limits);
    let (addr, der) = start(app(state.clone())).await;
    TestServer { addr, der, state, _dir: dir }
}

/// One WebSocket connection to `/ws`.
pub struct Client(WebSocketStream<TlsStream<TcpStream>>);

impl Client {
    pub async fn connect(server: &TestServer) -> Client {
        let tls = connect(server.addr, server.der.clone()).await;
        let (ws, _) =
            tokio_tungstenite::client_async("wss://localhost/ws", tls).await.expect("the WebSocket handshake succeeds");
        Client(ws)
    }

    pub async fn send(&mut self, frame: &ClientFrame) {
        let text = serde_json::to_string(frame).expect("frames serialize");
        self.send_raw(Message::text(text)).await;
    }

    pub async fn send_raw(&mut self, message: Message) {
        self.0.send(message).await.expect("the frame is sent");
    }

    /// The next frame. Panics after 5 s, or if the server closes.
    pub async fn recv(&mut self) -> ServerFrame {
        loop {
            let message = timeout(WAIT, self.0.next()).await.expect("a frame within 5 s");
            match message {
                Some(Ok(Message::Text(text))) => return serde_json::from_str(&text).expect("a server frame"),
                Some(Ok(Message::Ping(_) | Message::Pong(_))) => continue,
                other => panic!("expected a frame, got {other:?}"),
            }
        }
    }

    /// Whether the server closes the connection within 5 s.
    pub async fn closed(&mut self) -> bool {
        self.closed_within(WAIT).await
    }

    /// Whether the server closes the connection within `wait`. Frames before
    /// the close are skipped.
    pub async fn closed_within(&mut self, wait: Duration) -> bool {
        let closed = async {
            loop {
                match self.0.next().await {
                    None | Some(Err(_)) | Some(Ok(Message::Close(_))) => return,
                    Some(Ok(_)) => {}
                }
            }
        };
        timeout(wait, closed).await.is_ok()
    }

    /// Sends `hello` and expects `ready`.
    pub async fn hello(&mut self) {
        self.send(&ClientFrame::Hello { protocol_version: PROTOCOL_VERSION, app_version: "test".into() }).await;
        match self.recv().await {
            ServerFrame::Ready { .. } => {}
            other => panic!("expected ready, got {other:?}"),
        }
    }
}

/// The key stretching both OPAQUE `finish` steps use.
pub type Ksf = <Suite as CipherSuite>::Ksf;

fn b64<const N: usize>(bytes: impl AsRef<[u8]>) -> B64<N> {
    B64(bytes.as_ref().try_into().expect("OPAQUE messages have fixed sizes"))
}

pub fn register_start(secret: &str) -> (ClientRegistration<Suite>, RegRequest) {
    let start = ClientRegistration::<Suite>::start(&mut OsRng, secret.as_bytes()).expect("a registration request");
    (start.state, b64(start.message.serialize()))
}

/// Finishes a registration with the fast recovery-code stretching.
pub fn register_finish(state: ClientRegistration<Suite>, secret: &str, response: RegResponse) -> (RegUpload, [u8; 32]) {
    register_finish_with(&recovery_ksf(), state, secret, response)
}

pub fn register_finish_with(
    ksf: &Ksf,
    state: ClientRegistration<Suite>,
    secret: &str,
    response: RegResponse,
) -> (RegUpload, [u8; 32]) {
    let response = RegistrationResponse::<Suite>::deserialize(&response.0).expect("a registration response");
    let params = ClientRegistrationFinishParameters::new(Identifiers::default(), Some(ksf));
    let finish = state.finish(&mut OsRng, secret.as_bytes(), response, params).expect("the registration finishes");
    (b64(finish.message.serialize()), wrap_key(&finish.export_key))
}

pub fn login_start(secret: &str) -> (ClientLogin<Suite>, CredRequest) {
    let start = ClientLogin::<Suite>::start(&mut OsRng, secret.as_bytes()).expect("a credential request");
    (start.state, b64(start.message.serialize()))
}

/// Finishes a login with the fast recovery-code stretching. `None` for a
/// wrong secret (or an unknown username).
pub fn login_finish(state: ClientLogin<Suite>, secret: &str, response: CredResponse) -> Option<(CredFinal, [u8; 32])> {
    login_finish_with(&recovery_ksf(), state, secret, response)
}

pub fn login_finish_with(
    ksf: &Ksf,
    state: ClientLogin<Suite>,
    secret: &str,
    response: CredResponse,
) -> Option<(CredFinal, [u8; 32])> {
    let response = CredentialResponse::<Suite>::deserialize(&response.0).ok()?;
    let params = ClientLoginFinishParameters::new(None, Identifiers::default(), Some(ksf));
    let finish = state.finish(&mut OsRng, secret.as_bytes(), response, params).ok()?;
    Some((b64(finish.message.serialize()), wrap_key(&finish.export_key)))
}
