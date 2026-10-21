//! Gate for acting on discoveries without asking the user.

use crate::types::Discovery;

/// How a discovery came to be suggested.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SuggestionOrigin {
    /// The agent touched the domain.
    Inline,
    /// The background reasoner guessed the domain from the conversation.
    Background,
}

/// Decides whether a discovery may be installed without an explicit click.
pub trait TrustPolicy: Send + Sync {
    fn may_auto_install(&self, discovery: &Discovery, origin: SuggestionOrigin) -> bool;
}

/// Never auto-installs; every discovery is a nudge. A trustManifest-backed policy (verifying the
/// card against a signed manifest from the publisher) would plug in here.
#[derive(Debug, Default)]
pub struct NudgeOnlyPolicy;

impl TrustPolicy for NudgeOnlyPolicy {
    fn may_auto_install(&self, _discovery: &Discovery, _origin: SuggestionOrigin) -> bool {
        false
    }
}
