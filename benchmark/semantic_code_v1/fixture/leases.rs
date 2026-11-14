use std::collections::HashMap;

pub fn reap_expired(now: u64, leases: &mut HashMap<String, u64>) {
    leases.retain(|_, deadline| *deadline > now);
}
