use std::collections::BTreeSet;

use eguidev::internal::presentation::Presentation;

#[derive(Debug, Clone, Default, PartialEq, Eq)]
struct ProcessObservation {
    group_members: BTreeSet<u32>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
struct RegistryObservation {
    entries: BTreeSet<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct PresentationObservation {
    requested: Presentation,
    observed_activation_policy: Option<String>,
    executable: String,
    bundle_root: Option<String>,
    bundle_identifier: Option<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
struct LaunchServicesObservation {
    bundle_paths: BTreeSet<String>,
    bundle_identifiers: BTreeSet<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct ObservationSnapshot {
    process: ProcessObservation,
    registry: RegistryObservation,
    presentation: PresentationObservation,
    launch_services: LaunchServicesObservation,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct ObservationComparison {
    process_group_stopped: bool,
    new_registry_entries: BTreeSet<String>,
    presentation_matches: bool,
    new_bundle_paths: BTreeSet<String>,
    new_bundle_identifiers: BTreeSet<String>,
}

fn new_entries<T: Ord + Clone>(before: &BTreeSet<T>, after: &BTreeSet<T>) -> BTreeSet<T> {
    after.difference(before).cloned().collect()
}

fn process_group_stopped(before: &ProcessObservation, after: &ProcessObservation) -> bool {
    !before.group_members.is_empty() && after.group_members.is_empty()
}

fn presentation_matches(
    expected: &PresentationObservation,
    observed: &PresentationObservation,
) -> bool {
    expected == observed
}

fn compare_observations(
    before: &ObservationSnapshot,
    after: &ObservationSnapshot,
) -> ObservationComparison {
    ObservationComparison {
        process_group_stopped: process_group_stopped(&before.process, &after.process),
        new_registry_entries: new_entries(&before.registry.entries, &after.registry.entries),
        presentation_matches: presentation_matches(&before.presentation, &after.presentation),
        new_bundle_paths: new_entries(
            &before.launch_services.bundle_paths,
            &after.launch_services.bundle_paths,
        ),
        new_bundle_identifiers: new_entries(
            &before.launch_services.bundle_identifiers,
            &after.launch_services.bundle_identifiers,
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn presentation() -> PresentationObservation {
        PresentationObservation {
            requested: Presentation::Background,
            observed_activation_policy: Some("accessory".to_string()),
            executable: "target/app".to_string(),
            bundle_root: Some("target/app.app".to_string()),
            bundle_identifier: Some("example.app".to_string()),
        }
    }

    #[test]
    fn comparison_tracks_stopped_process_and_new_residue() {
        let process_before = ProcessObservation {
            group_members: [10, 11].into_iter().collect(),
        };
        let registry_before = RegistryObservation {
            entries: ["instance-a".to_string()].into_iter().collect(),
        };
        let launch_services_before = LaunchServicesObservation {
            bundle_paths: ["stable.app".to_string()].into_iter().collect(),
            bundle_identifiers: ["stable.example".to_string()].into_iter().collect(),
        };
        let mut launch_services_after = launch_services_before.clone();
        launch_services_after
            .bundle_paths
            .insert("new.app".to_string());
        launch_services_after
            .bundle_identifiers
            .insert("new.example".to_string());

        let comparison = compare_observations(
            &ObservationSnapshot {
                process: process_before,
                registry: registry_before,
                presentation: presentation(),
                launch_services: launch_services_before,
            },
            &ObservationSnapshot {
                process: ProcessObservation::default(),
                registry: RegistryObservation {
                    entries: ["instance-a", "instance-b"]
                        .into_iter()
                        .map(str::to_string)
                        .collect(),
                },
                presentation: presentation(),
                launch_services: launch_services_after,
            },
        );

        assert!(comparison.process_group_stopped);
        assert_eq!(
            comparison.new_registry_entries,
            ["instance-b".to_string()].into_iter().collect()
        );
        assert!(comparison.presentation_matches);
        assert_eq!(
            comparison.new_bundle_paths,
            ["new.app".to_string()].into_iter().collect()
        );
        assert_eq!(
            comparison.new_bundle_identifiers,
            ["new.example".to_string()].into_iter().collect()
        );
    }

    #[test]
    fn comparison_accepts_clean_shutdown_without_absolute_paths() {
        let before = PresentationObservation {
            requested: Presentation::Foreground,
            observed_activation_policy: Some("regular".to_string()),
            executable: "bin/app".to_string(),
            bundle_root: None,
            bundle_identifier: None,
        };
        let comparison = compare_observations(
            &ObservationSnapshot {
                process: ProcessObservation {
                    group_members: [1].into_iter().collect(),
                },
                registry: RegistryObservation::default(),
                presentation: before.clone(),
                launch_services: LaunchServicesObservation::default(),
            },
            &ObservationSnapshot {
                process: ProcessObservation::default(),
                registry: RegistryObservation::default(),
                presentation: before,
                launch_services: LaunchServicesObservation::default(),
            },
        );

        assert!(comparison.process_group_stopped);
        assert!(comparison.new_registry_entries.is_empty());
        assert!(comparison.presentation_matches);
        assert!(comparison.new_bundle_paths.is_empty());
        assert!(comparison.new_bundle_identifiers.is_empty());
    }
}
