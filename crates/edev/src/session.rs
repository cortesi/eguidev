//! App-backed command session ownership.

use super::*;

/// One launched app, its direct MCP connection, and its lifecycle registration.
pub struct AppSession {
    /// App launch settings used for bundle metadata.
    launch: LaunchConfig,
    /// Mutable app lifecycle state.
    state: State,
    /// Connected app MCP client.
    pub(super) client: Arc<AsyncMutex<tmcp::Client<()>>>,
}

impl AppSession {
    /// Register, launch, and connect one app session.
    pub(super) async fn start(
        launch: LaunchConfig,
        unavailable_message: &str,
    ) -> Result<Self, EdevError> {
        let instance_registry = InstanceRegistry::register(&launch)?;
        let mut state = State::new(launch.clone(), instance_registry);
        let client = start_app_client(&mut state, unavailable_message).await?;
        Ok(Self {
            launch,
            state,
            client,
        })
    }

    /// Build failure-bundle context while the app process is still alive.
    pub(super) fn bundle_context(&self, config: &SmokeConfig) -> Option<BundleContext> {
        config.bundle_dir.as_ref().and_then(|dir| {
            self.state.app.as_ref().map(|app| BundleContext {
                dir: dir.clone(),
                launch: self.launch.clone(),
                stderr_buffer: Arc::clone(&app.stderr_buffer),
                stdout_buffer: Arc::clone(&app.stdout_buffer),
                collection_timeout_ms: config
                    .suite
                    .script_timeout
                    .map(|duration| duration.as_millis().min(u128::from(u64::MAX)) as u64)
                    .unwrap_or(10_000),
            })
        })
    }

    /// Return the app process group id, if the launched process is still alive.
    pub(super) fn process_group_id(&self) -> Option<i32> {
        self.state.app.as_ref().and_then(|app| app.process_group_id)
    }

    /// Shut down the app and unregister the launcher.
    pub(super) async fn shutdown(mut self) -> Result<(), EdevError> {
        self.state.shutdown().await
    }

    /// Shut down while preserving the command error over a cleanup error.
    pub(super) async fn finish<T>(self, result: Result<T, EdevError>) -> Result<T, EdevError> {
        let shutdown_result = self.shutdown().await;
        match (result, shutdown_result) {
            (Ok(value), Ok(())) => Ok(value),
            (Err(error), _) => Err(error),
            (Ok(_), Err(error)) => Err(error),
        }
    }
}
