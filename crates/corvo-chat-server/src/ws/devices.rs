//! The device list, and signing another device out.

use corvo_chat_proto::{
    B64,
    frames::{DeviceInfo, ErrorCode, ServerFrame, SignedOutReason},
};
use rusqlite::Connection;

use super::{Conn, Flow, Link};
use crate::{hub::Kick, sessions};

/// The account's devices, oldest first, with `this` marked.
pub(super) fn infos(c: &Connection, account_id: i64, this: i64) -> rusqlite::Result<Vec<DeviceInfo>> {
    let list = sessions::list(c, account_id)?;
    Ok(list
        .into_iter()
        .map(|s| DeviceInfo {
            id: s.device.id,
            kind: s.device.kind,
            name: s.device.name,
            first_seen: s.first_seen,
            last_used: s.last_used,
            this_device: s.id == this,
        })
        .collect())
}

impl Link {
    pub(super) async fn list_devices(&mut self) -> Flow {
        let Conn::SignedIn { session, account_id, .. } = self.conn else {
            return self.error(ErrorCode::NotSignedIn, "sign in first").await;
        };
        match self.state.db.call(move |c| infos(c, account_id, session)).await {
            Ok(devices) => self.send(&ServerFrame::Devices { devices }).await,
            Err(err) => self.internal("listing devices", err).await,
        }
    }

    /// Signs out another of the account's devices; a connected one is told
    /// and closed. An unknown id leaves the list as it is.
    pub(super) async fn sign_out_device(&mut self, id: B64<16>) -> Flow {
        let Conn::SignedIn { session, account_id, .. } = self.conn else {
            return self.error(ErrorCode::NotSignedIn, "sign in first").await;
        };
        let hub = self.state.hub.clone();
        let done = self
            .state
            .db
            .call(move |c| -> rusqlite::Result<Option<Vec<DeviceInfo>>> {
                if let Some(target) = sessions::list(c, account_id)?.into_iter().find(|s| s.device.id == id) {
                    if target.id == session {
                        return Ok(None);
                    }
                    let tx = c.transaction()?;
                    sessions::delete(&tx, target.id)?;
                    crate::events::note_devices(&tx, &hub, account_id)?;
                    tx.commit()?;
                    hub.kick(target.id, Kick::SignedOut(SignedOutReason::ByAnotherDevice));
                }
                infos(c, account_id, session).map(Some)
            })
            .await;
        match done {
            Ok(Some(devices)) => self.send(&ServerFrame::Devices { devices }).await,
            Ok(None) => self.error(ErrorCode::BadFrame, "use sign_out for this device").await,
            Err(err) => self.internal("signing out a device", err).await,
        }
    }
}
