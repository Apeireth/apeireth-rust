//! The runtime kernel's mechanism container.
//!
//! # One place where the system is assembled
//!
//! [`Runtime`] is the single canonical execution container. The production
//! assembly crate, CLI, gateway, desktop, and tests all use the same instance;
//! concrete providers, behaviors, capabilities, and session adapters arrive
//! through explicit ports and registries.
//!
//! # What it borrows, and what it refuses, from `UnifiedRuntimeHost`
//!
//! Borrowed: the idea of *one living runtime* rather than a fresh object graph
//! per request.
//!
//! Refused: everything else about its shape. That type had twenty public fields,
//! constructed `MinimaxAdapter::new()` and `"MiniMax-Text-01"` inline, and stored
//! `api_key: String`. Consequently it could not serve a second vendor, could not
//! be built without a key, and exposed a secret to every `Debug` print.
//!
//! Here: providers arrive as capabilities from the plugin registry, so the
//! runtime names no vendor anywhere; credentials arrive through an injected
//! resolver, so no secret is stored on the struct; and the fields are private,
//! so the surface is the methods rather than the layout.
//!
//! # Booting without keys
//!
//! The default credential resolver resolves nothing. A runtime with no keys
//! configured still builds, still starts its plugins, and still answers
//! questions about its capabilities — it simply cannot complete. Requiring a key
//! to construct the object makes every test that does not need one pay for it.

use std::collections::BTreeMap;
use std::sync::Arc;
use std::sync::RwLock;

use tokio::sync::Mutex as TokioMutex;

use apeireth_core::kernel::{
    system_clock, ApprovalId, CapabilityId, Clock, PluginId, SessionId, Timestamp, TraceId,
};
use apeireth_governance::{DenyUnconfigured, GovernanceHook};
use apeireth_plugin::{
    CredentialResolver, NoCredentials, Plugin, PluginContext, PluginManager, ToolCapability,
};
use apeireth_protocol::canonical::{ModelDescriptor, ModelFeature};
use serde::Serialize;

use super::approval::PendingApprovalView;
use super::capability::CapabilityRegistry;
use super::error::{RuntimeError, RuntimeResult};
use super::events::{
    CompositeRuntimeEventSink, NoopRuntimeEventSink, RuntimeEvent, RuntimeEventSink,
};
use super::module::{BehaviorModule, BehaviorRegistry, DEFAULT_MAX_MODULE_INVOCATIONS};
use super::provider::ProviderRouter;
use super::session::{InMemorySessionStore, SessionManager, SessionStore};

/// How many logical execution rounds one turn may take before the runtime stops it.
///
/// This is a structural guard, not a policy: it applies even when no governance
/// is configured. Eight is enough for realistic tool chains and small enough that
/// a model or module stuck in a loop fails fast. Module retries consume a slot.
pub const DEFAULT_MAX_ROUNDS: u32 = 8;

/// Default lifetime of a pending approval before it expires.
pub const DEFAULT_APPROVAL_TTL_MS: u64 = 5 * 60 * 1000;

/// Runtime-wide settings.
#[derive(Debug, Clone)]
pub struct RuntimeConfig {
    /// Model used when a request does not name one.
    pub default_model: Option<String>,
    /// Logical round limit for one turn, including module retry attempts.
    pub max_rounds: u32,
    /// How long a pending approval stays resumable, in milliseconds.
    pub approval_ttl_ms: u64,
    /// Maximum isolated module provider calls in one top-level turn.
    pub max_module_invocations: usize,
}

/// Secret-free diagnostic projection of the live runtime graph.
#[derive(Debug, Clone, Serialize)]
pub struct RuntimeSnapshot {
    pub default_model: Option<String>,
    pub max_rounds: u32,
    pub max_module_invocations: usize,
    pub providers: Vec<RuntimeProviderSnapshot>,
    pub models: Vec<RuntimeModelSnapshot>,
    pub behavior_modules: Vec<RuntimeModuleSnapshot>,
    pub capabilities: Vec<RuntimeCapabilitySnapshot>,
}

#[derive(Debug, Clone, Serialize)]
pub struct RuntimeProviderSnapshot {
    pub id: String,
    pub available: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub health: Option<RuntimeHealthSnapshot>,
}

#[derive(Debug, Clone, Serialize)]
pub struct RuntimeHealthSnapshot {
    pub healthy: bool,
    pub latency_p50_ms: u64,
    pub error_rate: f64,
    pub consecutive_failures: u32,
}

#[derive(Debug, Clone, Serialize)]
pub struct RuntimeModelSnapshot {
    pub id: String,
    pub provider: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub display_name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub context_window: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_output_tokens: Option<u32>,
    pub features: Vec<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct RuntimeModuleSnapshot {
    pub id: String,
    pub name: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct RuntimeCapabilitySnapshot {
    pub id: String,
    pub name: String,
    pub owner: String,
    pub available: bool,
}

impl Default for RuntimeConfig {
    fn default() -> Self {
        Self {
            default_model: None,
            max_rounds: DEFAULT_MAX_ROUNDS,
            approval_ttl_ms: DEFAULT_APPROVAL_TTL_MS,
            max_module_invocations: DEFAULT_MAX_MODULE_INVOCATIONS,
        }
    }
}

/// Per-session serialization for turns and approval resolution.
///
/// This is deliberately not a global runtime mutex. Different sessions may
/// proceed concurrently; the same session cannot start a new turn or resolve
/// an approval while another operation on that session is still in progress.
#[derive(Debug, Default)]
pub struct SessionLocks {
    locks: TokioMutex<BTreeMap<SessionId, Arc<TokioMutex<()>>>>,
}

impl SessionLocks {
    pub(crate) async fn acquire(&self, session: SessionId) -> Arc<TokioMutex<()>> {
        let mut map = self.locks.lock().await;
        let entry = map
            .entry(session)
            .or_insert_with(|| Arc::new(TokioMutex::new(())));
        Arc::clone(entry)
    }
}

/// The assembled runtime.
pub struct Runtime {
    pub(super) plugins: PluginManager,
    // Arc so per-turn invoker handles can carry the one canonical router
    // without borrowing the runtime. Still exactly one router instance.
    pub(super) providers: Arc<ProviderRouter>,
    pub(super) sessions: SessionManager,
    pub(super) governance: Arc<dyn GovernanceHook>,
    pub(super) clock: Arc<dyn Clock>,
    pub(super) config: RuntimeConfig,
    pub(super) modules: BehaviorRegistry,
    pub(super) capabilities: CapabilityRegistry,
    pub(super) session_locks: SessionLocks,
    pub(super) event_sink: RwLock<Arc<dyn RuntimeEventSink>>,
}

impl Runtime {
    /// Start building a runtime.
    pub fn builder() -> RuntimeBuilder {
        RuntimeBuilder::new()
    }

    /// The plugin manager, and through it both canonical registries.
    pub fn plugins(&self) -> &PluginManager {
        &self.plugins
    }

    /// The session manager.
    pub fn sessions(&self) -> &SessionManager {
        &self.sessions
    }

    /// The governance hook every action is checked against.
    pub fn governance(&self) -> &Arc<dyn GovernanceHook> {
        &self.governance
    }

    /// The runtime's time source.
    pub fn clock(&self) -> &Arc<dyn Clock> {
        &self.clock
    }

    /// Runtime settings.
    pub fn config(&self) -> &RuntimeConfig {
        &self.config
    }

    /// Return a live, secret-free projection for diagnostics and capability
    /// discovery. It never includes credentials, prompts, tool arguments, or
    /// provider metadata that may contain vendor-specific secrets.
    pub fn snapshot(&self) -> RuntimeSnapshot {
        let providers = self
            .providers
            .provider_ids()
            .into_iter()
            .map(|id| {
                let health = self.providers.health(&id);
                RuntimeProviderSnapshot {
                    id: id.to_string(),
                    available: health.as_ref().is_none_or(|value| value.healthy),
                    health: health.map(|value| RuntimeHealthSnapshot {
                        healthy: value.healthy,
                        latency_p50_ms: value.latency_p50_ms,
                        error_rate: value.error_rate,
                        consecutive_failures: value.consecutive_failures,
                    }),
                }
            })
            .collect();
        let models = self
            .providers
            .model_descriptors()
            .into_iter()
            .map(|model: ModelDescriptor| RuntimeModelSnapshot {
                id: model.id.to_string(),
                provider: model.provider.to_string(),
                display_name: model.display_name,
                context_window: model.context_window,
                max_output_tokens: model.max_output_tokens,
                features: model
                    .features
                    .into_iter()
                    .map(|feature| match feature {
                        ModelFeature::ToolCalls => "tool_calls",
                        ModelFeature::Streaming => "streaming",
                        ModelFeature::Vision => "vision",
                        ModelFeature::SystemPrompt => "system_prompt",
                        ModelFeature::StructuredOutput => "structured_output",
                    })
                    .map(str::to_string)
                    .collect(),
            })
            .collect();
        let behavior_modules = self
            .modules()
            .iter()
            .map(|module| RuntimeModuleSnapshot {
                id: module.manifest().id.clone(),
                name: module.manifest().name.clone(),
            })
            .collect();
        let mut capabilities = self
            .capabilities
            .entries()
            .into_iter()
            .map(|(owner, capability)| RuntimeCapabilitySnapshot {
                id: capability.id().to_string(),
                name: capability.declaration().name,
                owner,
                available: true,
            })
            .collect::<Vec<_>>();
        capabilities.extend(self.plugins.active_tools().into_iter().map(|capability| {
            RuntimeCapabilitySnapshot {
                id: capability.id().to_string(),
                name: capability.declaration().name,
                owner: "plugin".to_string(),
                available: true,
            }
        }));
        RuntimeSnapshot {
            default_model: self.config.default_model.clone(),
            max_rounds: self.config.max_rounds,
            max_module_invocations: self.config.max_module_invocations,
            providers,
            models,
            behavior_modules,
            capabilities,
        }
    }

    /// Every tool that can currently be dispatched to.
    pub fn tools(&self) -> Vec<Arc<dyn ToolCapability>> {
        let mut tools = self.capabilities.capabilities();
        tools.extend(self.plugins.active_tools());
        tools
    }

    /// The runtime-owned registry for non-plugin capabilities.
    pub fn capability_registry(&self) -> &CapabilityRegistry {
        &self.capabilities
    }

    /// Replace the host event sink without changing the runtime graph.
    ///
    /// This is used by adapters that are assembled after the runtime. Event
    /// delivery is observational and never changes the turn result.
    pub fn set_event_sink(&self, sink: Arc<dyn RuntimeEventSink>) {
        *self
            .event_sink
            .write()
            .expect("runtime event sink poisoned") = sink;
    }

    /// Add an observer while preserving sinks installed by another
    /// composition root. Gateway setup uses this so dataset/audit observers
    /// attached during runtime bootstrap are not silently discarded.
    pub fn add_event_sink(&self, sink: Arc<dyn RuntimeEventSink>) {
        let existing = self
            .event_sink
            .read()
            .expect("runtime event sink poisoned")
            .clone();
        *self
            .event_sink
            .write()
            .expect("runtime event sink poisoned") =
            Arc::new(CompositeRuntimeEventSink::new(vec![existing, sink]));
    }

    pub(crate) fn emit_event(&self, event: RuntimeEvent) {
        self.event_sink
            .read()
            .expect("runtime event sink poisoned")
            .emit(event);
    }

    /// Register a dynamic tool on a named module after build.
    ///
    /// The tool is rejected if its capability id or model-facing name collides
    /// with any already-visible module or plugin tool.
    pub fn register_dynamic_tool(
        &self,
        module_id: &str,
        tool: Arc<dyn ToolCapability>,
    ) -> Result<(), RuntimeError> {
        let visible_plugins = self.plugins.active_tools();
        crate::canonical::module::reject_tool_identity_collisions(
            &visible_plugins,
            std::slice::from_ref(&tool),
            module_id,
        )
        .map_err(RuntimeError::misconfigured)?;
        self.capabilities
            .register(module_id, tool)
            .map_err(RuntimeError::misconfigured)
    }

    /// Model-facing tool declarations for all active tools.
    pub fn tool_declarations(&self) -> Vec<apeireth_protocol::canonical::NormalizedTool> {
        let mut declarations: Vec<apeireth_protocol::canonical::NormalizedTool> = self
            .capabilities
            .declarations()
            .into_iter()
            .collect::<Vec<_>>();
        declarations.extend(self.plugins.tool_declarations());
        declarations
    }

    /// The router over every provider that can currently serve a completion.
    ///
    /// Its members come from the capability registry, which is why the runtime
    /// can name no vendor: it knows only that some plugin declared
    /// `provider.something`.
    pub fn providers(&self) -> &ProviderRouter {
        &self.providers
    }

    /// The shared router behind an [`Arc`](std::sync::Arc), for turn-scoped
    /// invoker construction. This is the same single router instance the
    /// runtime serves every completion from.
    pub(super) fn providers_arc(&self) -> Arc<ProviderRouter> {
        Arc::clone(&self.providers)
    }

    /// The modules registered with this runtime, in execution order.
    pub fn modules(&self) -> &[Arc<dyn BehaviorModule>] {
        self.modules.modules()
    }

    /// The module registry holding all registered modules.
    pub fn module_registry(&self) -> &BehaviorRegistry {
        &self.modules
    }

    /// Pending approvals waiting on a human for one session.
    ///
    /// This is a read projection of session-owned governance state. It never
    /// executes a tool and never mutates policy.
    pub async fn pending_approvals(
        &self,
        session: SessionId,
    ) -> RuntimeResult<Vec<PendingApprovalView>> {
        let Some(loaded) = self.sessions.load(&session).await? else {
            return Ok(Vec::new());
        };
        let now = Timestamp::from_clock(self.clock.as_ref());
        Ok(loaded
            .approvals
            .values()
            .filter(|approval| approval.status.is_pending() && !approval.is_expired(now))
            .map(PendingApprovalView::from)
            .collect())
    }

    /// Stop every plugin in reverse start order.
    ///
    /// Returns the failures encountered; shutdown continues past each one.
    pub async fn shutdown(&mut self) -> Vec<apeireth_plugin::PluginError> {
        self.plugins.shutdown_all().await
    }
}

impl std::fmt::Debug for Runtime {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Runtime")
            .field("plugins", &self.plugins.plugins().len())
            .field("capabilities", &self.plugins.capabilities().len())
            .field("providers", &self.providers.len())
            .field("governance", &self.governance.name())
            .field("config", &self.config)
            .field("modules", &self.modules.len())
            .finish_non_exhaustive()
    }
}

/// Assembles a [`Runtime`].
///
/// Every dependency has a working default, so the smallest useful runtime is
/// `Runtime::builder().build().await`. Defaults are inert rather than
/// surprising: no credentials, fail-closed capability dispatch, memory-backed
/// sessions. Completions remain allowed so a zero-module kernel can still chat.
pub struct RuntimeBuilder {
    clock: Arc<dyn Clock>,
    credentials: Arc<dyn CredentialResolver>,
    session_store: Arc<dyn SessionStore>,
    governance: Arc<dyn GovernanceHook>,
    plugins: Vec<Arc<dyn Plugin>>,
    modules: Vec<Arc<dyn BehaviorModule>>,
    capabilities: Vec<Arc<dyn ToolCapability>>,
    event_sink: Arc<dyn RuntimeEventSink>,
    fallback_order: Option<Vec<CapabilityId>>,
    config: RuntimeConfig,
}

impl Default for RuntimeBuilder {
    fn default() -> Self {
        Self::new()
    }
}

impl RuntimeBuilder {
    /// A builder with default dependencies.
    pub fn new() -> Self {
        Self {
            clock: system_clock(),
            credentials: Arc::new(NoCredentials),
            session_store: Arc::new(InMemorySessionStore::new()),
            governance: Arc::new(DenyUnconfigured),
            plugins: Vec::new(),
            modules: Vec::new(),
            capabilities: Vec::new(),
            event_sink: Arc::new(NoopRuntimeEventSink),
            fallback_order: None,
            config: RuntimeConfig::default(),
        }
    }

    /// Use a specific time source.
    ///
    /// Supplying a virtual clock is what makes a turn's timing reproducible.
    #[must_use]
    pub fn with_clock(mut self, clock: Arc<dyn Clock>) -> Self {
        self.clock = clock;
        self
    }

    /// Supply secrets to plugins at start-up.
    #[must_use]
    pub fn with_credentials(mut self, credentials: Arc<dyn CredentialResolver>) -> Self {
        self.credentials = credentials;
        self
    }

    /// Use a specific session backend.
    #[must_use]
    pub fn with_session_store(mut self, store: Arc<dyn SessionStore>) -> Self {
        self.session_store = store;
        self
    }

    /// Check every action against this hook.
    #[must_use]
    pub fn with_governance(mut self, governance: Arc<dyn GovernanceHook>) -> Self {
        self.governance = governance;
        self
    }

    /// Add a plugin. Order is irrelevant; start order comes from declared
    /// dependencies.
    #[must_use]
    pub fn with_plugin(mut self, plugin: Arc<dyn Plugin>) -> Self {
        self.plugins.push(plugin);
        self
    }

    /// Register a cognitive module. Modules run in registration order.
    #[must_use]
    pub fn with_module(mut self, module: Arc<dyn BehaviorModule>) -> Self {
        self.modules.push(module);
        self
    }

    /// Compatibility alias that makes the behavior/capability distinction
    /// explicit at composition sites.
    #[must_use]
    pub fn with_behavior(self, module: Arc<dyn BehaviorModule>) -> Self {
        self.with_module(module)
    }

    /// Register a dispatchable capability independently of behavior hooks.
    #[must_use]
    pub fn with_capability(mut self, capability: Arc<dyn ToolCapability>) -> Self {
        self.capabilities.push(capability);
        self
    }

    /// Compatibility spelling for tool capability registration.
    #[must_use]
    pub fn with_tool(mut self, capability: Arc<dyn ToolCapability>) -> Self {
        self.capabilities.push(capability);
        self
    }

    /// Send canonical lifecycle events to a host-owned sink.
    #[must_use]
    pub fn with_event_sink(mut self, sink: Arc<dyn RuntimeEventSink>) -> Self {
        self.event_sink = sink;
        self
    }

    /// Order in which providers are tried.
    ///
    /// Providers absent from `order` remain usable and are tried after every
    /// listed one. Without this, providers are tried in registration order.
    #[must_use]
    pub fn with_fallback_order(mut self, order: Vec<CapabilityId>) -> Self {
        self.fallback_order = Some(order);
        self
    }

    /// Model used when a request does not name one.
    #[must_use]
    pub fn with_default_model(mut self, model: impl Into<String>) -> Self {
        self.config.default_model = Some(model.into());
        self
    }

    /// Round limit for one turn.
    #[must_use]
    pub fn with_max_rounds(mut self, rounds: u32) -> Self {
        self.config.max_rounds = rounds;
        self
    }

    /// Pending-approval lifetime, in milliseconds.
    #[must_use]
    pub fn with_approval_ttl(mut self, ttl_ms: u64) -> Self {
        self.config.approval_ttl_ms = ttl_ms;
        self
    }

    /// Set the per-turn budget for isolated module provider calls.
    #[must_use]
    pub fn with_max_module_invocations(mut self, invocations: usize) -> Self {
        self.config.max_module_invocations = invocations;
        self
    }

    /// Register the plugins, start them in dependency order, and assemble.
    ///
    /// Plugins are started here rather than lazily on first use, so that a
    /// misconfiguration surfaces at boot rather than inside somebody's first
    /// request.
    pub async fn build(self) -> RuntimeResult<Runtime> {
        if self.config.max_rounds == 0 {
            return Err(RuntimeError::misconfigured(
                "max_rounds must be at least 1, otherwise no turn can ever run",
            ));
        }
        if self.config.max_module_invocations == 0 {
            return Err(RuntimeError::misconfigured(
                "max_module_invocations must be at least 1",
            ));
        }
        let mut module_registry = BehaviorRegistry::new();
        for module in self.modules {
            module_registry
                .register(module)
                .map_err(RuntimeError::misconfigured)?;
        }

        let capability_registry = CapabilityRegistry::new();
        capability_registry
            .register_all_for_builder(self.capabilities)
            .map_err(RuntimeError::misconfigured)?;

        let mut manager = PluginManager::new();
        for plugin in self.plugins {
            manager.register(plugin)?;
        }

        let ctx = PluginContext::new(
            Arc::clone(&self.clock),
            Arc::clone(&self.credentials),
            TraceId::new(),
        );
        manager.start_all(&ctx).await?;

        crate::canonical::module::reject_tool_identity_collisions(
            &capability_registry.capabilities(),
            &manager.active_tools(),
            "plugin tools",
        )
        .map_err(RuntimeError::misconfigured)?;

        // Providers are read out of the capability registry *after* start-up, so
        // the router contains exactly those whose plugins actually came up. A
        // provider whose plugin failed to initialize is never routed to.
        let mut providers =
            ProviderRouter::new(manager.active_providers(), Arc::clone(&self.clock));
        if let Some(order) = self.fallback_order {
            providers = providers.with_fallback_order(order);
        }

        Ok(Runtime {
            plugins: manager,
            providers: Arc::new(providers),
            sessions: SessionManager::new(self.session_store, Arc::clone(&self.clock)),
            governance: self.governance,
            clock: self.clock,
            config: self.config,
            modules: module_registry,
            capabilities: capability_registry,
            session_locks: SessionLocks::default(),
            event_sink: RwLock::new(self.event_sink),
        })
    }
}

/// Plugin ids known to a runtime, in id order. Convenience for diagnostics.
pub fn plugin_ids(runtime: &Runtime) -> Vec<&PluginId> {
    runtime.plugins.plugins().ids().collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use apeireth_core::kernel::{Lifecycle, Timestamp, VirtualClock};
    use apeireth_governance::{DenyCapabilities, GovernancePipeline};
    use apeireth_plugin::{
        CapabilityKind, PluginManifest, PluginResult, StaticCredentials, ToolCapability,
    };
    use apeireth_protocol::canonical::{NormalizedTool, ToolCall, ToolParameters, ToolResult};
    use async_trait::async_trait;
    use std::sync::Mutex;

    struct Echo(CapabilityId);

    #[async_trait]
    impl ToolCapability for Echo {
        fn id(&self) -> &CapabilityId {
            &self.0
        }
        fn declaration(&self) -> NormalizedTool {
            NormalizedTool {
                name: "echo".into(),
                description: Some("echo".into()),
                parameters: ToolParameters::new(),
                strict: false,
            }
        }
        async fn invoke(&self, call: &ToolCall) -> ToolResult {
            ToolResult::ok(&call.id, call.arguments.clone())
        }
    }

    /// Captures whatever credential it was offered at start-up.
    struct KeyReader {
        manifest: PluginManifest,
        seen: Mutex<Option<String>>,
    }

    #[async_trait]
    impl Plugin for KeyReader {
        fn manifest(&self) -> &PluginManifest {
            &self.manifest
        }
        async fn initialize(&self, ctx: &PluginContext) -> PluginResult<()> {
            *self.seen.lock().unwrap() = ctx
                .credentials
                .resolve("provider.fake.api_key")
                .map(|s| s.expose().to_string());
            Ok(())
        }
        async fn shutdown(&self) -> PluginResult<()> {
            Ok(())
        }
        fn tools(&self) -> Vec<Arc<dyn ToolCapability>> {
            vec![Arc::new(Echo(CapabilityId::new("tool.echo").unwrap()))]
        }
    }

    fn key_reader() -> Arc<KeyReader> {
        Arc::new(KeyReader {
            manifest: PluginManifest::new(
                PluginId::new("builtin.key_reader").unwrap(),
                "1.0.0",
                "reads a key at boot",
            )
            .declare_capability(
                CapabilityId::new("tool.echo").unwrap(),
                CapabilityKind::Tool,
                "echo",
            )
            .unwrap(),
            seen: Mutex::new(None),
        })
    }

    #[tokio::test]
    async fn the_smallest_runtime_builds_with_no_arguments() {
        let runtime = Runtime::builder().build().await.unwrap();
        assert_eq!(runtime.plugins().plugins().len(), 0);
        assert!(runtime.tools().is_empty());
        assert!(runtime.providers().is_empty());
        assert_eq!(runtime.config().max_rounds, DEFAULT_MAX_ROUNDS);
    }

    #[tokio::test]
    async fn a_runtime_builds_and_boots_without_any_credentials() {
        let plugin = key_reader();
        let runtime = Runtime::builder()
            .with_plugin(plugin.clone())
            .build()
            .await
            .unwrap();

        assert_eq!(
            runtime
                .plugins()
                .state(&PluginId::new("builtin.key_reader").unwrap())
                .unwrap(),
            Lifecycle::Active,
            "no key must not prevent boot"
        );
        assert_eq!(*plugin.seen.lock().unwrap(), None);
        assert_eq!(runtime.tools().len(), 1);
    }

    #[tokio::test]
    async fn credentials_reach_plugins_through_the_injected_resolver() {
        let plugin = key_reader();
        let _runtime = Runtime::builder()
            .with_plugin(plugin.clone())
            .with_credentials(Arc::new(
                StaticCredentials::new().with("provider.fake.api_key", "sk-injected"),
            ))
            .build()
            .await
            .unwrap();

        assert_eq!(
            plugin.seen.lock().unwrap().as_deref(),
            Some("sk-injected"),
            "the plugin must receive the key without the runtime storing it"
        );
    }

    #[tokio::test]
    async fn the_runtime_never_prints_a_secret() {
        let runtime = Runtime::builder()
            .with_plugin(key_reader())
            .with_credentials(Arc::new(
                StaticCredentials::new().with("provider.fake.api_key", "sk-injected"),
            ))
            .build()
            .await
            .unwrap();

        let printed = format!("{runtime:?}");
        assert!(!printed.contains("sk-injected"), "{printed}");
    }

    #[tokio::test]
    async fn the_injected_clock_is_the_one_sessions_are_stamped_with() {
        let clock: Arc<dyn Clock> = Arc::new(VirtualClock::new(
            Timestamp::from_epoch_millis(1_700_000_000_000)
                .unwrap()
                .as_datetime(),
        ));
        let runtime = Runtime::builder().with_clock(clock).build().await.unwrap();

        let session = runtime
            .sessions()
            .load_or_create(apeireth_core::kernel::SessionId::new())
            .await
            .unwrap();
        assert_eq!(session.created_at.epoch_millis(), 1_700_000_000_000);
    }

    #[tokio::test]
    async fn a_zero_round_limit_is_rejected_at_build() {
        let err = Runtime::builder()
            .with_max_rounds(0)
            .build()
            .await
            .unwrap_err();
        assert!(matches!(err, RuntimeError::Misconfigured(_)), "{err}");
    }

    #[tokio::test]
    async fn a_duplicate_plugin_is_rejected_at_build() {
        let err = Runtime::builder()
            .with_plugin(key_reader())
            .with_plugin(key_reader())
            .build()
            .await
            .unwrap_err();
        assert!(matches!(err, RuntimeError::Plugin(_)), "{err}");
    }

    #[tokio::test]
    async fn governance_is_held_as_configured() {
        let runtime = Runtime::builder()
            .with_governance(Arc::new(GovernancePipeline::new().with(Arc::new(
                DenyCapabilities::new().deny(CapabilityId::new("tool.example").unwrap()),
            ))))
            .build()
            .await
            .unwrap();
        assert_eq!(runtime.governance().name(), "pipeline");
    }

    #[tokio::test]
    async fn default_governance_is_fail_closed_for_capabilities() {
        let runtime = Runtime::builder().build().await.unwrap();
        assert_eq!(runtime.governance().name(), "deny_unconfigured");
    }

    #[tokio::test]
    async fn shutdown_stops_the_plugins_it_started() {
        let mut runtime = Runtime::builder()
            .with_plugin(key_reader())
            .build()
            .await
            .unwrap();
        let id = PluginId::new("builtin.key_reader").unwrap();

        assert_eq!(runtime.plugins().state(&id).unwrap(), Lifecycle::Active);
        assert!(runtime.shutdown().await.is_empty());
        assert_eq!(runtime.plugins().state(&id).unwrap(), Lifecycle::Stopped);
    }

    #[tokio::test]
    async fn plugin_ids_are_reported_in_id_order() {
        let runtime = Runtime::builder()
            .with_plugin(key_reader())
            .build()
            .await
            .unwrap();
        let ids: Vec<&str> = plugin_ids(&runtime).iter().map(|i| i.as_str()).collect();
        assert_eq!(ids, ["builtin.key_reader"]);
    }
}
