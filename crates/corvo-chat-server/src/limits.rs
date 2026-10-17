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
        }
    }
}
