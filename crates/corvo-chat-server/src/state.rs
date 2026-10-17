//! What every handler shares.

use std::sync::Arc;

use corvo_chat_proto::opaque::Suite;
use opaque_ke::ServerSetup;

use crate::{clock::Clock, db::Db, limits::Limits};

#[derive(Clone)]
pub struct AppState {
    pub db: Db,
    pub setup: Arc<ServerSetup<Suite>>,
    pub clock: Clock,
    pub limits: Arc<Limits>,
}
