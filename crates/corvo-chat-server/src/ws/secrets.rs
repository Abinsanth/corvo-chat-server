//! New secrets: recovery with the recovery code, a password change, and a
//! new recovery code. The last two need a fresh reauth.

use corvo_chat_proto::{
    Blob,
    frames::{Device, ErrorCode, ServerFrame, SignedOutReason},
    opaque::{RegRequest, RegUpload, password_id, recovery_id},
};

use super::{Conn, Flow, Link, sign_in::Done, sign_in::join, signup::password_file};
use crate::{
    accounts::{self, Which},
    hub::Kick,
    sessions::{self, SessionError},
};

/// A connection that has proven the recovery code and now sets new secrets.
pub struct Recovering {
    pub account_id: i64,
    device: Device,
}

impl Link {
    /// The recovery code is proven: hand over its copy and wait for the new
    /// secrets. Nothing changes until `recovery_complete`.
    pub(super) async fn recovery_offer(&mut self, account_id: i64, device: Device) -> Flow {
        let now = self.state.clock.now();
        let copy = self
            .state
            .db
            .call(move |c| -> rusqlite::Result<Option<Vec<u8>>> {
                if accounts::by_id(c, account_id, now)?.is_none() {
                    return Ok(None);
                }
                accounts::copy(c, account_id, Which::Recovery).map(Some)
            })
            .await;
        match copy {
            Ok(Some(copy)) => {
                self.conn = Conn::Recovering(Box::new(Recovering { account_id, device }));
                self.send(&ServerFrame::RecoveryOffer { recovery_copy: Blob(copy) }).await
            }
            Ok(None) => self.wrong().await,
            Err(err) => self.internal("starting a recovery", err).await,
        }
    }

    /// The server's half of fresh registrations for the account's new secrets.
    pub(super) async fn reregister(&mut self, password: Option<RegRequest>, recovery: Option<RegRequest>) -> Flow {
        let account_id = match &self.conn {
            Conn::Recovering(recovering) => recovering.account_id,
            Conn::SignedIn { account_id, .. } => *account_id,
            _ => return self.error(ErrorCode::NotAllowed, "enter your password again first").await,
        };
        let now = self.state.clock.now();
        let username = match self.state.db.call(move |c| accounts::by_id(c, account_id, now)).await {
            Ok(Some(row)) => row.username,
            Ok(None) => {
                self.conn = Conn::Anonymous;
                return self.wrong().await;
            }
            Err(err) => return self.internal("reregistering", err).await,
        };
        let responses = {
            let respond = |request: Option<RegRequest>, id: Vec<u8>| match request {
                Some(request) => self.registration_response(&request, &id).map(Some),
                None => Some(None),
            };
            (respond(password, password_id(&username)), respond(recovery, recovery_id(&username)))
        };
        let (Some(password), Some(recovery)) = responses else {
            return self.error(ErrorCode::BadFrame, "not a registration request").await;
        };
        self.send(&ServerFrame::Registration { password, recovery }).await
    }

    /// Both secrets are replaced, the lock is cleared, every device is
    /// signed out, and this one signs in. No limit applies.
    pub(super) async fn recovery_complete(
        &mut self,
        password: RegUpload,
        recovery: RegUpload,
        password_copy: Blob,
        recovery_copy: Blob,
    ) -> Flow {
        let Conn::Recovering(recovering) = &self.conn else {
            return self.error(ErrorCode::BadFrame, "not expected now").await;
        };
        let (account_id, device) = (recovering.account_id, recovering.device.clone());
        let (Some(password_record), Some(recovery_record)) = (password_file(&password), password_file(&recovery))
        else {
            return self.error(ErrorCode::BadFrame, "not a registration upload").await;
        };
        let (now, limits) = (self.state.clock.now(), self.state.limits.clone());
        let (hub, conn_id) = (self.state.hub.clone(), self.id);
        let done = self
            .state
            .db
            .call(move |c| -> Result<Option<Done>, String> {
                let text = |e: rusqlite::Error| e.to_string();
                let Some(row) = accounts::by_id(c, account_id, now).map_err(text)? else { return Ok(None) };
                let tx = c.transaction().map_err(text)?;
                accounts::replace(&tx, account_id, Which::Password, &password_record, &password_copy.0)
                    .map_err(text)?;
                accounts::replace(&tx, account_id, Which::Recovery, &recovery_record, &recovery_copy.0)
                    .map_err(text)?;
                accounts::note_success(&tx, account_id).map_err(text)?;
                let gone: Vec<i64> = sessions::list(&tx, account_id).map_err(text)?.iter().map(|s| s.id).collect();
                for id in &gone {
                    sessions::delete(&tx, *id).map_err(text)?;
                }
                tx.commit().map_err(text)?;
                for id in gone {
                    hub.kick(id, Kick::SignedOut(SignedOutReason::Recovered));
                }
                // Every session is gone, so the device limit can't refuse this one.
                let session =
                    sessions::create(c, account_id, &device, now, limits.max_devices).map_err(|e| match e {
                        SessionError::Db(e) => e.to_string(),
                        SessionError::DeviceLimit => "device limit right after deleting every session".into(),
                    })?;
                crate::events::note_devices(c, &hub, account_id).map_err(text)?;
                let joined = join(&hub, account_id, conn_id, &session);
                Ok(Some(Done {
                    session: session.id,
                    account_id,
                    token: session.token,
                    account: row.to_frame(),
                    copy: None,
                    joined,
                }))
            })
            .await;
        match done {
            Ok(Some(done)) => {
                tracing::info!("recovered {}", done.account.username);
                self.enter(done).await
            }
            Ok(None) => {
                self.conn = Conn::Anonymous;
                self.wrong().await
            }
            Err(err) => self.internal("finishing a recovery", err).await,
        }
    }

    /// A new password; every other device is signed out.
    pub(super) async fn change_password(&mut self, password: RegUpload, password_copy: Blob) -> Flow {
        self.replace_secret(Which::Password, password, password_copy).await
    }

    /// A new recovery code; the other devices stay.
    pub(super) async fn new_recovery_code(&mut self, recovery: RegUpload, recovery_copy: Blob) -> Flow {
        self.replace_secret(Which::Recovery, recovery, recovery_copy).await
    }

    async fn replace_secret(&mut self, which: Which, upload: RegUpload, copy: Blob) -> Flow {
        let Conn::SignedIn { session, account_id, .. } = self.conn else {
            return self.error(ErrorCode::NotSignedIn, "sign in first").await;
        };
        let Some(record) = password_file(&upload) else {
            return self.error(ErrorCode::BadFrame, "not a registration upload").await;
        };
        let hub = self.state.hub.clone();
        let done = self
            .state
            .db
            .call(move |c| -> rusqlite::Result<()> {
                let tx = c.transaction()?;
                accounts::replace(&tx, account_id, which, &record, &copy.0)?;
                let gone = match which {
                    Which::Password => {
                        accounts::note_success(&tx, account_id)?;
                        sessions::delete_others(&tx, account_id, session)?
                    }
                    Which::Recovery => Vec::new(),
                };
                tx.commit()?;
                if !gone.is_empty() {
                    crate::events::note_devices(c, &hub, account_id)?;
                }
                for id in gone {
                    hub.kick(id, Kick::SignedOut(SignedOutReason::PasswordChanged));
                }
                Ok(())
            })
            .await;
        match (done, which) {
            (Ok(()), Which::Password) => self.send(&ServerFrame::PasswordChanged).await,
            (Ok(()), Which::Recovery) => self.send(&ServerFrame::RecoveryCodeChanged).await,
            (Err(err), _) => self.internal("changing a secret", err).await,
        }
    }
}
