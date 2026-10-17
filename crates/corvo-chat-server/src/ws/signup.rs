//! Signing up, activating with an invite code, inviting, and signing out.

use corvo_chat_proto::{
    B64, Blob, Number, PublicKeys,
    frames::{Account, Device, ErrorCode, InviteInfo, ServerFrame, SignedOutReason},
    names::{check_base, check_device_name, check_display_name},
    opaque::{RegRequest, RegUpload, Suite, password_id, recovery_id},
};
use data_encoding::BASE64;
use opaque_ke::{RegistrationRequest, RegistrationUpload, ServerRegistration};

use super::{Conn, Flow, Link};
use crate::{
    accounts::{self, CreateError, NewAccount},
    attempts,
    invites::{self, InviteError},
    sessions,
};

/// The rolling window of the signup limits.
const SIGNUP_WINDOW: i64 = 86_400;

/// What `signup_start` leaves for `signup_finish`.
#[derive(Clone)]
pub struct Offered {
    username: String,
    number: Number,
    display_name: String,
    device: Device,
}

/// The two OPAQUE messages the server sends in a `signup_offer`.
type Responses = (B64<64>, B64<64>);

/// The result of the signup limit checks and the name draw.
enum Start {
    Offer(String, Number),
    Limit(&'static str),
}

/// The result of creating the account and its session.
enum Created {
    Done { token: [u8; 32], account_id: i64, session: i64, kick: tokio::sync::oneshot::Receiver<crate::hub::Kick> },
    Taken,
}

impl Link {
    pub(super) async fn signup_start(
        &mut self,
        base: String,
        display_name: String,
        mut device: Device,
        password: RegRequest,
        recovery: RegRequest,
    ) -> Flow {
        let base = match check_base(&base) {
            Ok(base) => base,
            Err(err) => return self.error(ErrorCode::BadUsername, &err.to_string()).await,
        };
        let display_name = match check_display_name(&display_name) {
            Ok(name) => name,
            Err(err) => return self.error(ErrorCode::BadDisplayName, &err.to_string()).await,
        };
        device.name = match check_device_name(&device.name) {
            Ok(name) => name,
            Err(err) => return self.error(ErrorCode::BadDevice, &err.to_string()).await,
        };
        let (now, limits) = (self.state.clock.now(), self.state.limits.clone());
        let (device_key, ip_key) = (BASE64.encode(&device.id.0), self.ip.to_string());
        let started = self
            .state
            .db
            .call(move |c| -> rusqlite::Result<Start> {
                // Tries at or before `now - 24 h` have left the window.
                let since = now - SIGNUP_WINDOW + 1;
                if attempts::count_since(c, "signup_device", &device_key, since)?
                    >= i64::from(limits.signups_per_device_day)
                {
                    return Ok(Start::Limit("Only one new account per device per day."));
                }
                if attempts::count_since(c, "signup_ip", &ip_key, since)? >= i64::from(limits.signups_per_ip_day) {
                    return Ok(Start::Limit("Too many new accounts from this network today."));
                }
                let (username, number) = accounts::pick_name(c, &base)?;
                Ok(Start::Offer(username, number))
            })
            .await;
        let (username, number) = match started {
            Ok(Start::Offer(username, number)) => (username, number),
            Ok(Start::Limit(message)) => return self.error(ErrorCode::SignupLimit, message).await,
            Err(err) => return self.internal("choosing a name", err).await,
        };
        let Some((password, recovery)) = self.register(&username, &password, &recovery) else {
            return self.error(ErrorCode::BadFrame, "not a registration request").await;
        };
        self.conn = Conn::SignupOffered(Box::new(Offered { username: username.clone(), number, display_name, device }));
        self.send(&ServerFrame::SignupOffer { username, number, password, recovery }).await
    }

    /// The server's half of both registrations, or `None` for a bad request.
    fn register(&self, username: &str, password: &RegRequest, recovery: &RegRequest) -> Option<Responses> {
        let start = |request: &RegRequest, id: Vec<u8>| -> Option<B64<64>> {
            let request = RegistrationRequest::<Suite>::deserialize(&request.0).ok()?;
            let started = ServerRegistration::<Suite>::start(&self.state.setup, request, &id).ok()?;
            Some(B64(started.message.serialize().as_slice().try_into().ok()?))
        };
        Some((start(password, password_id(username))?, start(recovery, recovery_id(username))?))
    }

    pub(super) async fn signup_finish(
        &mut self,
        password: RegUpload,
        recovery: RegUpload,
        password_copy: Blob,
        recovery_copy: Blob,
        public_keys: PublicKeys,
    ) -> Flow {
        let Conn::SignupOffered(offered) = &self.conn else {
            return self.error(ErrorCode::BadFrame, "not expected now").await;
        };
        let Offered { username, number, display_name, device } = (**offered).clone();
        if !public_keys.verify() {
            return self.error(ErrorCode::BadPublicKeys, "those keys don't check out").await;
        }
        let record = |upload: &RegUpload| {
            let upload = RegistrationUpload::<Suite>::deserialize(&upload.0).ok()?;
            Some(ServerRegistration::<Suite>::finish(upload).serialize().to_vec())
        };
        let (Some(password_record), Some(recovery_record)) = (record(&password), record(&recovery)) else {
            return self.error(ErrorCode::BadFrame, "not a registration upload").await;
        };
        let new = NewAccount {
            username: username.clone(),
            number,
            display_name: display_name.clone(),
            password_record,
            recovery_record,
            password_copy: password_copy.0,
            recovery_copy: recovery_copy.0,
            public_keys,
        };
        let (now, limits) = (self.state.clock.now(), self.state.limits.clone());
        let (device_key, ip_key) = (BASE64.encode(&device.id.0), self.ip.to_string());
        let (hub, conn_id) = (self.state.hub.clone(), self.id);
        // The account, its signup tries, the session and the hub entry go in
        // one call, so the database lock orders them.
        let created = self
            .state
            .db
            .call(move |c| -> Result<Created, String> {
                let account_id = match accounts::create_pending(c, &new, now, limits.pending_secs) {
                    Ok(id) => id,
                    Err(CreateError::Taken) => return Ok(Created::Taken),
                    Err(CreateError::Db(err)) => return Err(err.to_string()),
                };
                attempts::note(c, "signup_device", &device_key, now).map_err(|e| e.to_string())?;
                attempts::note(c, "signup_ip", &ip_key, now).map_err(|e| e.to_string())?;
                let session =
                    sessions::create(c, account_id, &device, now, limits.max_devices).map_err(|e| format!("{e:?}"))?;
                let kick = hub.join(session.id, conn_id);
                Ok(Created::Done { token: session.token, account_id, session: session.id, kick })
            })
            .await;
        match created {
            Ok(Created::Done { token, account_id, session, kick }) => {
                let account = Account {
                    username,
                    number,
                    display_name,
                    pending_until: Some(now + self.state.limits.pending_secs),
                };
                tracing::info!("signed up {} ({})", account.username, account.number);
                self.kick = Some(kick);
                self.conn = Conn::SignedIn { session, account_id, reauthed_until: None };
                self.send(&ServerFrame::SignedIn { token: B64(token), account, copy: None }).await
            }
            Ok(Created::Taken) => {
                self.conn = Conn::Anonymous;
                self.error(ErrorCode::TryAgain, "Please sign up again.").await
            }
            Err(err) => self.internal("signing up", err).await,
        }
    }
}

impl Link {
    /// A pending account types the invite code it was given.
    pub(super) async fn activate(&mut self, code: String) -> Flow {
        let Conn::SignedIn { account_id, .. } = self.conn else {
            return self.error(ErrorCode::NotSignedIn, "sign in first").await;
        };
        let (now, limits) = (self.state.clock.now(), self.state.limits.clone());
        let done = self
            .state
            .db
            .call(move |c| -> rusqlite::Result<Result<Option<Account>, ErrorCode>> {
                let Some(row) = accounts::by_id(c, account_id, now)? else { return Ok(Ok(None)) };
                if row.pending_until.is_none() {
                    return Ok(Err(ErrorCode::BadFrame));
                }
                match invites::redeem(c, account_id, &code, now, limits.code_tries) {
                    Ok(inviter) => {
                        accounts::activate(c, account_id, inviter, now)?;
                        Ok(Ok(Some(Account { pending_until: None, ..row.to_frame() })))
                    }
                    Err(InviteError::Wrong) => Ok(Err(ErrorCode::CodeWrong)),
                    Err(InviteError::Dead | InviteError::TooMany) => Ok(Err(ErrorCode::CodeDead)),
                    Err(InviteError::Db(err)) => Err(err),
                }
            })
            .await;
        match done {
            Ok(Ok(Some(account))) => {
                tracing::info!("activated {}", account.username);
                self.send(&ServerFrame::Activated { account }).await
            }
            Ok(Ok(None)) => self.expired().await,
            Ok(Err(ErrorCode::CodeWrong)) => self.error(ErrorCode::CodeWrong, "That code is wrong.").await,
            Ok(Err(ErrorCode::CodeDead)) => {
                self.error(ErrorCode::CodeDead, "That code no longer works. Ask for a new invite.").await
            }
            Ok(Err(code)) => self.error(code, "not expected now").await,
            Err(err) => self.internal("activating", err).await,
        }
    }

    /// An active user makes a code for a waiting account.
    pub(super) async fn invite(&mut self, number: Number) -> Flow {
        let Conn::SignedIn { account_id, .. } = self.conn else {
            return self.error(ErrorCode::NotSignedIn, "sign in first").await;
        };
        let (now, limits) = (self.state.clock.now(), self.state.limits.clone());
        let made = self
            .state
            .db
            .call(move |c| -> rusqlite::Result<Result<InviteInfo, ErrorCode>> {
                let Some(target) = accounts::by_number(c, number, now)? else { return Ok(Err(ErrorCode::NotPending)) };
                let Some(expires_at) = target.pending_until else { return Ok(Err(ErrorCode::NotPending)) };
                match invites::create(c, target.id, Some(account_id), now, limits.open_invites) {
                    Ok(code) => Ok(Ok(InviteInfo { number, display_name: target.display_name, code, expires_at })),
                    Err(InviteError::TooMany) => Ok(Err(ErrorCode::TooManyInvites)),
                    Err(InviteError::Db(err)) => Err(err),
                    Err(InviteError::Wrong | InviteError::Dead) => unreachable!("only redeem says so"),
                }
            })
            .await;
        match made {
            Ok(Ok(invite)) => self.send(&ServerFrame::Invited { invite }).await,
            Ok(Err(ErrorCode::NotPending)) => {
                self.error(ErrorCode::NotPending, "That number isn't waiting for an invite.").await
            }
            Ok(Err(code)) => self.error(code, "You have too many open invites.").await,
            Err(err) => self.internal("inviting", err).await,
        }
    }

    pub(super) async fn list_invites(&mut self) -> Flow {
        let Conn::SignedIn { account_id, .. } = self.conn else {
            return self.error(ErrorCode::NotSignedIn, "sign in first").await;
        };
        let now = self.state.clock.now();
        match self.state.db.call(move |c| invites::open(c, account_id, now)).await {
            Ok(invites) => self.send(&ServerFrame::Invites { invites }).await,
            Err(err) => self.internal("listing invites", err).await,
        }
    }

    /// The device signs itself out: the session goes, the connection closes.
    pub(super) async fn sign_out(&mut self) -> Flow {
        let Conn::SignedIn { session, .. } = self.conn else {
            return self.error(ErrorCode::NotSignedIn, "sign in first").await;
        };
        if let Err(err) = self.state.db.call(move |c| sessions::delete(c, session)).await {
            return self.internal("signing out", err).await;
        }
        self.send(&ServerFrame::SignedOut { reason: SignedOutReason::ThisDevice }).await;
        Flow::Close
    }

    /// The signed-in pending account ran out: tell the device and close.
    pub(super) async fn expired(&mut self) -> Flow {
        self.error(ErrorCode::SignupExpired, "Your signup expired. Sign up again.").await;
        self.send(&ServerFrame::SignedOut { reason: SignedOutReason::Expired }).await;
        Flow::Close
    }

    /// Logs a storage failure (never frame contents) and answers `internal`.
    pub(super) async fn internal(&mut self, what: &str, err: impl std::fmt::Debug) -> Flow {
        tracing::error!("{what}: {err:?}");
        self.error(ErrorCode::Internal, "server error, try again").await
    }
}
