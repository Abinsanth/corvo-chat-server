//! Signing in with OPAQUE, resuming a session after a restart, and proving
//! the password again (reauth) before changing a secret.
//!
//! A wrong password only shows on the client, which then sends no
//! finalization. So every `login_start` against a real account counts as a
//! wrong try, and only a finished login clears the count.

use corvo_chat_proto::{
    B64, Blob,
    frames::{Account, Device, ErrorCode, ServerFrame},
    names::{check_device_name, is_username},
    opaque::{CredFinal, CredRequest, CredResponse, Suite, password_id, recovery_id},
};
use opaque_ke::{CredentialFinalization, CredentialRequest, ServerLogin, ServerLoginParameters, ServerRegistration};
use rand_core::OsRng;
use rusqlite::Connection;

use super::{Conn, Flow, Link};
use crate::{
    accounts::{self, AccountRow, LockState, Which},
    attempts,
    hub::{Hub, Joined, Kick},
    limits::Limits,
    sessions::{self, NewSession, SessionError},
};

/// The window of the per-IP limit on unknown usernames and of the recovery limit.
pub(super) const HOUR: i64 = 3_600;
/// The window of the daily sign-in limit.
const DAY: i64 = 86_400;

/// Which secret a `login_offer` is for.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum LoginKind {
    Login,
    Recovery,
}

/// What a `login_offer` leaves for the finish.
pub struct Offered {
    pub kind: LoginKind,
    login: ServerLogin<Suite>,
    /// `None` for a username that doesn't exist: its finish always fails.
    account_id: Option<i64>,
    device: Device,
}

/// Why a start gets no offer.
pub(super) enum Refusal {
    RateLimited,
    Locked(i64),
    LockedForever,
}

/// What the database says about a `login_start` or `recovery_start`.
enum Looked {
    /// No such account: it gets a fake offer.
    Unknown,
    Known {
        account_id: i64,
        record: Vec<u8>,
    },
    Refused(Refusal),
}

/// A new session, ready to hand to its connection.
pub(super) struct Done {
    pub session: i64,
    pub account_id: i64,
    pub token: [u8; 32],
    pub account: Account,
    pub copy: Option<Blob>,
    pub joined: Joined,
}

/// How a proven password sign-in ended.
enum SignIn {
    Done(Box<Done>),
    DailyLimit,
    DeviceLimit,
    /// The account went away in between (an expired pending account).
    Gone,
}

/// How a `resume` ended.
enum Resumed {
    Done(Box<Done>),
    Unknown,
    Expired,
}

/// The lock that refuses a password sign-in, if any.
fn locked(row: &AccountRow, now: i64) -> Option<Refusal> {
    match accounts::lock_state(row, now) {
        LockState::Open => None,
        LockState::Until(until) => Some(Refusal::Locked(until)),
        LockState::Forever => Some(Refusal::LockedForever),
    }
}

/// The checks and counts before an offer. Unknown names count against the
/// IP, and once it has had too many, every start from it is refused, so the
/// refusal says nothing about which names exist.
fn look_up(
    c: &Connection,
    kind: LoginKind,
    username: &str,
    ip: &str,
    now: i64,
    limits: &Limits,
) -> rusqlite::Result<Looked> {
    let since = now - HOUR + 1;
    if attempts::count_since(c, "unknown_user", ip, since)? >= i64::from(limits.unknown_user_tries_per_ip_hour) {
        return Ok(Looked::Refused(Refusal::RateLimited));
    }
    let row = if is_username(username) { accounts::by_username(c, username, now)? } else { None };
    let Some(row) = row else {
        attempts::note(c, "unknown_user", ip, now)?;
        return Ok(Looked::Unknown);
    };
    let (password, recovery) = accounts::records(c, row.id)?;
    match kind {
        LoginKind::Login => {
            if let Some(refusal) = locked(&row, now) {
                return Ok(Looked::Refused(refusal));
            }
            // Counted now: a client that finds the password wrong never finishes.
            accounts::note_try(c, row.id, now, limits)?;
            Ok(Looked::Known { account_id: row.id, record: password })
        }
        // The recovery code is how a lock opens, so locks don't apply here.
        LoginKind::Recovery => {
            let key = row.id.to_string();
            if attempts::count_since(c, "recovery", &key, since)? >= i64::from(limits.recovery_tries_per_hour) {
                return Ok(Looked::Refused(Refusal::RateLimited));
            }
            attempts::note(c, "recovery", &key, now)?;
            Ok(Looked::Known { account_id: row.id, record: recovery })
        }
    }
}

/// Hands `session` to connection `conn` in the hub, and closes the
/// connection of the session it replaced.
pub(super) fn join(hub: &Hub, account: i64, conn: u64, session: &NewSession) -> Joined {
    // Joining first keeps the account online throughout a same-device sign-in.
    let joined = hub.join(session.id, account, conn);
    if let Some(old) = session.replaced {
        hub.kick(old, Kick::Replaced);
    }
    joined
}

/// Whether the client's finalization completes `login`.
fn proven(login: ServerLogin<Suite>, finalization: &CredFinal) -> bool {
    CredentialFinalization::<Suite>::deserialize(&finalization.0)
        .and_then(|message| login.finish(message, ServerLoginParameters::default()))
        .is_ok()
}

impl Link {
    /// `login_start` and `recovery_start`: an offer against the password or
    /// the recovery record, or a fake one for a name that doesn't exist.
    pub(super) async fn login_start(
        &mut self,
        kind: LoginKind,
        username: String,
        request: CredRequest,
        mut device: Device,
    ) -> Flow {
        device.name = match check_device_name(&device.name) {
            Ok(name) => name,
            Err(err) => return self.error(ErrorCode::BadDevice, &err.to_string()).await,
        };
        let Ok(request) = CredentialRequest::<Suite>::deserialize(&request.0) else {
            return self.error(ErrorCode::BadFrame, "not a credential request").await;
        };
        let username = username.trim().to_lowercase();
        let (now, limits, ip) = (self.state.clock.now(), self.state.limits.clone(), self.ip.to_string());
        let name = username.clone();
        let looked = self.state.db.call(move |c| look_up(c, kind, &name, &ip, now, &limits)).await;
        let (account_id, record) = match looked {
            Ok(Looked::Unknown) => (None, None),
            Ok(Looked::Known { account_id, record }) => (Some(account_id), Some(record)),
            Ok(Looked::Refused(refusal)) => return self.refuse(refusal).await,
            Err(err) => return self.internal("looking up a sign-in", err).await,
        };
        let id = match kind {
            LoginKind::Login => password_id(&username),
            LoginKind::Recovery => recovery_id(&username),
        };
        let Some((login, response)) = self.offer(record.as_deref(), &id, request) else {
            return self.internal("starting a login", "a stored record doesn't parse").await;
        };
        self.conn = Conn::LoginOffered(Box::new(Offered { kind, login, account_id, device }));
        self.send(&ServerFrame::LoginOffer { response }).await
    }

    /// The server's half of an OPAQUE login against `record`, or against a
    /// fake one for `None`. `None` back means the record doesn't parse.
    fn offer(
        &self,
        record: Option<&[u8]>,
        id: &[u8],
        request: CredentialRequest<Suite>,
    ) -> Option<(ServerLogin<Suite>, CredResponse)> {
        let record = match record {
            Some(bytes) => Some(ServerRegistration::<Suite>::deserialize(bytes).ok()?),
            None => None,
        };
        let params = ServerLoginParameters::default();
        let started = ServerLogin::start(&mut OsRng, &self.state.setup, record, request, id, params).ok()?;
        Some((started.state, B64(started.message.serialize().as_slice().try_into().ok()?)))
    }

    /// `login_finish` and `recovery_finish`.
    pub(super) async fn login_finish(&mut self, finalization: CredFinal) -> Flow {
        let offered = match std::mem::replace(&mut self.conn, Conn::Anonymous) {
            Conn::LoginOffered(offered) => offered,
            other => {
                self.conn = other;
                return self.error(ErrorCode::BadFrame, "not expected now").await;
            }
        };
        let Offered { kind, login, account_id, device } = *offered;
        let account_id = match account_id {
            Some(id) if proven(login, &finalization) => id,
            _ => return self.wrong().await,
        };
        match kind {
            LoginKind::Login => self.password_sign_in(account_id, device).await,
            LoginKind::Recovery => self.recovery_offer(account_id, device).await,
        }
    }

    /// The password is proven: clear the count, then the daily and device
    /// limits, so a stranger learns nothing from them.
    async fn password_sign_in(&mut self, account_id: i64, device: Device) -> Flow {
        let (now, limits) = (self.state.clock.now(), self.state.limits.clone());
        let (hub, conn_id) = (self.state.hub.clone(), self.id);
        let signed_in = self
            .state
            .db
            .call(move |c| -> rusqlite::Result<SignIn> {
                accounts::note_success(c, account_id)?;
                let Some(row) = accounts::by_id(c, account_id, now)? else { return Ok(SignIn::Gone) };
                let key = account_id.to_string();
                if attempts::count_since(c, "sign_in", &key, now - DAY + 1)? >= i64::from(limits.sign_ins_per_day) {
                    return Ok(SignIn::DailyLimit);
                }
                let session = match sessions::create(c, account_id, &device, now, limits.max_devices) {
                    Ok(session) => session,
                    Err(SessionError::DeviceLimit) => return Ok(SignIn::DeviceLimit),
                    Err(SessionError::Db(err)) => return Err(err),
                };
                attempts::note(c, "sign_in", &key, now)?;
                crate::events::note_devices(c, &hub, account_id)?;
                let copy = accounts::copy(c, account_id, Which::Password)?;
                let joined = join(&hub, account_id, conn_id, &session);
                Ok(SignIn::Done(Box::new(Done {
                    session: session.id,
                    account_id,
                    token: session.token,
                    account: row.to_frame(),
                    copy: Some(Blob(copy)),
                    joined,
                })))
            })
            .await;
        match signed_in {
            Ok(SignIn::Done(done)) => {
                tracing::info!("signed in {}", done.account.username);
                self.enter(*done).await
            }
            Ok(SignIn::DailyLimit) => {
                self.error(ErrorCode::SignInLimit, "Too many sign-ins today, try again tomorrow.").await
            }
            Ok(SignIn::DeviceLimit) => {
                let max = self.state.limits.max_devices;
                let message = format!("Maximum devices reached ({max}). Sign out a device first.");
                self.error(ErrorCode::DeviceLimit, &message).await
            }
            Ok(SignIn::Gone) => self.wrong().await,
            Err(err) => self.internal("signing in", err).await,
        }
    }

    /// A restart: the session goes on. No lock or limit applies, since it
    /// isn't a sign-in.
    pub(super) async fn resume(&mut self, token: B64<32>, device_id: B64<16>) -> Flow {
        let now = self.state.clock.now();
        let (hub, conn_id) = (self.state.hub.clone(), self.id);
        let resumed = self
            .state
            .db
            .call(move |c| -> rusqlite::Result<Resumed> {
                let session = sessions::by_token(c, &token.0)?.filter(|s| s.device.id == device_id);
                let Some(session) = session else { return Ok(Resumed::Unknown) };
                let Some(row) = accounts::by_id(c, session.account_id, now)? else { return Ok(Resumed::Expired) };
                sessions::touch(c, session.id, now)?;
                let joined = hub.join(session.id, row.id, conn_id);
                Ok(Resumed::Done(Box::new(Done {
                    session: session.id,
                    account_id: row.id,
                    token: token.0,
                    account: row.to_frame(),
                    copy: None,
                    joined,
                })))
            })
            .await;
        match resumed {
            Ok(Resumed::Done(done)) => self.enter(*done).await,
            Ok(Resumed::Unknown) => self.error(ErrorCode::SessionUnknown, "This device was signed out.").await,
            Ok(Resumed::Expired) => self.expired().await,
            Err(err) => self.internal("resuming", err).await,
        }
    }

    /// The connection now holds `done`'s session: say so.
    pub(super) async fn enter(&mut self, done: Done) -> Flow {
        let Done { session, account_id, token, account, copy, joined } = done;
        self.kick = Some(joined.kick);
        self.push = Some(joined.push);
        self.conn = Conn::SignedIn { session, account_id, reauthed_until: None, reauth: None };
        let flow = self.send(&ServerFrame::SignedIn { token: B64(token), account, copy }).await;
        if joined.came_online {
            self.tell_contacts(account_id).await;
        }
        flow
    }

    /// Tells the account's mutual contacts whether it's online now. The
    /// state is read when they're told, so a join and a leave racing each
    /// other still end with the right word.
    pub(super) async fn tell_contacts(&mut self, account: i64) {
        let (now, hub) = (self.state.clock.now(), self.state.hub.clone());
        let told = self
            .state
            .db
            .call(move |c| -> rusqlite::Result<()> {
                let Some(row) = accounts::by_id(c, account, now)? else { return Ok(()) };
                let contacts = crate::relations::mutual_contacts(c, account)?;
                hub.announce(account, row.number, &contacts, true);
                Ok(())
            })
            .await;
        if let Err(err) = told {
            tracing::error!("telling contacts about account {account}: {err}");
        }
    }

    /// A signed-in device proves the password again. It counts toward the
    /// lock like a sign-in, but a failure leaves the device signed in.
    pub(super) async fn reauth_start(&mut self, request: CredRequest) -> Flow {
        let Conn::SignedIn { account_id, .. } = self.conn else {
            return self.error(ErrorCode::NotSignedIn, "sign in first").await;
        };
        let Ok(request) = CredentialRequest::<Suite>::deserialize(&request.0) else {
            return self.error(ErrorCode::BadFrame, "not a credential request").await;
        };
        let (now, limits) = (self.state.clock.now(), self.state.limits.clone());
        let looked = self
            .state
            .db
            .call(move |c| -> rusqlite::Result<Result<(String, Vec<u8>), Refusal>> {
                let row = accounts::by_id(c, account_id, now)?.ok_or(rusqlite::Error::QueryReturnedNoRows)?;
                if let Some(refusal) = locked(&row, now) {
                    return Ok(Err(refusal));
                }
                accounts::note_try(c, row.id, now, &limits)?;
                Ok(Ok((row.username, accounts::records(c, row.id)?.0)))
            })
            .await;
        let (username, record) = match looked {
            Ok(Ok(found)) => found,
            Ok(Err(refusal)) => return self.refuse(refusal).await,
            Err(err) => return self.internal("starting a reauth", err).await,
        };
        let Some((login, response)) = self.offer(Some(&record), &password_id(&username), request) else {
            return self.internal("starting a reauth", "the stored record doesn't parse").await;
        };
        if let Conn::SignedIn { reauth, .. } = &mut self.conn {
            *reauth = Some(Box::new(login));
        }
        self.send(&ServerFrame::LoginOffer { response }).await
    }

    pub(super) async fn reauth_finish(&mut self, finalization: CredFinal) -> Flow {
        let Conn::SignedIn { account_id, reauth, .. } = &mut self.conn else {
            return self.error(ErrorCode::NotSignedIn, "sign in first").await;
        };
        let (account_id, login) = (*account_id, reauth.take());
        let Some(login) = login else {
            return self.error(ErrorCode::BadFrame, "not expected now").await;
        };
        if !proven(*login, &finalization) {
            return self.wrong().await;
        }
        if let Err(err) = self.state.db.call(move |c| accounts::note_success(c, account_id)).await {
            return self.internal("finishing a reauth", err).await;
        }
        let until = self.state.clock.now() + self.state.limits.reauth_secs;
        if let Conn::SignedIn { reauthed_until, .. } = &mut self.conn {
            *reauthed_until = Some(until);
        }
        self.send(&ServerFrame::Reauthed).await
    }

    /// The one answer for a wrong username, password or recovery code.
    pub(super) async fn wrong(&mut self) -> Flow {
        self.error(ErrorCode::WrongCredentials, "Username or password is wrong").await
    }

    pub(super) async fn refuse(&mut self, refusal: Refusal) -> Flow {
        match refusal {
            Refusal::RateLimited => self.error(ErrorCode::RateLimited, "Too many tries. Wait an hour.").await,
            Refusal::LockedForever => {
                self.error(ErrorCode::LockedForever, "Locked. Sign in with your recovery code").await
            }
            Refusal::Locked(until) => {
                let minutes = (until - self.state.clock.now() + 59) / 60;
                let message = match minutes {
                    1 => "Locked for 1 more minute".to_string(),
                    n => format!("Locked for {n} more minutes"),
                };
                self.send(&ServerFrame::Error { code: ErrorCode::Locked, message, until: Some(until) }).await
            }
        }
    }
}
