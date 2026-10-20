use std::collections::HashSet;

use crate::types::Discovery;

/// De-duplication, negative caching and pending/installed/dismissed bookkeeping for one app
/// instance. Holds no I/O so the caller decides where probes run.
#[derive(Debug, Default)]
pub struct DiscoveryState {
    /// Domains already probed or in flight, positive or negative.
    seen_domains: HashSet<String>,
    /// Domains whose probe advertised nothing.
    negative_domains: HashSet<String>,
    pending: Vec<Discovery>,
    installed: HashSet<String>,
    dismissed: HashSet<String>,
}

impl DiscoveryState {
    /// Returns true if `domain` has not been probed yet and marks it as in flight.
    pub fn begin_probe(&mut self, domain: &str) -> bool {
        self.seen_domains.insert(domain.to_string())
    }

    /// Records probe results and returns the discoveries that are newly pending.
    pub fn finish_probe(&mut self, domain: &str, discoveries: Vec<Discovery>) -> Vec<Discovery> {
        if discoveries.is_empty() {
            self.negative_domains.insert(domain.to_string());
            return vec![];
        }
        let mut fresh = Vec::new();
        for discovery in discoveries {
            if self.installed.contains(&discovery.id)
                || self.dismissed.contains(&discovery.id)
                || self
                    .pending
                    .iter()
                    .any(|pending| pending.id == discovery.id)
            {
                continue;
            }
            self.pending.push(discovery.clone());
            fresh.push(discovery);
        }
        fresh
    }

    pub fn is_negative(&self, domain: &str) -> bool {
        self.negative_domains.contains(domain)
    }

    pub fn pending(&self) -> &[Discovery] {
        &self.pending
    }

    pub fn get(&self, id: &str) -> Option<&Discovery> {
        self.pending.iter().find(|discovery| discovery.id == id)
    }

    pub fn mark_installed(&mut self, id: &str) {
        self.installed.insert(id.to_string());
        self.pending.retain(|discovery| discovery.id != id);
    }

    pub fn mark_dismissed(&mut self, id: &str) {
        self.dismissed.insert(id.to_string());
        self.pending.retain(|discovery| discovery.id != id);
    }

    /// Returns a discovery whose install failed to the pending list so it can be retried.
    pub fn requeue(&mut self, discovery: Discovery) {
        self.installed.remove(&discovery.id);
        if self.get(&discovery.id).is_none() {
            self.pending.push(discovery);
        }
    }

    pub fn is_installed(&self, id: &str) -> bool {
        self.installed.contains(id)
    }
}
