//! The WebSocket route: one task per connection, which starts with `hello`
//! and then moves through the states of [`Conn`] as frames arrive.

use std::net::IpAddr;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use axum::{
    Extension,
    extract::{
        State, WebSocketUpgrade,
        ws::{Message, WebSocket},
    },
    response::Response,
};
use corvo_chat_proto::{
    PROTOCOL_VERSION,
    frames::{ClientFrame, ErrorCode, ServerFrame, SignedOutReason},
    opaque::Suite,
};
use opaque_ke::ServerLogin;
use tokio::{
    sync::oneshot,
    time::{Instant, MissedTickBehavior, interval_at, sleep_until, timeout, timeout_at},
};

use crate::{AppState, PeerAddr, accounts, hub::Kick};

mod devices;
mod secrets;
mod sign_in;
mod signup;

use sign_in::LoginKind;

/// The WebSocket route.
pub const PATH: &str = "/ws";
/// The largest frame (and message) a client may send.
pub const MAX_FRAME: usize = 128 * 1024;
/// How long a connection may stay open without signing in.
pub const ANON_TIMEOUT: Duration = Duration::from_secs(300);
/// How often a signed-in connection is pinged.
pub const PING_EVERY: Duration = Duration::from_secs(30);
/// How long a signed-in connection may send nothing (pongs count).
pub const IDLE_TIMEOUT: Duration = Duration::from_secs(90);
/// How long one write may wait for a client that doesn't read. A write
/// blocks the loop and its timers, so this bounds how long any connection
/// outlives its deadline.
pub const SEND_TIMEOUT: Duration = Duration::from_secs(10);

/// Each connection's id, so a stale connection can't let go of a session a
/// newer one holds.
static NEXT_CONN: AtomicU64 = AtomicU64::new(1);

pub async fn upgrade(
    ws: WebSocketUpgrade,
    State(state): State<AppState>,
    Extension(peer): Extension<PeerAddr>,
) -> Response {
    let ip = peer.0.ip();
    ws.max_message_size(MAX_FRAME).max_frame_size(MAX_FRAME).on_upgrade(move |socket| run(socket, state, ip))
}

/// Where a connection is.
enum Conn {
    Anonymous,
    /// After `signup_offer`, waiting for `signup_finish`.
    SignupOffered(Box<signup::Offered>),
    /// After a sign-in or recovery `login_offer`, waiting for the matching finish.
    LoginOffered(Box<sign_in::Offered>),
    /// After `recovery_finish`, waiting for `reregister` and `recovery_complete`.
    Recovering(Box<secrets::Recovering>),
    SignedIn {
        session: i64,
        account_id: i64,
        /// Until when the password was proven again, for secret changes.
        reauthed_until: Option<i64>,
        /// After a reauth `login_offer`: the device stays signed in meanwhile.
        reauth: Option<Box<ServerLogin<Suite>>>,
    },
}

impl Conn {
    fn session(&self) -> Option<i64> {
        match self {
            Conn::SignedIn { session, .. } => Some(*session),
            _ => None,
        }
    }
}

/// Whether the connection goes on after a frame.
enum Flow {
    Stay,
    Close,
}

/// What the gate says about a frame in the connection's state.
enum Verdict {
    Pass,
    Refuse(ErrorCode),
    /// The signed-in account is gone (an expired pending account).
    Gone,
}

/// What woke the loop.
enum Event {
    Received(Option<Result<Message, axum::Error>>),
    AnonTimeout,
    Ping,
    Idle,
    Kicked(Kick),
}

/// One connection.
struct Link {
    socket: WebSocket,
    state: AppState,
    id: u64,
    /// Where the connection comes from, for the per-IP limits.
    ip: IpAddr,
    conn: Conn,
    /// Set while signed in: the hub's way to stop this connection.
    kick: Option<oneshot::Receiver<Kick>>,
}

async fn run(socket: WebSocket, state: AppState, ip: IpAddr) {
    let id = NEXT_CONN.fetch_add(1, Ordering::Relaxed);
    let mut link = Link { socket, state, id, ip, conn: Conn::Anonymous, kick: None };
    let anon_until = Instant::now() + ANON_TIMEOUT;
    if let Ok(Flow::Stay) = timeout_at(anon_until, link.hello()).await {
        link.serve(anon_until).await;
    }
    if let Some(session) = link.conn.session() {
        link.state.hub.leave(session, link.id);
    }
    let _ = timeout(SEND_TIMEOUT, link.socket.send(Message::Close(None))).await;
}

impl Link {
    /// The first frame: `hello`, or the connection closes.
    async fn hello(&mut self) -> Flow {
        let frame = loop {
            match self.socket.recv().await {
                Some(Ok(Message::Text(text))) => break serde_json::from_str::<ClientFrame>(text.as_str()).ok(),
                Some(Ok(Message::Binary(_))) => break None,
                Some(Ok(Message::Ping(_) | Message::Pong(_))) => {}
                Some(Ok(Message::Close(_)) | Err(_)) | None => return Flow::Close,
            }
        };
        match frame {
            Some(ClientFrame::Hello { protocol_version, .. }) if protocol_version < PROTOCOL_VERSION => {
                self.error(ErrorCode::UpdateRequired, "run /update").await;
                Flow::Close
            }
            Some(ClientFrame::Hello { .. }) => {
                self.send(&ServerFrame::Ready { server_version: env!("CARGO_PKG_VERSION").into() }).await
            }
            _ => {
                self.error(ErrorCode::BadFrame, "the first frame must be hello").await;
                Flow::Close
            }
        }
    }

    /// Every frame after `hello`, until the connection closes.
    async fn serve(&mut self, anon_until: Instant) {
        let mut ping = interval_at(Instant::now() + PING_EVERY, PING_EVERY);
        ping.set_missed_tick_behavior(MissedTickBehavior::Delay);
        let mut heard = Instant::now();
        loop {
            let signed_in = self.conn.session().is_some();
            // Kicks first, then the timers, then frames: a flood of frames
            // never keeps a signed-out device served or a deadline unchecked.
            let event = tokio::select! {
                biased;
                kick = kicked(&mut self.kick), if signed_in => Event::Kicked(kick),
                _ = sleep_until(anon_until), if !signed_in => Event::AnonTimeout,
                _ = sleep_until(heard + IDLE_TIMEOUT), if signed_in => Event::Idle,
                _ = ping.tick(), if signed_in => Event::Ping,
                message = self.socket.recv() => Event::Received(message),
            };
            let flow = match event {
                Event::Received(Some(Ok(message))) => {
                    heard = Instant::now();
                    self.receive(message).await
                }
                Event::Received(_) | Event::AnonTimeout | Event::Idle | Event::Kicked(Kick::Replaced) => Flow::Close,
                Event::Ping => self.send_raw(Message::Ping(Default::default())).await,
                Event::Kicked(Kick::SignedOut(reason)) => {
                    self.send(&ServerFrame::SignedOut { reason }).await;
                    Flow::Close
                }
            };
            if let Flow::Close = flow {
                return;
            }
            if !signed_in && self.conn.session().is_some() {
                ping.reset();
                heard = Instant::now();
            }
        }
    }

    async fn receive(&mut self, message: Message) -> Flow {
        match message {
            Message::Text(text) => match serde_json::from_str::<ClientFrame>(text.as_str()) {
                Ok(frame) => self.dispatch(frame).await,
                Err(_) => self.error(ErrorCode::BadFrame, "not a frame").await,
            },
            Message::Binary(_) => self.error(ErrorCode::BadFrame, "frames are text").await,
            Message::Ping(_) | Message::Pong(_) => Flow::Stay,
            Message::Close(_) => Flow::Close,
        }
    }

    async fn dispatch(&mut self, frame: ClientFrame) -> Flow {
        match gate(&self.conn, &self.state, &frame).await {
            Verdict::Pass => {}
            Verdict::Refuse(code) => return self.error(code, refusal(code)).await,
            Verdict::Gone => {
                if let ClientFrame::Activate { .. } = frame {
                    return self.expired().await;
                }
                self.send(&ServerFrame::SignedOut { reason: SignedOutReason::Expired }).await;
                return Flow::Close;
            }
        }
        use ClientFrame as F;
        match frame {
            F::SignupStart { base, display_name, device, password, recovery } => {
                self.signup_start(base, display_name, device, password, recovery).await
            }
            F::SignupFinish { password, recovery, password_copy, recovery_copy, public_keys } => {
                self.signup_finish(password, recovery, password_copy, recovery_copy, public_keys).await
            }
            F::LoginStart { username, request, device } => {
                self.login_start(LoginKind::Login, username, request, device).await
            }
            F::RecoveryStart { username, request, device } => {
                self.login_start(LoginKind::Recovery, username, request, device).await
            }
            F::LoginFinish { finalization } | F::RecoveryFinish { finalization } => {
                self.login_finish(finalization).await
            }
            F::Resume { token, device_id } => self.resume(token, device_id).await,
            F::ReauthStart { request } => self.reauth_start(request).await,
            F::ReauthFinish { finalization } => self.reauth_finish(finalization).await,
            F::Activate { code } => self.activate(code).await,
            F::Invite { number } => self.invite(number).await,
            F::ListInvites => self.list_invites().await,
            F::Reregister { password, recovery } => self.reregister(password, recovery).await,
            F::RecoveryComplete { password, recovery, password_copy, recovery_copy } => {
                self.recovery_complete(password, recovery, password_copy, recovery_copy).await
            }
            F::ChangePassword { password, password_copy } => self.change_password(password, password_copy).await,
            F::NewRecoveryCode { recovery, recovery_copy } => self.new_recovery_code(recovery, recovery_copy).await,
            F::ListDevices => self.list_devices().await,
            F::SignOutDevice { id } => self.sign_out_device(id).await,
            F::SignOut => self.sign_out().await,
            // The gate refuses a second hello.
            F::Hello { .. } => self.error(ErrorCode::BadFrame, "not expected now").await,
        }
    }

    async fn error(&mut self, code: ErrorCode, message: &str) -> Flow {
        self.send(&ServerFrame::Error { code, message: message.into(), until: None }).await
    }

    async fn send(&mut self, frame: &ServerFrame) -> Flow {
        let text = serde_json::to_string(frame).expect("frames serialize");
        self.send_raw(Message::Text(text.into())).await
    }

    /// Sends `message`, or gives up on the connection after [`SEND_TIMEOUT`].
    async fn send_raw(&mut self, message: Message) -> Flow {
        match timeout(SEND_TIMEOUT, self.socket.send(message)).await {
            Ok(Ok(())) => Flow::Stay,
            Ok(Err(_)) | Err(_) => Flow::Close,
        }
    }
}

/// Whether `frame` may come now. Signed in, the account row is read
/// fresh, so activation and expiry show at once.
async fn gate(conn: &Conn, state: &AppState, frame: &ClientFrame) -> Verdict {
    use ClientFrame as F;
    match conn {
        Conn::Anonymous => anonymous(frame),
        Conn::SignupOffered(_) => match frame {
            F::SignupFinish { .. } => Verdict::Pass,
            _ => anonymous(frame),
        },
        Conn::LoginOffered(offered) => match (offered.kind, frame) {
            (LoginKind::Login, F::LoginFinish { .. }) | (LoginKind::Recovery, F::RecoveryFinish { .. }) => {
                Verdict::Pass
            }
            _ => anonymous(frame),
        },
        Conn::Recovering(_) => match frame {
            F::Reregister { .. } | F::RecoveryComplete { .. } => Verdict::Pass,
            _ => anonymous(frame),
        },
        Conn::SignedIn { account_id, reauthed_until, reauth, .. } => {
            match frame {
                F::Hello { .. } | F::SignupFinish { .. } | F::LoginFinish { .. } | F::RecoveryFinish { .. } => {
                    return Verdict::Refuse(ErrorCode::BadFrame);
                }
                F::SignupStart { .. } | F::LoginStart { .. } | F::RecoveryStart { .. } | F::Resume { .. } => {
                    return Verdict::Refuse(ErrorCode::AlreadySignedIn);
                }
                _ => {}
            }
            let (id, now) = (*account_id, state.clock.now());
            let account = match state.db.call(move |c| accounts::by_id(c, id, now)).await {
                Ok(Some(account)) => account,
                Ok(None) => return Verdict::Gone,
                Err(err) => {
                    tracing::error!("reading account {id}: {err}");
                    return Verdict::Refuse(ErrorCode::Internal);
                }
            };
            let reauthed = reauthed_until.is_some_and(|until| until > now);
            match frame {
                F::Activate { .. } | F::SignOut => Verdict::Pass,
                _ if account.pending_until.is_some() => Verdict::Refuse(ErrorCode::NotActive),
                F::Reregister { .. } | F::ChangePassword { .. } | F::NewRecoveryCode { .. } if !reauthed => {
                    Verdict::Refuse(ErrorCode::NotAllowed)
                }
                F::ReauthFinish { .. } if reauth.is_none() => Verdict::Refuse(ErrorCode::BadFrame),
                F::RecoveryComplete { .. } => Verdict::Refuse(ErrorCode::NotAllowed),
                _ => Verdict::Pass,
            }
        }
    }
}

/// The gate before signing in: the starts pass, the later OPAQUE steps are
/// out of order, and the rest needs a session.
fn anonymous(frame: &ClientFrame) -> Verdict {
    use ClientFrame as F;
    match frame {
        F::SignupStart { .. } | F::LoginStart { .. } | F::RecoveryStart { .. } | F::Resume { .. } => Verdict::Pass,
        F::Hello { .. }
        | F::SignupFinish { .. }
        | F::LoginFinish { .. }
        | F::RecoveryFinish { .. }
        | F::RecoveryComplete { .. } => Verdict::Refuse(ErrorCode::BadFrame),
        _ => Verdict::Refuse(ErrorCode::NotSignedIn),
    }
}

/// The message with a gate refusal.
fn refusal(code: ErrorCode) -> &'static str {
    match code {
        ErrorCode::NotSignedIn => "sign in first",
        ErrorCode::AlreadySignedIn => "already signed in",
        ErrorCode::NotActive => "the account is waiting for its invite code",
        ErrorCode::NotAllowed => "enter your password again first",
        ErrorCode::Internal => "server error, try again",
        _ => "not expected now",
    }
}

/// The hub's kick, or never when not signed in. A dropped sender means the
/// session is no longer this connection's.
async fn kicked(kick: &mut Option<oneshot::Receiver<Kick>>) -> Kick {
    match kick {
        Some(receiver) => receiver.await.unwrap_or(Kick::Replaced),
        None => std::future::pending().await,
    }
}
