//! Every tunable number, with the spec's values as the default.

#[derive(Clone, Debug)]
pub struct Limits {
    pub max_devices: usize,
    pub sign_ins_per_day: u32,
    pub wrong_before_lock: i64,
    pub lock_secs: i64,
    pub wrong_before_forever: i64,
    pub recovery_tries_per_hour: u32,
    pub unknown_user_tries_per_ip_hour: u32,
    pub signups_per_device_day: u32,
    pub signups_per_ip_day: u32,
    pub pending_secs: i64,
    pub code_tries: i64,
    pub open_invites: usize,
    pub device_idle_secs: i64,
    pub reauth_secs: i64,
    pub messages_per_minute: u32,
    pub new_people_per_day: u32,
    pub lookups_per_hour: u32,
    pub open_requests: i64,
    pub request_messages: i64,
    /// Messages per chat in a new device's snapshot, and the most one
    /// `history` page holds.
    pub snapshot_messages: usize,
    /// Display name, device name, pin, block and unblock changes per account per hour.
    pub changes_per_hour: u32,
    /// Pinned keys per account.
    pub max_pins: i64,
}

impl Default for Limits {
    fn default() -> Limits {
        Limits {
            max_devices: 3,
            sign_ins_per_day: 5,
            wrong_before_lock: 5,
            lock_secs: 900,
            wrong_before_forever: 10,
            recovery_tries_per_hour: 5,
            unknown_user_tries_per_ip_hour: 20,
            signups_per_device_day: 1,
            signups_per_ip_day: 3,
            pending_secs: 86_400,
            code_tries: 5,
            open_invites: 5,
            device_idle_secs: 30 * 86_400,
            reauth_secs: 300,
            messages_per_minute: 60,
            new_people_per_day: 20,
            lookups_per_hour: 30,
            open_requests: 20,
            request_messages: 20,
            snapshot_messages: 50,
            changes_per_hour: 60,
            max_pins: 500,
        }
    }
}
