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

/// Presentation requests held by connected automation sessions.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct PresentationSession {
    prior_activation_policy: Option<i64>,
    sessions: Vec<(u64, Presentation)>,
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
        session_id: u64,
        requested: Presentation,
        observed_activation_policy: Option<i64>,
        accessory_policy: i64,
    ) -> PresentationTransition {
        if self.sessions.is_empty() {
            self.prior_activation_policy = observed_activation_policy;
        }
        self.sessions.retain(|(id, _)| *id != session_id);
        self.sessions.push((session_id, requested));
        self.conflict_reported = false;
        let desired_policy = match requested {
            Presentation::Background => Some(accessory_policy),
            Presentation::Foreground => self.prior_activation_policy,
        };
        PresentationTransition {
            target_activation_policy: desired_policy
                .filter(|policy| Some(*policy) != observed_activation_policy),
            deactivate: requested == Presentation::Background,
        }
    }

    /// End one connection session and restore the newest remaining request.
    pub fn disconnect(
        &mut self,
        session_id: u64,
        observed_activation_policy: Option<i64>,
        accessory_policy: i64,
    ) -> Option<PresentationTransition> {
        let position = self.sessions.iter().position(|(id, _)| *id == session_id)?;
        self.sessions.remove(position);
        self.conflict_reported = false;
        let requested = self.sessions.last().map(|(_, requested)| *requested);
        let desired_policy = match requested {
            Some(Presentation::Background) => Some(accessory_policy),
            Some(Presentation::Foreground) => self.prior_activation_policy,
            None => self.prior_activation_policy,
        };
        let deactivate = requested == Some(Presentation::Background);
        if self.sessions.is_empty() {
            self.prior_activation_policy = None;
        }
        Some(PresentationTransition {
            target_activation_policy: desired_policy
                .filter(|policy| Some(*policy) != observed_activation_policy),
            deactivate,
        })
    }

    /// Return whether at least one automation connection is active.
    pub fn is_active(&self) -> bool {
        !self.sessions.is_empty()
    }

    /// Return the newest active presentation request, defaulting to background.
    pub fn requested(&self) -> Presentation {
        self.sessions
            .last()
            .map_or_else(Presentation::default, |(_, requested)| *requested)
    }

    /// Return whether a background policy should be reasserted on this frame.
    pub fn should_reassert(
        &self,
        observed_activation_policy: Option<i64>,
        accessory_policy: i64,
    ) -> bool {
        if !self.is_active() {
            return false;
        }
        self.requested() == Presentation::Background
            && observed_activation_policy != Some(accessory_policy)
    }

    /// Mark a conflicting observed policy and return whether it is the first report.
    pub fn report_conflict(
        &mut self,
        observed_activation_policy: Option<i64>,
        accessory_policy: i64,
    ) -> bool {
        let conflicting = self.is_active()
            && self.requested() == Presentation::Background
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
        let background = session.configure(1, Presentation::Background, Some(0), 1);
        assert_eq!(background.target_activation_policy, Some(1));
        assert!(background.deactivate);
        assert!(session.should_reassert(Some(0), 1));
        assert!(!session.should_reassert(Some(1), 1));

        let foreground = session.configure(1, Presentation::Foreground, Some(1), 1);
        assert_eq!(foreground.target_activation_policy, Some(0));
        assert!(!foreground.deactivate);

        let restored = session.disconnect(1, Some(0), 1).expect("active session");
        assert_eq!(restored.target_activation_policy, None);
        assert!(session.disconnect(1, Some(0), 1).is_none());
    }

    #[test]
    fn disconnect_leaves_session_removed_when_transition_cannot_apply() {
        let mut session = PresentationSession::default();
        session.configure(7, Presentation::Background, Some(0), 1);
        let previous = session.clone();
        let transition = session.disconnect(7, Some(1), 1);
        assert!(transition.is_some());
        assert!(!session.is_active());
        assert_ne!(session, previous);
    }

    #[test]
    fn conflicting_policy_is_reported_once() {
        let mut session = PresentationSession::default();
        session.configure(1, Presentation::Background, Some(0), 1);
        assert!(session.report_conflict(Some(0), 1));
        assert!(!session.report_conflict(Some(0), 1));
        assert!(!session.report_conflict(Some(1), 1));
    }

    #[test]
    fn session_handles_missing_native_policy_and_repeated_cycles() {
        let mut session = PresentationSession::default();
        let first = session.configure(1, Presentation::Background, None, 1);
        assert_eq!(first.target_activation_policy, Some(1));
        assert_eq!(
            session
                .disconnect(1, Some(1), 1)
                .expect("active session")
                .target_activation_policy,
            None
        );
        assert!(session.disconnect(1, None, 1).is_none());

        let second = session.configure(2, Presentation::Foreground, None, 1);
        assert_eq!(second.target_activation_policy, None);
        assert_eq!(
            session
                .disconnect(2, None, 1)
                .expect("active session")
                .target_activation_policy,
            None
        );
    }

    #[test]
    fn concurrent_sessions_restore_the_newest_remaining_request() {
        let mut session = PresentationSession::default();
        let background = session.configure(1, Presentation::Background, Some(0), 1);
        assert_eq!(background.target_activation_policy, Some(1));

        let duplicate_background = session.configure(2, Presentation::Background, Some(1), 1);
        assert_eq!(duplicate_background.target_activation_policy, None);
        assert!(session.is_active());

        let foreground = session.configure(3, Presentation::Foreground, Some(1), 1);
        assert_eq!(foreground.target_activation_policy, Some(0));
        assert_eq!(session.requested(), Presentation::Foreground);

        let restore_background = session
            .disconnect(3, Some(0), 1)
            .expect("foreground session");
        assert_eq!(restore_background.target_activation_policy, Some(1));
        assert_eq!(session.requested(), Presentation::Background);

        let unchanged = session
            .disconnect(2, Some(1), 1)
            .expect("second background session");
        assert_eq!(unchanged.target_activation_policy, None);
        assert!(session.is_active());

        let restore_prior = session
            .disconnect(1, Some(1), 1)
            .expect("first background session");
        assert_eq!(restore_prior.target_activation_policy, Some(0));
        assert!(!session.is_active());
    }
}
