//! Canonical Apeireth command-line entry points.
//!
//! The CLI is a thin adapter: it bootstraps one canonical runtime, constructs
//! a canonical turn request, and delegates execution to
//! `Runtime::execute_outcome`. Pending approvals keep their `ApprovalId`.

// v2.0.0-rc.1 RC-9: KeyringSelector 真接 OS keyring / EncryptedFile backend
// (per `v2.0.0-rc-roadmap.md` §3 RC-9: "keyring 真正接到 EnvCredentialResolver 之前").
// 0 装诚实: 4 backend + KeyringSelector alpha 已真 impl; 本模块只做 bootstrap 集成.
pub mod gateway_panels;
pub mod keyring_bootstrap;
pub mod portable_bundle;

pub use portable_bundle::{PortableBundleManifest, PortableBundleSynthesizer};

use std::path::PathBuf;
use std::sync::Arc;

use apeireth_core::kernel::{ApprovalId, CapabilityId, SessionId};
use apeireth_governance::{
    CredentialDisclosureHook, GovernancePipeline, Permission, PermissionGovernanceHook,
    PermissionPolicy, PromptInjectionHook,
};
use apeireth_plugin::memory_backend::MemoryBackend;
use apeireth_runtime::canonical::{
    ApprovalDecision, ApprovalResolution, PendingApprovalView, Runtime, SessionStore, TurnOutcome,
    TurnRequest, TurnResponse,
};
use apeireth_runtime_assembly::SqliteSessionStore;

/// One persistent SQLite database is shared by the cognitive backends.
/// `APEIRETH_COGNITIVE_DB` may override the path; Judge remains opt-in.
const COGNITIVE_DB_ENV: &str = "APEIRETH_COGNITIVE_DB";
const SESSION_DB_ENV: &str = "APEIRETH_SESSION_DB";
const COGNITIVE_JUDGE_ENV: &str = "APEIRETH_COGNITIVE_JUDGE";
const COGNITIVE_COUNCIL_ENV: &str = "APEIRETH_COGNITIVE_COUNCIL";

/// Enables the local filesystem and search tools in the production policy.
pub const ENABLE_LOCAL_READ_TOOLS_ENV: &str = "APEIRETH_ENABLE_LOCAL_READ_TOOLS";

/// Build the production governance policy from an explicit local-read choice.
///
/// The explicit boolean keeps the policy deterministic and easy to test. The
/// environment-facing wrapper is [`build_production_governance_from_env`].
/// Authorization is deliberately the first hook: a later content-risk hook
/// must never turn an unauthorized capability into an approval request.
pub fn build_production_governance(enable_local_read_tools: bool) -> GovernancePipeline {
    build_production_governance_parts(enable_local_read_tools).0
}

use apeireth_guard::BehaviorChainGuardHook;

/// Build the production pipeline and return the shared policy handle alongside
/// it, so the gateway can serve grants listing and session-scoped hot revoke
/// against the same policy the live hooks evaluate.
pub fn build_production_governance_parts(
    enable_local_read_tools: bool,
) -> (
    GovernancePipeline,
    Arc<std::sync::Mutex<PermissionPolicy>>,
    Arc<BehaviorChainGuardHook>,
) {
    let mut policy = PermissionPolicy::new();
    policy.grant(Permission::ExecuteTool("tool.repo".to_string()));
    if enable_local_read_tools {
        policy.grant(Permission::ExecuteTool("tool.filesystem".to_string()));
        policy.grant(Permission::ExecuteTool("tool.search".to_string()));
    }
    let policy = Arc::new(std::sync::Mutex::new(policy));
    let guard_hook = Arc::new(BehaviorChainGuardHook::new());

    let pipeline = GovernancePipeline::new()
        .with(Arc::new(PermissionGovernanceHook::new_shared(
            policy.clone(),
        )))
        .with(Arc::new(CredentialDisclosureHook::new()))
        .with(Arc::new(PromptInjectionHook::new()))
        .with(guard_hook.clone());
    (pipeline, policy, guard_hook)
}

/// Build the production governance policy using the process environment.
///
/// Production is default-deny for capability execution. Only the exact value
/// `1` enables the two local read tools; shell, fetch, and unknown capabilities
/// remain denied even if a future plugin registers them.
pub fn build_production_governance_from_env() -> GovernancePipeline {
    let enable_local_read_tools = std::env::var(ENABLE_LOCAL_READ_TOOLS_ENV)
        .ok()
        .is_some_and(|value| value.trim() == "1");
    build_production_governance(enable_local_read_tools)
}

fn build_production_governance_parts_from_env() -> (
    GovernancePipeline,
    Arc<std::sync::Mutex<PermissionPolicy>>,
    Arc<BehaviorChainGuardHook>,
) {
    let enable_local_read_tools = std::env::var(ENABLE_LOCAL_READ_TOOLS_ENV)
        .ok()
        .is_some_and(|value| value.trim() == "1");
    build_production_governance_parts(enable_local_read_tools)
}

/// Build the one canonical runtime used by CLI chat and the HTTP gateway.
///
/// Provider implementations are injected as plugins. Credentials are resolved
/// at execution time, so neither the runtime nor a provider stores API keys.
pub async fn build_canonical_runtime_from_env() -> Result<Runtime, String> {
    let (runtime, _, _, _, _) = build_canonical_runtime_with_sessions_from_env().await?;
    Ok(runtime)
}

/// Build the runtime and return the shared session-store, memory-backend and
/// permission-policy handles alongside it, so the gateway can serve the
/// sessions/memory/permissions introspection surfaces from the same durable
/// stores and the same live policy.
pub async fn build_canonical_runtime_with_sessions_from_env() -> Result<
    (
        Runtime,
        Arc<dyn SessionStore>,
        Arc<dyn MemoryBackend>,
        Arc<std::sync::Mutex<PermissionPolicy>>,
        Arc<BehaviorChainGuardHook>,
    ),
    String,
> {
    let session_store = production_session_store().await?;
    let clock: Arc<dyn apeireth_core::kernel::Clock> = apeireth_core::kernel::system_clock();
    let (cognitive, memory) = build_cognitive_modules_from_env(Arc::clone(&clock)).await?;
    let (runtime, policy, guard_hook) =
        build_canonical_runtime_with_parts(session_store.clone(), cognitive, clock).await?;
    Ok((runtime, session_store, memory, policy, guard_hook))
}

async fn build_canonical_runtime_with_parts(
    session_store: Arc<dyn SessionStore>,
    cognitive: apeireth_runtime_assembly::ProductionCognitiveModules,
    clock: Arc<dyn apeireth_core::kernel::Clock>,
) -> Result<
    (
        Runtime,
        Arc<std::sync::Mutex<PermissionPolicy>>,
        Arc<BehaviorChainGuardHook>,
    ),
    String,
> {
    use apeireth_provider::canonical_anthropic::AnthropicProviderPlugin;
    use apeireth_provider::canonical_minimax::MinimaxProviderPlugin;
    use apeireth_provider::canonical_openai_compatible::OpenAiCompatibleProviderPlugin;

    let configured_model = std::env::var("APEIRETH_MODEL")
        .ok()
        .filter(|model| !model.trim().is_empty());

    let mut builder = Runtime::builder().with_clock(Arc::clone(&clock));
    // P-arch (2026-08-27) + v2.0.0-rc.1 RC-9: KeyringSelector 真接 OS keyring
    // 优先用 keyring (设 APEIRETH_KEYRING_BACKEND env), fallback 到 EnvCredentialResolver
    // (alpha 0 装路径, 0 行为变化). 详见 `keyring_bootstrap` 模块.
    let resolver: Arc<dyn apeireth_plugin::CredentialResolver> =
        keyring_bootstrap::build_keyring_resolver();
    builder = builder.with_credentials(resolver);
    let (governance, policy, guard_hook) = build_production_governance_parts_from_env();
    builder = builder.with_governance(Arc::new(governance));
    builder = builder.with_session_store(session_store);

    // The CLI is the composition root. Gateway reuses this function, while
    // SDK remains an HTTP client and does not host a second Runtime.
    // Builtin tools are owned by ProductionModules, not BuiltinToolsPlugin.
    builder = cognitive.register_into(builder);

    let first_default_model: Option<String>;
    let mut fallback_order: Vec<CapabilityId> = Vec::new();

    let minimax = MinimaxProviderPlugin::from_env()
        .map_err(|error| format!("minimax provider activation failed: {error}"))?;
    first_default_model = minimax.model_ids().first().cloned();
    fallback_order.push(CapabilityId::new("provider.minimax").unwrap());
    builder = builder.with_plugin(Arc::new(minimax));

    let anthropic = AnthropicProviderPlugin::from_env()
        .map_err(|error| format!("anthropic provider activation failed: {error}"))?;
    fallback_order.push(CapabilityId::new("provider.anthropic").unwrap());
    builder = builder.with_plugin(Arc::new(anthropic));

    if std::env::var("APEIRETH_OPENAI_MODELS")
        .ok()
        .as_ref()
        .is_some_and(|models| !models.trim().is_empty())
    {
        let openai = OpenAiCompatibleProviderPlugin::from_env()
            .map_err(|error| format!("openai-compatible provider activation failed: {error}"))?;
        fallback_order.push(CapabilityId::new("provider.openai-compatible").unwrap());
        builder = builder.with_plugin(Arc::new(openai));
    }

    builder = builder.with_fallback_order(fallback_order);
    if let Some(model) = configured_model.or(first_default_model) {
        builder = builder.with_default_model(model);
    }

    let runtime = builder
        .build()
        .await
        .map_err(|error| format!("canonical runtime bootstrap failed: {error}"))?;
    Ok((runtime, policy, guard_hook))
}

async fn production_session_store() -> Result<Arc<dyn apeireth_runtime::SessionStore>, String> {
    let path = std::env::var(SESSION_DB_ENV)
        .ok()
        .filter(|value| !value.trim().is_empty())
        .unwrap_or_else(|| ".apeireth/sessions.sqlite3".into());
    SqliteSessionStore::open(&path)
        .await
        .map(|store| Arc::new(store) as Arc<dyn apeireth_runtime::SessionStore>)
        .map_err(|error| format!("session store open failed: {error}"))
}

/// Build the direct CLI runtime with the same trace/audit observer used by the
/// HTTP gateway. The CLI has no SSE transport, but its turns must still be
/// observable and durable through the gateway bounded-context ports.
async fn build_canonical_runtime_from_env_with_observability(
) -> Result<(Arc<Runtime>, Arc<apeireth_gateway::RuntimeObservationSink>), String> {
    let (runtime, sessions, memory, policy, guard_hook) =
        build_canonical_runtime_with_sessions_from_env().await?;
    let runtime = Arc::new(runtime);
    let governance = Arc::new(
        apeireth_memory::SqliteMemoryStore::open(cognitive_db_path())
            .map_err(|error| format!("memory governance store open failed: {error}"))?,
    );
    let enable_local_read_tools = std::env::var(ENABLE_LOCAL_READ_TOOLS_ENV)
        .ok()
        .is_some_and(|value| value.trim() == "1");
    let panel = Arc::new(
        crate::gateway_panels::CliPanelData::new_with_runtime(
            Arc::clone(&runtime),
            sessions,
            memory,
            governance,
            policy,
            enable_local_read_tools,
            default_panel_data_dir(),
        )
        .with_guard(guard_hook),
    );
    let services = crate::gateway_panels::gateway_services(panel);
    let observer = Arc::new(apeireth_gateway::RuntimeObservationSink::new(
        services.trace_commands.clone(),
        services.audit_commands.clone(),
    ));
    runtime.set_event_sink(observer.clone());
    Ok((runtime, observer))
}

/// Completed turn or a pending approval that still has its [`ApprovalId`].
#[derive(Debug, Clone)]
pub enum CanonicalCliTurn {
    /// The turn reached a final assistant response.
    Completed(TurnResponse),
    /// The turn is suspended until a human resolves the returned approval.
    PendingApproval(PendingApprovalView),
}

async fn build_cognitive_modules_from_env(
    clock: Arc<dyn apeireth_core::kernel::Clock>,
) -> Result<
    (
        apeireth_runtime_assembly::ProductionCognitiveModules,
        Arc<dyn MemoryBackend>,
    ),
    String,
> {
    use apeireth_memory::backend::sqlite::SqliteBackend;
    use apeireth_memory::{
        experience_store_sqlite::SQLiteExperienceStore,
        preference_store_sqlite::SQLitePreferenceStore,
        self_assessment_store_sqlite::SQLiteSelfAssessmentStore,
    };
    use apeireth_runtime_assembly::{CognitiveBackends, CognitiveModuleConfig, JudgeConfig};
    use apeireth_storage::{SqliteConnectionPool, StorageError};

    let path = cognitive_db_path();
    let pool = Arc::new(
        SqliteConnectionPool::open(&path)
            .await
            .map_err(|error| format!("cognitive backend open failed: {error}"))?,
    );

    // The storage foundation has an older generic `episodes(id, data)` table.
    // Refuse that shape explicitly instead of letting an additive
    // `CREATE IF NOT EXISTS` migration produce a runtime write failure.
    pool.read(|conn| {
        let mut statement = conn.prepare("PRAGMA table_info(episodes)")?;
        let columns = statement
            .query_map([], |row| row.get::<_, String>(1))?
            .collect::<Result<Vec<_>, _>>()?;
        if !columns.is_empty()
            && ["id", "timestamp", "role", "content", "session_id"]
                .iter()
                .any(|required| !columns.iter().any(|column| column == required))
        {
            return Err(apeireth_storage::StorageError::InvalidConfiguration(
                "cognitive database uses incompatible generic episodes schema; migrate or choose a new APEIRETH_COGNITIVE_DB path".into(),
            ));
        }
        Ok(())
    })
    .map_err(|error| format!("cognitive database schema validation failed: {error}"))?;

    // Memory migrations own the episode and six-stream tables. The preference,
    // experience, and assessment stores own their additive tables. All use
    // this one injected pool; no module opens a connection itself.
    let migration_pool = Arc::clone(&pool);
    migration_pool
        .write(|conn| {
            apeireth_memory::run_migrations(conn).map_err(|error| StorageError::Migration {
                version: 0,
                name: "cognitive_memory",
                message: error.to_string(),
            })
        })
        .await
        .map_err(|error| format!("cognitive memory schema failed: {error}"))?;

    let experience = Arc::new(SQLiteExperienceStore::from_arc(Arc::clone(&pool)));
    experience
        .ensure_schema()
        .await
        .map_err(|error| format!("cognitive experience schema failed: {error}"))?;
    let preferences = Arc::new(SQLitePreferenceStore::from_arc(Arc::clone(&pool)));
    preferences
        .ensure_schema()
        .await
        .map_err(|error| format!("cognitive preference schema failed: {error}"))?;
    let self_assessments = Arc::new(SQLiteSelfAssessmentStore::from_arc(Arc::clone(&pool)));
    self_assessments
        .ensure_schema()
        .await
        .map_err(|error| format!("cognitive assessment schema failed: {error}"))?;

    let judge_enabled = std::env::var(COGNITIVE_JUDGE_ENV)
        .ok()
        .is_some_and(|value| value.trim() == "1");
    let council_enabled = std::env::var(COGNITIVE_COUNCIL_ENV)
        .ok()
        .is_some_and(|value| value.trim() == "1");
    let config = CognitiveModuleConfig {
        judge: JudgeConfig {
            enabled: judge_enabled,
            ..JudgeConfig::default()
        },
        council: council_enabled,
        ..CognitiveModuleConfig::default()
    };
    let memory: Arc<dyn apeireth_plugin::memory_backend::MemoryBackend> =
        Arc::new(SqliteBackend::from_arc(Arc::clone(&pool)));
    let wiki: Arc<dyn apeireth_plugin::experience::WikiEntryStore> = experience.clone();
    let graph: Arc<dyn apeireth_plugin::experience::KnowledgeGraphStore> = experience.clone();
    let associations: Arc<dyn apeireth_plugin::experience::AssociationStore> = experience.clone();
    let preferences: Arc<dyn apeireth_plugin::preference::PreferenceStore> = preferences.clone();
    let self_assessments: Arc<dyn apeireth_plugin::self_assessment::SelfAssessmentStore> =
        self_assessments.clone();
    let council = if council_enabled {
        Some(Arc::new(apeireth_orchestration::Council::default_llm()))
    } else {
        None
    };
    let backends = CognitiveBackends {
        memory: Some(memory.clone()),
        wiki: Some(wiki),
        graph: Some(graph),
        associations: Some(associations),
        preferences: Some(preferences),
        self_assessments: Some(self_assessments),
        council,
        workspace_root: std::env::current_dir().ok(),
    };
    let modules =
        apeireth_runtime_assembly::ProductionCognitiveModules::build(config, backends, clock)
            .map_err(|error| error.to_string())?;
    Ok((modules, memory))
}

fn cognitive_db_path() -> PathBuf {
    std::env::var(COGNITIVE_DB_ENV)
        .ok()
        .filter(|value| !value.trim().is_empty())
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(".apeireth/cognitive.sqlite3"))
}

/// Execute one CLI turn directly through [`Runtime::execute_outcome`].
pub async fn execute_canonical_cli_turn(
    runtime: &Runtime,
    prompt: impl Into<String>,
    model: Option<String>,
    session: Option<SessionId>,
) -> Result<CanonicalCliTurn, String> {
    let mut request = TurnRequest::new(session.unwrap_or_else(SessionId::new), prompt);
    if let Some(model) = model {
        request = request.with_model(model);
    }
    match runtime
        .execute_outcome(request)
        .await
        .map_err(|error| error.to_string())?
    {
        TurnOutcome::Completed(response) => Ok(CanonicalCliTurn::Completed(response)),
        TurnOutcome::PendingApproval(view) => Ok(CanonicalCliTurn::PendingApproval(view)),
    }
}

/// Resolve a pending approval through the canonical runtime API.
pub async fn resolve_canonical_cli_approval(
    runtime: &Runtime,
    session: SessionId,
    approval: ApprovalId,
    decision: ApprovalDecision,
) -> Result<ApprovalResolution, String> {
    runtime
        .resolve_approval(session, approval, decision)
        .await
        .map_err(|error| error.to_string())
}

/// Bootstrap and execute the canonical CLI chat path.
pub async fn dispatch_canonical_chat(
    prompt: impl Into<String>,
    model: Option<String>,
    session: Option<String>,
) -> Result<CanonicalCliTurn, String> {
    let session = session
        .map(|id| id.parse::<SessionId>().map_err(|error| error.to_string()))
        .transpose()?;
    let (runtime, observer) = build_canonical_runtime_from_env_with_observability().await?;
    let result = execute_canonical_cli_turn(&runtime, prompt, model, session).await;
    observer.flush().await;
    result
}

/// Bootstrap and resolve a pending approval on the production session store.
pub async fn dispatch_canonical_approval(
    session: String,
    approval: String,
    decision: ApprovalDecision,
) -> Result<ApprovalResolution, String> {
    let session = session
        .parse::<SessionId>()
        .map_err(|error| error.to_string())?;
    let approval = approval
        .parse::<ApprovalId>()
        .map_err(|error| error.to_string())?;
    let (runtime, observer) = build_canonical_runtime_from_env_with_observability().await?;
    let result = resolve_canonical_cli_approval(&runtime, session, approval, decision).await;
    observer.flush().await;
    result
}

/// Start the HTTP Gateway backed by one long-lived canonical runtime.
/// Blocks until the server exits.
pub async fn dispatch_gateway_serve(port: u16) -> Result<String, String> {
    dispatch_gateway_serve_on("127.0.0.1", port).await
}

/// Start the HTTP Gateway on an explicitly selected bind address.
///
/// Loopback is the safe default. Binding a non-loopback address is an
/// intentional operator decision and is called out before the listener starts.
pub async fn dispatch_gateway_serve_on(bind: &str, port: u16) -> Result<String, String> {
    let (runtime, sessions, memory, policy, guard_hook) =
        build_canonical_runtime_with_sessions_from_env().await?;
    let runtime = Arc::new(runtime);
    let governance = Arc::new(
        apeireth_memory::SqliteMemoryStore::open(cognitive_db_path())
            .map_err(|error| format!("memory governance store open failed: {error}"))?,
    );
    let enable_local_read_tools = std::env::var(ENABLE_LOCAL_READ_TOOLS_ENV)
        .ok()
        .is_some_and(|value| value.trim() == "1");
    let panel = Arc::new(
        crate::gateway_panels::CliPanelData::new_with_runtime(
            Arc::clone(&runtime),
            sessions,
            memory,
            governance,
            policy,
            enable_local_read_tools,
            default_panel_data_dir(),
        )
        .with_guard(guard_hook),
    );
    let services = crate::gateway_panels::gateway_services(panel);
    let address = format!("{bind}:{port}");
    let listener = tokio::net::TcpListener::bind(&address)
        .await
        .map_err(|error| format!("bind {address} failed: {error}"))?;
    let local_addr = listener
        .local_addr()
        .map_err(|error| format!("local_addr: {error}"))?;
    let url = format!("http://{local_addr}");

    if local_addr.ip().is_loopback() {
        eprintln!("canonical gateway started at {url}");
    } else {
        eprintln!(
            "WARNING: canonical gateway is exposed on non-loopback address {local_addr}; use this only on a trusted network"
        );
    }
    apeireth_gateway::serve_canonical_with_services(listener, runtime, services)
        .await
        .map_err(|error| format!("gateway server failed: {error}"))?;

    Ok(format!("server stopped at {url}"))
}

/// Data directory for panel archives. `APEIRETH_DATA_DIR` overrides; the
/// default is `~/.apeireth` (same place as the keyring and session dbs).
fn default_panel_data_dir() -> PathBuf {
    if let Ok(dir) = std::env::var("APEIRETH_DATA_DIR") {
        if !dir.trim().is_empty() {
            return PathBuf::from(dir);
        }
    }
    let home = std::env::var("USERPROFILE")
        .or_else(|_| std::env::var("HOME"))
        .unwrap_or_else(|_| ".".to_string());
    PathBuf::from(home).join(".apeireth")
}
