//! End-to-end coverage for the first production organ ownership wiring.
//!
//! `OrganModule` (id `cognitive.organs`) is the ONE canonical module owning
//! the persistent organ cognition backend; W1/W2 are rebuilt transiently per
//! invocation from `ctx.invoker_handle()`. These tests lock the ownership
//! invariants: default-off composition, single module, no tools, AfterTurn
//! only, real session ids, transient W1/W2, fail-open governance refusals,
//! untouched primary transcripts, and cross-session isolation.

use apeireth_runtime_assembly as apeireth_runtime;

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use apeireth_core::clock::SystemClock;
use apeireth_core::kernel::{CapabilityId, Clock, ModelId, PluginId, SessionId};
use apeireth_governance::{Decision, GovernanceHook, GovernanceRequest};
use apeireth_plugin::memory_backend::{BackendKind, CapabilityResult};
use apeireth_plugin::preference::UserPreference;
use apeireth_plugin::self_assessment::SelfAssessment;
use apeireth_plugin::{
    CapabilityKind, Plugin, PluginContext, PluginManifest, PluginResult, ProviderCapability,
    ProviderError,
};
use apeireth_protocol::canonical::{
    ContentPart, ModelDescriptor, NormalizedFinishReason, NormalizedMessage, NormalizedRequest,
    NormalizedResponse, NormalizedUsage,
};
use apeireth_runtime::canonical::{
    AgentModule, CognitiveBackends, CognitiveModuleConfig, OrganModule, ProductionCognitiveModules,
    Runtime, TurnOutcome, TurnRequest, ORGAN_MODULE_ID,
};
use async_trait::async_trait;

// Organ-side fake session id sentinels.
const NIL_SESSION: &str = "00000000-0000-0000-0000-000000000000";

// ---------------------------------------------------------------------
// Scripted provider recording every request
// ---------------------------------------------------------------------

struct ScriptedProvider {
    id: CapabilityId,
    calls: AtomicUsize,
    requests: Mutex<Vec<(String, String)>>, // (model, joined text)
}

impl ScriptedProvider {
    fn new() -> Arc<Self> {
        Arc::new(Self {
            id: CapabilityId::new("provider.fake").unwrap(),
            calls: AtomicUsize::new(0),
            requests: Mutex::new(Vec::new()),
        })
    }

    fn call_count(&self) -> usize {
        self.calls.load(Ordering::SeqCst)
    }

    fn requests(&self) -> Vec<(String, String)> {
        self.requests.lock().unwrap().clone()
    }
}

#[async_trait]
impl ProviderCapability for ScriptedProvider {
    fn id(&self) -> &CapabilityId {
        &self.id
    }

    fn models(&self) -> Vec<ModelDescriptor> {
        ["fake-model-1", "model-a", "model-b"]
            .into_iter()
            .map(|m| ModelDescriptor::new(ModelId::new(m).unwrap(), self.id.clone()))
            .collect()
    }

    async fn complete(
        &self,
        request: &NormalizedRequest,
    ) -> Result<NormalizedResponse, ProviderError> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        let text = request
            .messages
            .iter()
            .map(|m| ContentPart::join_text(&m.content))
            .collect::<Vec<_>>()
            .join("\n");
        self.requests
            .lock()
            .unwrap()
            .push((request.model.clone(), text));
        Ok(NormalizedResponse {
            id: format!("response-{}", self.call_count()),
            model: request.model.clone(),
            content: format!(
                "echo: {}",
                request
                    .messages
                    .last()
                    .map(|m| ContentPart::join_text(&m.content))
                    .unwrap_or_default()
            ),
            finish_reason: Some(NormalizedFinishReason::Stop),
            usage: NormalizedUsage::default(),
            tool_calls: Vec::new(),
            raw_metadata: serde_json::Map::new(),
        })
    }
}

struct ProviderPlugin {
    manifest: PluginManifest,
    provider: Arc<ScriptedProvider>,
}

impl ProviderPlugin {
    fn new(provider: Arc<ScriptedProvider>) -> Arc<Self> {
        Arc::new(Self {
            manifest: PluginManifest::new(
                PluginId::new("builtin.fake_provider").unwrap(),
                "1.0.0",
                "fake provider",
            )
            .declare_capability(
                provider.id.clone(),
                CapabilityKind::Provider,
                "fake provider",
            )
            .unwrap(),
            provider,
        })
    }
}

#[async_trait]
impl Plugin for ProviderPlugin {
    fn manifest(&self) -> &PluginManifest {
        &self.manifest
    }

    async fn initialize(&self, _ctx: &PluginContext) -> PluginResult<()> {
        Ok(())
    }

    async fn shutdown(&self) -> PluginResult<()> {
        Ok(())
    }

    fn providers(&self) -> Vec<Arc<dyn ProviderCapability>> {
        vec![Arc::clone(&self.provider) as Arc<dyn ProviderCapability>]
    }
}

// ---------------------------------------------------------------------
// Governance: scripted decisions + (session, trace) captures
// ---------------------------------------------------------------------

struct SequenceHook {
    decisions: Mutex<Vec<Decision>>,
    served: AtomicUsize,
    captures: Mutex<Vec<(SessionId, apeireth_core::kernel::TraceId)>>,
}

impl SequenceHook {
    fn new(decisions: Vec<Decision>) -> Arc<Self> {
        Arc::new(Self {
            decisions: Mutex::new(decisions),
            served: AtomicUsize::new(0),
            captures: Mutex::new(Vec::new()),
        })
    }

    fn captures(&self) -> Vec<(SessionId, apeireth_core::kernel::TraceId)> {
        self.captures.lock().unwrap().clone()
    }
}

#[async_trait]
impl GovernanceHook for SequenceHook {
    fn name(&self) -> &str {
        "sequence-hook"
    }

    async fn evaluate(&self, request: &GovernanceRequest<'_>) -> Decision {
        self.captures
            .lock()
            .unwrap()
            .push((request.session, request.trace));
        let index = self.served.fetch_add(1, Ordering::SeqCst);
        match self.decisions.lock().unwrap().get(index) {
            Some(decision) => decision.clone(),
            None => Decision::Allow,
        }
    }
}

/// Distinguishes the main completion (a round-1 request carrying the whole
/// transcript, one message in these tests) from isolated organ side-calls
/// (system + input = two messages), and applies a fixed verdict to every
/// side-call while letting the main loop through.
struct SideCallHook {
    verdict: Decision,
    captures: Mutex<Vec<(SessionId, apeireth_core::kernel::TraceId)>>,
}

impl SideCallHook {
    fn new(verdict: Decision) -> Arc<Self> {
        Arc::new(Self {
            verdict,
            captures: Mutex::new(Vec::new()),
        })
    }

    fn captures(&self) -> Vec<(SessionId, apeireth_core::kernel::TraceId)> {
        self.captures.lock().unwrap().clone()
    }
}

#[async_trait]
impl GovernanceHook for SideCallHook {
    fn name(&self) -> &str {
        "side-call-hook"
    }

    async fn evaluate(&self, request: &GovernanceRequest<'_>) -> Decision {
        self.captures
            .lock()
            .unwrap()
            .push((request.session, request.trace));
        match &request.action {
            // Organ side-calls carry exactly system + input (two messages).
            // Main rounds carry the whole transcript (one message on a fresh
            // turn, more later), so they always pass through.
            apeireth_governance::Action::Completion { message_count, .. }
                if *message_count == 2 =>
            {
                self.verdict.clone()
            }
            _ => Decision::Allow,
        }
    }
}

// ---------------------------------------------------------------------
// Backends for the production composition tests (A/B)
// ---------------------------------------------------------------------

#[derive(Default)]
struct FakeMemory;

impl apeireth_plugin::memory_backend::MemoryBackend for FakeMemory {
    fn name(&self) -> &'static str {
        "fake"
    }

    fn kind(&self) -> BackendKind {
        BackendKind::InMemory
    }

    fn put_episode(&self, _episode: &apeireth_core::kernel::Episode) -> CapabilityResult<()> {
        Ok(())
    }

    fn get_episode(&self, _id: &str) -> CapabilityResult<Option<apeireth_core::kernel::Episode>> {
        Ok(None)
    }

    fn recent_episodes(
        &self,
        _session_id: &str,
        _n: usize,
    ) -> CapabilityResult<Vec<apeireth_core::kernel::Episode>> {
        Ok(Vec::new())
    }

    fn append_stream(
        &self,
        _kind: apeireth_core::kernel::StreamKind,
        _entry: apeireth_core::kernel::HistoryEntry,
    ) -> CapabilityResult<()> {
        Ok(())
    }

    fn list_stream(
        &self,
        _kind: apeireth_core::kernel::StreamKind,
        _session_id: &str,
        _n: usize,
    ) -> CapabilityResult<Vec<apeireth_core::kernel::HistoryEntry>> {
        Ok(Vec::new())
    }
}

impl apeireth_memory::MemoryGovernanceStore for FakeMemory {
    fn get_governed(
        &self,
        _episode_id: &str,
    ) -> Result<Option<apeireth_memory::GovernedEpisode>, apeireth_memory::MemoryGovernanceError>
    {
        Ok(None)
    }

    fn update_episode_content(
        &self,
        episode_id: &str,
        _new_content: &str,
        _updated_by: Option<&str>,
        _expected_rev: i64,
    ) -> Result<apeireth_memory::GovernedEpisode, apeireth_memory::MemoryGovernanceError> {
        Err(apeireth_memory::MemoryGovernanceError::NotFound(
            episode_id.to_string(),
        ))
    }

    fn forget_episode(
        &self,
        episode_id: &str,
        _reason: Option<&str>,
        _expected_rev: i64,
    ) -> Result<apeireth_memory::GovernedEpisode, apeireth_memory::MemoryGovernanceError> {
        Err(apeireth_memory::MemoryGovernanceError::NotFound(
            episode_id.to_string(),
        ))
    }

    fn protect_episode(
        &self,
        episode_id: &str,
        _expected_rev: i64,
    ) -> Result<apeireth_memory::GovernedEpisode, apeireth_memory::MemoryGovernanceError> {
        Err(apeireth_memory::MemoryGovernanceError::NotFound(
            episode_id.to_string(),
        ))
    }

    fn unprotect_episode(
        &self,
        episode_id: &str,
        _expected_rev: i64,
    ) -> Result<apeireth_memory::GovernedEpisode, apeireth_memory::MemoryGovernanceError> {
        Err(apeireth_memory::MemoryGovernanceError::NotFound(
            episode_id.to_string(),
        ))
    }

    fn governed_recent_episodes(
        &self,
        _session_id: &str,
        _n: usize,
    ) -> Result<Vec<apeireth_memory::GovernedEpisode>, apeireth_memory::MemoryGovernanceError> {
        Ok(Vec::new())
    }

    fn governed_query(
        &self,
        _q: &apeireth_memory::EpisodeQuery,
    ) -> Result<Vec<apeireth_memory::GovernedEpisode>, apeireth_memory::MemoryGovernanceError> {
        Ok(Vec::new())
    }
}

#[derive(Default)]
struct FakePreferences;

impl apeireth_plugin::preference::PreferenceStore for FakePreferences {
    fn record(&self, _pref: &UserPreference) -> CapabilityResult<()> {
        Ok(())
    }

    fn recall_for_context(
        &self,
        _session_id: &SessionId,
        _topic: &str,
        _limit: u32,
    ) -> CapabilityResult<Vec<UserPreference>> {
        Ok(Vec::new())
    }

    fn forget(&self, _pref_id: &str) -> CapabilityResult<()> {
        Ok(())
    }

    fn list_for_session(&self, _session_id: &SessionId) -> CapabilityResult<Vec<UserPreference>> {
        Ok(Vec::new())
    }
}

#[derive(Default)]
struct FakeAssessments;

impl apeireth_plugin::self_assessment::SelfAssessmentStore for FakeAssessments {
    fn record(&self, _assessment: &SelfAssessment) -> CapabilityResult<()> {
        Ok(())
    }

    fn recent_for_task(
        &self,
        _task_id: &str,
        _limit: u32,
    ) -> CapabilityResult<Vec<SelfAssessment>> {
        Ok(Vec::new())
    }

    fn latest_alignment(&self, _task_id: &str) -> CapabilityResult<Option<f64>> {
        Ok(None)
    }
}

fn fake_backends() -> CognitiveBackends {
    let mem = Arc::new(FakeMemory);
    CognitiveBackends {
        memory: Some(mem.clone()),
        memory_governance: Some(mem),
        preferences: Some(Arc::new(FakePreferences)),
        self_assessments: Some(Arc::new(FakeAssessments)),
        ..CognitiveBackends::default()
    }
}

// ---------------------------------------------------------------------
// Runtime assembly
// ---------------------------------------------------------------------

async fn organ_runtime(
    provider: Arc<ScriptedProvider>,
    governance: Arc<dyn GovernanceHook>,
    max_invocations: usize,
    module: Arc<OrganModule>,
) -> Runtime {
    Runtime::builder()
        .with_default_model("fake-model-1")
        .with_governance(governance)
        .with_max_module_invocations(max_invocations)
        .with_plugin(ProviderPlugin::new(provider))
        .with_module(module)
        .build()
        .await
        .unwrap()
}

// ---------------------------------------------------------------------
// A. Default OFF
// ---------------------------------------------------------------------

#[test]
fn organ_module_is_absent_by_default() {
    assert!(
        !CognitiveModuleConfig::default().organs,
        "organs must default off"
    );
    let clock: Arc<dyn Clock> = Arc::new(SystemClock);
    let modules =
        ProductionCognitiveModules::build(CognitiveModuleConfig::default(), fake_backends(), clock)
            .unwrap();
    assert!(
        !modules.ids().iter().any(|id| id == ORGAN_MODULE_ID),
        "default composition must not register the organ module"
    );
}

// ---------------------------------------------------------------------
// B. Opt-in registers exactly ONE organ module
// ---------------------------------------------------------------------

#[test]
fn opt_in_registers_exactly_one_organ_module() {
    let clock: Arc<dyn Clock> = Arc::new(SystemClock);
    let config = CognitiveModuleConfig {
        organs: true,
        ..CognitiveModuleConfig::default()
    };
    let modules = ProductionCognitiveModules::build(config, fake_backends(), clock).unwrap();
    let organ_slots = modules
        .ids()
        .iter()
        .filter(|id| **id == ORGAN_MODULE_ID)
        .count();
    assert_eq!(
        organ_slots, 1,
        "exactly one organ module, no 9-module explosion"
    );
    assert!(
        modules
            .ids()
            .iter()
            .all(|id| { id == ORGAN_MODULE_ID || !id.to_lowercase().contains("organ") }),
        "no stray organ-flavoured slots"
    );
}

// ---------------------------------------------------------------------
// C. tools are empty
// ---------------------------------------------------------------------

#[test]
fn organ_module_exposes_no_tools() {
    let module = OrganModule::new(Arc::new(SystemClock));
    assert_eq!(module.manifest().id, ORGAN_MODULE_ID);
}

// ---------------------------------------------------------------------
// D. AfterTurn only, and after the main completion
// ---------------------------------------------------------------------

#[tokio::test]
async fn organ_chain_runs_only_at_afterturn() {
    let provider = ScriptedProvider::new();
    let governance = SequenceHook::new(Vec::new());
    let module = Arc::new(OrganModule::new(Arc::new(SystemClock)));
    let runtime = organ_runtime(provider.clone(), governance, 8, module.clone()).await;

    let session = SessionId::new();
    runtime
        .execute_outcome(TurnRequest::new(session, "turn one payload"))
        .await
        .unwrap();

    let observations = module.observations();
    assert_eq!(
        observations.len(),
        1,
        "exactly one organ chain execution per turn"
    );
    assert_eq!(
        observations[0].hook, "AfterTurn",
        "the organ chain ran at AfterTurn, no other hook"
    );

    // The first provider request is the main completion carrying the user
    // text; organ side-calls only appear after it.
    let requests = provider.requests();
    assert!(requests.len() >= 2, "main + at least one organ side-call");
    assert!(
        requests[0].1.contains("turn one payload"),
        "request #0 must be the main completion, got {:?}",
        requests[0]
    );
}

// ---------------------------------------------------------------------
// E. real session id reaches the organ input
// ---------------------------------------------------------------------

#[tokio::test]
async fn organ_observation_carries_the_real_session_id() {
    let provider = ScriptedProvider::new();
    let governance = SequenceHook::new(Vec::new());
    let module = Arc::new(OrganModule::new(Arc::new(SystemClock)));
    let runtime = organ_runtime(provider.clone(), governance, 8, module.clone()).await;

    let session = SessionId::new();
    runtime
        .execute_outcome(TurnRequest::new(session, "hello"))
        .await
        .unwrap();

    let observations = module.observations();
    assert_eq!(observations.len(), 1);
    assert_eq!(
        observations[0].session_id,
        session.to_string(),
        "the organ path used the canonical session id"
    );
    assert_ne!(
        observations[0].session_id, NIL_SESSION,
        "no default/nil session id on the production path"
    );
}

// ---------------------------------------------------------------------
// F. W1/W2 are transient: rebuilt per invocation with the turn's model
// ---------------------------------------------------------------------

#[tokio::test]
async fn transient_w1_w2_are_rebuilt_per_turn() {
    let provider = ScriptedProvider::new();
    let governance = SequenceHook::new(Vec::new());
    let module = Arc::new(OrganModule::new(Arc::new(SystemClock)));
    let runtime = organ_runtime(provider.clone(), governance, 8, module.clone()).await;

    let session = SessionId::new();
    runtime
        .execute_outcome(TurnRequest::new(session, "turn one").with_model("model-a"))
        .await
        .unwrap();
    runtime
        .execute_outcome(TurnRequest::new(session, "turn two").with_model("model-b"))
        .await
        .unwrap();

    let requests = provider.requests();
    // Per turn, the FIRST request with the turn's model is the main
    // completion; any later request with that model is an organ side-call
    // built from the transient W1/W2 carrying the turn's model.
    let positions = |model: &str| {
        requests
            .iter()
            .enumerate()
            .filter(|(_, (m, _))| m == model)
            .map(|(i, _)| i)
            .collect::<Vec<_>>()
    };
    let model_a = positions("model-a");
    let model_b = positions("model-b");
    assert!(
        model_a.len() >= 2,
        "turn one ran the main completion plus an organ side-call with model-a"
    );
    assert!(
        model_b.len() >= 2,
        "turn two rebuilt transient organs with turn two's model; a cached          turn-one W1/W2 would still have shown model-a"
    );
    assert_eq!(module.observations().len(), 2);
}

// ---------------------------------------------------------------------
// H. side-call budget does not leak between turns
// ---------------------------------------------------------------------

#[tokio::test]
async fn organ_side_call_budget_is_fresh_per_turn() {
    let provider = ScriptedProvider::new();
    let governance = SequenceHook::new(Vec::new());
    // Budget of 2: turn one's organ side-calls exhaust it; turn two must still
    // run its own organ side-calls on a fresh budget.
    let module = Arc::new(OrganModule::new(Arc::new(SystemClock)));
    let runtime = organ_runtime(provider.clone(), governance, 2, module.clone()).await;

    let session = SessionId::new();
    runtime
        .execute_outcome(TurnRequest::new(session, "turn one").with_model("model-a"))
        .await
        .unwrap();
    runtime
        .execute_outcome(TurnRequest::new(session, "turn two").with_model("model-b"))
        .await
        .unwrap();

    // With a budget of 2, turn one's organ side-calls exhaust the turn. If the
    // budget leaked across turns, turn two's W1/W2 side-calls would be refused
    // and no model-b organ request would exist.
    let requests = provider.requests();
    let model_b = requests
        .iter()
        .enumerate()
        .filter(|(_, (model, _))| model == "model-b")
        .map(|(i, _)| i)
        .collect::<Vec<_>>();
    assert!(
        model_b.len() >= 2,
        "turn two must have fresh side-call budget for organ side-calls;          the first model-b request is the main completion, later ones are          organ side-calls"
    );
    assert_eq!(module.observations().len(), 2);
}

// ---------------------------------------------------------------------
// I. governance deny: zero provider side-calls, fail open
// ---------------------------------------------------------------------

#[tokio::test]
async fn denied_organ_side_calls_fail_open_with_zero_provider_calls() {
    let provider = ScriptedProvider::new();
    // Evaluation order: main completion (allowed) first, then the AfterTurn
    // organ side-calls (denied).
    let governance = SideCallHook::new(Decision::deny("organ side-calls disabled"));
    let module = Arc::new(OrganModule::new(Arc::new(SystemClock)));
    let runtime = organ_runtime(provider.clone(), governance, 8, module.clone()).await;

    let outcome = runtime
        .execute_outcome(TurnRequest::new(SessionId::new(), "denied organ turn"))
        .await
        .unwrap();
    assert!(
        matches!(outcome, TurnOutcome::Completed(_)),
        "organ enhancement failure must not fail the committed turn"
    );
    assert_eq!(
        provider.call_count(),
        1,
        "only the main completion may reach the provider under Deny"
    );
    assert_eq!(
        module.observations().len(),
        1,
        "chain still executed (fail-open)"
    );
}

// ---------------------------------------------------------------------
// J. RequireApproval: no hidden approval, session stays usable
// ---------------------------------------------------------------------

#[tokio::test]
async fn require_approval_organ_side_calls_create_no_hidden_approval() {
    let provider = ScriptedProvider::new();
    let governance = SideCallHook::new(Decision::require_approval("escalation needed"));
    let module = Arc::new(OrganModule::new(Arc::new(SystemClock)));
    let runtime = organ_runtime(provider.clone(), governance, 8, module.clone()).await;

    let session = SessionId::new();
    let outcome = runtime
        .execute_outcome(TurnRequest::new(session, "approval turn"))
        .await
        .unwrap();
    assert!(
        matches!(outcome, TurnOutcome::Completed(_)),
        "no approval may be minted by organ side-calls"
    );
    assert_eq!(
        provider.call_count(),
        1,
        "organ side-calls must not reach the provider under RequireApproval"
    );
    // The session remains usable for the next turn.
    let outcome = runtime
        .execute_outcome(TurnRequest::new(session, "next turn"))
        .await
        .unwrap();
    assert!(matches!(outcome, TurnOutcome::Completed(_)));
    assert_eq!(module.observations().len(), 2);
}

// ---------------------------------------------------------------------
// K. the primary transcript is untouched by organ cognition
// ---------------------------------------------------------------------

#[tokio::test]
async fn organ_cognition_does_not_mutate_the_primary_transcript() {
    let provider = ScriptedProvider::new();
    let governance = SequenceHook::new(Vec::new());
    let module = Arc::new(OrganModule::new(Arc::new(SystemClock)));
    let runtime = organ_runtime(provider.clone(), governance, 8, module.clone()).await;

    let session = SessionId::new();
    runtime
        .execute_outcome(TurnRequest::new(session, "clean turn"))
        .await
        .unwrap();

    let stored = runtime.sessions().load(&session).await.unwrap().unwrap();
    assert_eq!(
        stored.len(),
        2,
        "only the canonical user and assistant messages persist"
    );
    for message in &stored.messages {
        match message.role {
            apeireth_protocol::canonical::MessageRole::User
            | apeireth_protocol::canonical::MessageRole::Assistant => {}
            other => panic!("unexpected persisted role {other:?}"),
        }
    }
    let joined: String = stored
        .messages
        .iter()
        .map(|m| ContentPart::join_text(&m.content))
        .collect::<Vec<_>>()
        .join("|");
    assert!(
        joined.contains("clean turn") && joined.contains("echo:"),
        "transcript holds the canonical exchange only, got {joined:?}"
    );
}

// ---------------------------------------------------------------------
// L. concurrent sessions stay isolated
// ---------------------------------------------------------------------

#[tokio::test]
async fn concurrent_sessions_isolate_organ_cognition() {
    let provider = ScriptedProvider::new();
    let governance = SequenceHook::new(Vec::new());
    let module = Arc::new(OrganModule::new(Arc::new(SystemClock)));
    let runtime = organ_runtime(provider.clone(), governance.clone(), 8, module.clone()).await;

    let session_a = SessionId::new();
    let session_b = SessionId::new();
    let (ra, rb) = tokio::join!(
        runtime.execute_outcome(TurnRequest::new(session_a, "hello a")),
        runtime.execute_outcome(TurnRequest::new(session_b, "hello b")),
    );
    assert!(matches!(ra.unwrap(), TurnOutcome::Completed(_)));
    assert!(matches!(rb.unwrap(), TurnOutcome::Completed(_)));

    let observations = module.observations();
    assert_eq!(observations.len(), 2, "one organ chain per session turn");
    let observed_sessions: std::collections::BTreeSet<String> =
        observations.iter().map(|o| o.session_id.clone()).collect();
    assert_eq!(
        observed_sessions,
        std::collections::BTreeSet::from([session_a.to_string(), session_b.to_string()]),
        "each observation carries its own session, no contamination"
    );
    let captured_sessions: std::collections::BTreeSet<String> = governance
        .captures()
        .into_iter()
        .map(|(sid, _)| sid.to_string())
        .collect();
    assert_eq!(
        captured_sessions,
        std::collections::BTreeSet::from([session_a.to_string(), session_b.to_string()]),
        "side-calls only fired under their own sessions"
    );
    assert!(
        governance.captures().len() >= 4,
        "main + organ evaluations for both turns"
    );
}

// ---------------------------------------------------------------------
// Architecture guard: the OrganModule struct owns no turn authority
// ---------------------------------------------------------------------

/// The struct definition must not contain runtime, provider, governance,
/// invoker, or factory fields. Behavioral tests above prove the transient
/// construction path; this guard pins the persistent shape.
#[test]
fn organ_module_struct_owns_no_turn_authority() {
    let source = include_str!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/src/canonical/organ_module.rs"
    ));
    let start = source
        .find("pub struct OrganModule {")
        .expect("OrganModule struct definition");
    let body = &source[start..];
    let end = body.find("\n}").expect("struct body end");
    let struct_body = &body[..end];

    for forbidden in [
        "ModuleInvoker",
        "InvokerLlmFactory",
        "ProviderRouter",
        "GovernanceHook",
        "SessionStore",
        "Runtime",
        "reqwest",
        "thread_local",
        "tokio::spawn",
    ] {
        assert!(
            !struct_body.contains(forbidden),
            "OrganModule struct must not own {forbidden:?}"
        );
    }
}
