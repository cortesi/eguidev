//! Reusable instrumentation and typed fixture helpers for egui applications.

use std::{
    collections::VecDeque,
    sync::{Arc, Mutex},
};

use crate::{
    DevMcp, FixtureCall, FixtureError, FixtureResponse, FixtureResult, FixtureSpec, WidgetRole,
    WidgetRoleMeta, WidgetValue, frame_scope, name_viewport, track_widget_with_meta,
};

/// Run one immediate named viewport frame under Eguidev instrumentation.
pub fn viewport_frame<R>(
    devmcp: &DevMcp,
    ui: &mut egui::Ui,
    viewport_name: impl Into<String>,
    container_id: impl Into<String>,
    add_contents: impl FnOnce(&mut egui::Ui) -> R,
) -> R {
    let viewport_name = viewport_name.into();
    frame_scope(devmcp, ui, container_id, |ui| {
        name_viewport(ui.ctx(), viewport_name);
        add_contents(ui)
    })
}

/// Record a tiny metadata widget without changing visible content.
pub fn value_anchor(ui: &mut egui::Ui, id: impl Into<String>, value: WidgetValue) {
    let id = id.into();
    track_widget_with_meta(
        ui,
        id.clone(),
        WidgetRoleMeta::Plain(WidgetRole::Label),
        Some(id),
        Some(value),
        |ui| {
            let (_rect, response) =
                ui.allocate_exact_size(egui::Vec2::splat(1.0), egui::Sense::hover());
            response
        },
    );
}

/// One typed fixture catalog entry.
pub struct TypedFixture<K> {
    kind: K,
    name: &'static str,
    description: &'static str,
    decorate: fn(FixtureSpec) -> FixtureSpec,
}

impl<K> TypedFixture<K> {
    /// Create a typed fixture catalog entry.
    #[must_use]
    pub const fn new(
        kind: K,
        name: &'static str,
        description: &'static str,
        decorate: fn(FixtureSpec) -> FixtureSpec,
    ) -> Self {
        Self {
            kind,
            name,
            description,
            decorate,
        }
    }

    /// Return the application fixture kind.
    pub const fn kind(&self) -> &K {
        &self.kind
    }

    /// Return the stable runtime fixture name.
    pub const fn name(&self) -> &'static str {
        self.name
    }

    /// Build the Eguidev fixture schema.
    #[must_use]
    pub fn spec(&self) -> FixtureSpec {
        (self.decorate)(FixtureSpec::new(self.name, self.description))
    }
}

/// Typed fixture catalog and cross-thread request queue.
pub struct TypedFixtures<K: 'static> {
    definitions: &'static [TypedFixture<K>],
    pending: Arc<Mutex<VecDeque<K>>>,
}

impl<K> Clone for TypedFixtures<K> {
    fn clone(&self) -> Self {
        Self {
            definitions: self.definitions,
            pending: Arc::clone(&self.pending),
        }
    }
}

impl<K> TypedFixtures<K>
where
    K: Copy + Eq + Send + 'static,
{
    /// Create an empty queue backed by a static typed fixture catalog.
    #[must_use]
    pub fn new(definitions: &'static [TypedFixture<K>]) -> Self {
        Self {
            definitions,
            pending: Arc::new(Mutex::new(VecDeque::new())),
        }
    }

    /// Return the typed fixture catalog entries.
    #[must_use]
    pub const fn definitions(&self) -> &'static [TypedFixture<K>] {
        self.definitions
    }

    /// Build the fixture schemas advertised to the Eguidev runtime.
    #[must_use]
    pub fn catalog(&self) -> Vec<FixtureSpec> {
        self.definitions.iter().map(TypedFixture::spec).collect()
    }

    /// Resolve a stable runtime name to its application fixture kind.
    #[must_use]
    pub fn kind(&self, name: &str) -> Option<K> {
        self.definitions
            .iter()
            .find(|definition| definition.name == name)
            .map(|definition| definition.kind)
    }

    /// Queue a runtime fixture call for application on the egui thread.
    pub fn request(&self, call: &FixtureCall) -> FixtureResult {
        let kind = self.kind(&call.name).ok_or_else(|| {
            FixtureError::new(
                "unknown_fixture",
                format!("unknown fixture `{}`", call.name),
            )
        })?;
        self.pending
            .lock()
            .map_err(|_| FixtureError::new("fixture_queue", "fixture queue lock poisoned"))?
            .push_back(kind);
        Ok(FixtureResponse::new())
    }

    /// Drain pending fixture kinds for application on the egui thread.
    #[must_use]
    pub fn drain(&self) -> Vec<K> {
        match self.pending.lock() {
            Ok(mut pending) => pending.drain(..).collect(),
            Err(poisoned) => poisoned.into_inner().drain(..).collect(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::FixtureParams;

    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    enum Kind {
        Base,
        Detail,
    }

    static DEFINITIONS: &[TypedFixture<Kind>] = &[
        TypedFixture::new(Kind::Base, "base", "Base fixture", |spec| {
            spec.anchor("root")
        }),
        TypedFixture::new(Kind::Detail, "detail", "Detail fixture", |spec| spec),
    ];

    #[test]
    fn typed_fixtures_share_catalog_lookup_and_queue_state() {
        let fixtures = TypedFixtures::new(DEFINITIONS);
        let requester = fixtures.clone();
        let call = FixtureCall {
            name: "detail".to_string(),
            params: FixtureParams::default(),
        };

        assert_eq!(fixtures.catalog().len(), 2);
        assert_eq!(fixtures.kind("base"), Some(Kind::Base));
        requester.request(&call).expect("queue fixture");
        assert_eq!(fixtures.drain(), vec![Kind::Detail]);
        assert!(fixtures.drain().is_empty());
    }

    #[test]
    fn typed_fixtures_reject_unknown_names() {
        let fixtures = TypedFixtures::new(DEFINITIONS);
        let call = FixtureCall {
            name: "missing".to_string(),
            params: FixtureParams::default(),
        };

        let error = fixtures.request(&call).expect_err("unknown fixture");
        assert_eq!(error.code, "unknown_fixture");
        assert!(fixtures.drain().is_empty());
    }
}
