//! Connection-scoped automation presentation negotiation.

use eguidev::internal::presentation::{EXPERIMENTAL_PRESENTATION_CAPABILITY, Presentation};
use serde_json::Value;
use tmcp::schema::ClientCapabilities;

/// Resolve the requested presentation from a private initialize capability.
pub fn parse_client_capabilities(
    capabilities: &ClientCapabilities,
) -> Result<Presentation, String> {
    let Some(value) = capabilities
        .experimental
        .as_ref()
        .and_then(|experimental| experimental.get(EXPERIMENTAL_PRESENTATION_CAPABILITY))
    else {
        return Ok(Presentation::default());
    };
    let Value::String(value) = value else {
        return Err(format!(
            "{EXPERIMENTAL_PRESENTATION_CAPABILITY} must be a string"
        ));
    };
    match value.as_str() {
        "background" => Ok(Presentation::Background),
        "foreground" => Ok(Presentation::Foreground),
        _ => Err(format!(
            "{EXPERIMENTAL_PRESENTATION_CAPABILITY} must be `background` or `foreground`"
        )),
    }
}

/// State kept for one connected automation session.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct PresentationSession {
    requested: Presentation,
    prior_activation_policy: Option<i64>,
    active: bool,
    conflict_reported: bool,
}

/// Main-thread policy actions for one session transition.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PresentationTransition {
    /// Policy to apply after the transition, if one is known.
    pub(crate) target_activation_policy: Option<i64>,
    /// Whether the app should be deactivated after applying the policy.
    pub(crate) deactivate: bool,
}

impl PresentationSession {
    /// Update the requested presentation and capture the policy for a new session.
    pub fn configure(
        &mut self,
        requested: Presentation,
        observed_activation_policy: Option<i64>,
        accessory_policy: i64,
    ) -> PresentationTransition {
        if !self.active {
            self.prior_activation_policy = observed_activation_policy;
            self.conflict_reported = false;
        }
        self.active = true;
        self.requested = requested;
        PresentationTransition {
            target_activation_policy: match requested {
                Presentation::Background => Some(accessory_policy),
                Presentation::Foreground => self.prior_activation_policy,
            },
            deactivate: requested == Presentation::Background,
        }
    }

    /// End the session and return the policy captured when it began.
    pub fn disconnect(&mut self) -> Option<PresentationTransition> {
        if !self.active {
            return None;
        }
        self.active = false;
        self.conflict_reported = false;
        Some(PresentationTransition {
            target_activation_policy: self.prior_activation_policy,
            deactivate: false,
        })
    }

    /// Return the currently requested presentation, defaulting to background.
    pub fn requested(&self) -> Presentation {
        self.requested
    }

    /// Return whether a background policy should be reasserted on this frame.
    pub fn should_reassert(
        &self,
        observed_activation_policy: Option<i64>,
        accessory_policy: i64,
    ) -> bool {
        self.active
            && self.requested == Presentation::Background
            && observed_activation_policy != Some(accessory_policy)
    }

    /// Mark a conflicting observed policy and return whether it is the first report.
    pub fn report_conflict(
        &mut self,
        observed_activation_policy: Option<i64>,
        accessory_policy: i64,
    ) -> bool {
        let conflicting = self.active
            && self.requested == Presentation::Background
            && observed_activation_policy != Some(accessory_policy);
        if !conflicting || self.conflict_reported {
            return false;
        }
        self.conflict_reported = true;
        true
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;
    use tmcp::schema::ClientCapabilities;

    use super::*;

    #[test]
    fn missing_capability_defaults_to_background() {
        assert_eq!(
            parse_client_capabilities(&ClientCapabilities::default()).expect("presentation"),
            Presentation::Background
        );
    }

    #[test]
    fn capability_parses_foreground() {
        let capabilities = ClientCapabilities::default().with_experimental_capability(
            EXPERIMENTAL_PRESENTATION_CAPABILITY,
            json!(Presentation::Foreground.as_str()),
        );
        assert_eq!(
            parse_client_capabilities(&capabilities).expect("presentation"),
            Presentation::Foreground
        );
    }

    #[test]
    fn capability_rejects_non_string_and_unknown_values() {
        for value in [json!(true), json!("sideways")] {
            let capabilities = ClientCapabilities::default()
                .with_experimental_capability(EXPERIMENTAL_PRESENTATION_CAPABILITY, value);
            assert!(parse_client_capabilities(&capabilities).is_err());
        }
    }

    #[test]
    fn session_transitions_capture_restore_and_reconfigure_idempotently() {
        let mut session = PresentationSession::default();
        let background = session.configure(Presentation::Background, Some(0), 1);
        assert_eq!(background.target_activation_policy, Some(1));
        assert!(background.deactivate);
        assert!(session.should_reassert(Some(0), 1));
        assert!(!session.should_reassert(Some(1), 1));

        let foreground = session.configure(Presentation::Foreground, Some(1), 1);
        assert_eq!(foreground.target_activation_policy, Some(0));
        assert!(!foreground.deactivate);

        let restored = session.disconnect().expect("active session");
        assert_eq!(restored.target_activation_policy, Some(0));
        assert!(session.disconnect().is_none());
    }

    #[test]
    fn conflicting_policy_is_reported_once() {
        let mut session = PresentationSession::default();
        session.configure(Presentation::Background, Some(0), 1);
        assert!(session.report_conflict(Some(0), 1));
        assert!(!session.report_conflict(Some(0), 1));
        assert!(!session.report_conflict(Some(1), 1));
    }

    #[test]
    fn session_handles_missing_native_policy_and_repeated_cycles() {
        let mut session = PresentationSession::default();
        let first = session.configure(Presentation::Background, None, 1);
        assert_eq!(first.target_activation_policy, Some(1));
        assert_eq!(
            session
                .disconnect()
                .expect("active session")
                .target_activation_policy,
            None
        );
        assert!(session.disconnect().is_none());

        let second = session.configure(Presentation::Foreground, None, 1);
        assert_eq!(second.target_activation_policy, None);
        assert_eq!(
            session
                .disconnect()
                .expect("active session")
                .target_activation_policy,
            None
        );
    }
}
