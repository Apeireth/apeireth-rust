//! End-to-end coverage for the first real preference-learning closed loop.
//!
//! Acceptance (test O): turn 1 learns explicit preferences through
//! `cognitive.preference_learning` into the existing `PreferenceStore`, and
//! turn 2's canonical provider request carries them through the existing
//! `cognitive.preference_recall` overlay — proven on the provider-visible
//! request, not by reading the store directly.

use apeireth_runtime_assembly as apeireth_runtime;

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use apeireth_core::clock::SystemClock;
use apeireth_core::kernel::{CapabilityId, Clock, ModelId, PluginId, SessionId};
use apeireth_governance::{Decision, GovernanceHook, GovernanceRequest};
use apeireth_plugin::memory_backend::{BackendKind, CapabilityResult};
use apeireth_plugin::preference::{PreferenceStore, UserPreference};
use apeireth_plugin::{
    CapabilityKind, Plugin, PluginContext, PluginManifest, PluginResult, ProviderCapability,
    ProviderError,
};
use apeireth_protocol::canonical::{
    ContentPart, ModelDescriptor, NormalizedFinishReason, NormalizedMessage, NormalizedRequest,
    NormalizedResponse, NormalizedUsage,
};
use apeireth_runtime::canonical::{
    CognitiveBackends, CognitiveModuleConfig, PreferenceLearningModule, ProductionCognitiveModules,
    Runtime, TurnOutcome, TurnRequest, PREFERENCE_LEARNING_MODULE_ID,
};
use async_trait::async_trait;

const MODEL: &str = "fake-model-1";

// ---------------------------------------------------------------------
// In-memory preference store mirroring the fixed recall semantics
// ---------------------------------------------------------------------

#[derive(Default)]
struct FakePrefStore {
    rows: Mutex<Vec<UserPreference>>,
}

impl FakePrefStore {
    fn rows(&self) -> Vec<UserPreference> {
        self.rows.lock().unwrap().clone()
    }
}

impl PreferenceStore for FakePrefStore {
    fn record(&self, pref: &UserPreference) -> CapabilityResult<()> {
        let mut rows = self.rows.lock().unwrap();
        rows.retain(|row| row.id != pref.id);
        rows.push(pref.clone());
        Ok(())
    }

    fn recall_for_context(
        &self,
        session_id: &SessionId,
        current_topic: &str,
        limit: u32,
    ) -> CapabilityResult<Vec<UserPreference>> {
        let rows = self.rows.lock().unwrap();
        let mut mine: Vec<UserPreference> = rows
            .iter()
            .filter(|row| row.session_id == *session_id)
            .cloned()
            .collect();
        if !current_topic.is_empty() {
            mine.retain(|row| {
                row.topic.contains(current_topic) || current_topic.contains(&row.topic)
            });
        }
        if mine.is_empty() {
            // Fixed donor semantics: fall back to the session's top-N.
            mine = rows
                .iter()
                .filter(|row| row.session_id == *session_id)
                .cloned()
                .collect();
        }
        mine.sort_by(|a, b| {
            b.confidence
                .partial_cmp(&a.confidence)
                .unwrap_or(std::cmp::Ordering::Equal)
                .then(b.created_at.cmp(&a.created_at))
        });
        mine.truncate(limit as usize);
        Ok(mine)
    }

    fn forget(&self, pref_id: &str) -> CapabilityResult<()> {
        self.rows.lock().unwrap().retain(|row| row.id != pref_id);
        Ok(())
    }

    fn list_for_session(&self, session_id: &SessionId) -> CapabilityResult<Vec<UserPreference>> {
        Ok(self
            .rows
            .lock()
            .unwrap()
            .iter()
            .filter(|row| row.session_id == *session_id)
            .cloned()
            .collect())
    }
}

// ---------------------------------------------------------------------
// Runtime doubles
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
struct FakeAssessments;

impl apeireth_plugin::self_assessment::SelfAssessmentStore for FakeAssessments {
    fn record(
        &self,
        _assessment: &apeireth_plugin::self_assessment::SelfAssessment,
    ) -> CapabilityResult<()> {
        Ok(())
    }

    fn recent_for_task(
        &self,
        _task_id: &str,
        _limit: u32,
    ) -> CapabilityResult<Vec<apeireth_plugin::self_assessment::SelfAssessment>> {
        Ok(Vec::new())
    }

    fn latest_alignment(&self, _task_id: &str) -> CapabilityResult<Option<f64>> {
        Ok(None)
    }
}

struct ScriptedProvider {
    id: CapabilityId,
    calls: AtomicUsize,
    requests: Mutex<Vec<String>>,
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

    fn requests(&self) -> Vec<String> {
        self.requests.lock().unwrap().clone()
    }
}

#[async_trait]
impl ProviderCapability for ScriptedProvider {
    fn id(&self) -> &CapabilityId {
        &self.id
    }

    fn models(&self) -> Vec<ModelDescriptor> {
        vec![ModelDescriptor::new(
            ModelId::new(MODEL).unwrap(),
            self.id.clone(),
        )]
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
        self.requests.lock().unwrap().push(text);
        Ok(NormalizedResponse {
            id: format!("response-{}", self.call_count()),
            model: request.model.clone(),
            content: "a small new project deserves a small toolchain".to_string(),
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

/// Applies a fixed verdict to side-call-shaped completions (system + input =
/// two messages) while main rounds always pass. With the deterministic-only
/// learner there are no side-calls at all; this hook proves that.
struct SideCallHook {
    verdict: Decision,
}

#[async_trait]
impl GovernanceHook for SideCallHook {
    fn name(&self) -> &str {
        "side-call-hook"
    }

    async fn evaluate(&self, request: &GovernanceRequest<'_>) -> Decision {
        match &request.action {
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
// Assembly
// ---------------------------------------------------------------------

fn learning_config() -> CognitiveModuleConfig {
    CognitiveModuleConfig {
        preference_learning: true,
        ..CognitiveModuleConfig::default()
    }
}

async fn learning_runtime(
    provider: Arc<ScriptedProvider>,
    store: Arc<FakePrefStore>,
    governance: Arc<dyn GovernanceHook>,
) -> Runtime {
    let clock: Arc<dyn Clock> = Arc::new(SystemClock);
    let mem = Arc::new(FakeMemory);
    let backends = CognitiveBackends {
        memory: Some(mem.clone()),
        memory_governance: Some(mem),
        preferences: Some(store),
        self_assessments: Some(Arc::new(FakeAssessments::default())),
        ..CognitiveBackends::default()
    };
    let modules = ProductionCognitiveModules::build(learning_config(), backends, clock).unwrap();
    modules
        .register_into(
            Runtime::builder()
                .with_default_model(MODEL)
                .with_governance(governance)
                .with_plugin(ProviderPlugin::new(provider)),
        )
        .build()
        .await
        .unwrap()
}

// ---------------------------------------------------------------------
// A. default OFF
// ---------------------------------------------------------------------

#[test]
fn preference_learning_is_absent_by_default() {
    assert!(
        !CognitiveModuleConfig::default().preference_learning,
        "preference learning must default off"
    );
    let clock: Arc<dyn Clock> = Arc::new(SystemClock);
    let mem = Arc::new(FakeMemory);
    let backends = CognitiveBackends {
        memory: Some(mem.clone()),
        memory_governance: Some(mem),
        preferences: Some(Arc::new(FakePrefStore::default())),
        self_assessments: Some(Arc::new(FakeAssessments::default())),
        ..CognitiveBackends::default()
    };
    let modules =
        ProductionCognitiveModules::build(CognitiveModuleConfig::default(), backends, clock)
            .unwrap();
    assert!(
        !modules
            .ids()
            .iter()
            .any(|id| id == PREFERENCE_LEARNING_MODULE_ID),
        "default composition must not register the learning slot"
    );
}

// ---------------------------------------------------------------------
// B. opt-in registration: exactly one slot
// ---------------------------------------------------------------------

#[test]
fn opt_in_registers_exactly_one_learning_slot() {
    let clock: Arc<dyn Clock> = Arc::new(SystemClock);
    let mem = Arc::new(FakeMemory);
    let backends = CognitiveBackends {
        memory: Some(mem.clone()),
        memory_governance: Some(mem),
        preferences: Some(Arc::new(FakePrefStore::default())),
        self_assessments: Some(Arc::new(FakeAssessments::default())),
        ..CognitiveBackends::default()
    };
    let modules = ProductionCognitiveModules::build(learning_config(), backends, clock).unwrap();
    assert_eq!(
        modules
            .ids()
            .iter()
            .filter(|id| **id == PREFERENCE_LEARNING_MODULE_ID)
            .count(),
        1
    );
}

// ---------------------------------------------------------------------
// C. missing backend is a boot-time error
// ---------------------------------------------------------------------

#[test]
fn enabled_without_preference_store_fails_at_boot() {
    let clock: Arc<dyn Clock> = Arc::new(SystemClock);
    let config = CognitiveModuleConfig {
        preference_learning: true,
        preference_recall: false,
        ..CognitiveModuleConfig::default()
    };
    let mem = Arc::new(FakeMemory);
    let backends = CognitiveBackends {
        memory: Some(mem.clone()),
        memory_governance: Some(mem),
        preferences: None,
        ..CognitiveBackends::default()
    };
    let Err(error) = ProductionCognitiveModules::build(config, backends, clock) else {
        panic!("missing preference backend must fail at boot");
    };
    assert!(
        error.to_string().contains("preference_learning"),
        "the error must name the slot requiring the backend, got {error}"
    );
}

// ---------------------------------------------------------------------
// E/F/G: deterministic extraction, zero provider side-calls
// ---------------------------------------------------------------------

#[tokio::test]
async fn explicit_positive_preference_is_learned_without_llm() {
    let provider = ScriptedProvider::new();
    let store = Arc::new(FakePrefStore::default());
    let runtime = learning_runtime(
        provider.clone(),
        store.clone(),
        Arc::new(apeireth_governance::AllowAll),
    )
    .await;
    runtime
        .execute_outcome(TurnRequest::new(SessionId::new(), "I like Rust."))
        .await
        .unwrap();

    let rows = store.rows();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].topic, "rust");
    assert!(rows[0].stance.contains("likes rust"));
    assert!(rows[0].tags.iter().any(|tag| tag == "explicit"));
    // Deterministic learning: the main completion is the ONLY provider call.
    assert_eq!(provider.call_count(), 1);
}

#[tokio::test]
async fn explicit_negative_preference_is_learned() {
    let provider = ScriptedProvider::new();
    let store = Arc::new(FakePrefStore::default());
    let runtime = learning_runtime(
        provider.clone(),
        store.clone(),
        Arc::new(apeireth_governance::AllowAll),
    )
    .await;
    runtime
        .execute_outcome(TurnRequest::new(SessionId::new(), "I don't like Python."))
        .await
        .unwrap();

    let rows = store.rows();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].topic, "python");
    assert!(rows[0].stance.starts_with("dislikes"));
    assert!(rows[0].tags.iter().any(|tag| tag == "negative"));
}

#[tokio::test]
async fn preference_comparison_is_coherent() {
    let provider = ScriptedProvider::new();
    let store = Arc::new(FakePrefStore::default());
    let runtime = learning_runtime(
        provider.clone(),
        store.clone(),
        Arc::new(apeireth_governance::AllowAll),
    )
    .await;
    runtime
        .execute_outcome(TurnRequest::new(
            SessionId::new(),
            "I prefer Rust to Python.",
        ))
        .await
        .unwrap();

    let rows = store.rows();
    assert_eq!(rows.len(), 1, "one coherent row for the comparison");
    assert_eq!(rows[0].topic, "rust");
    assert!(rows[0].stance.contains("prefers rust over python"));
    assert!(rows[0].tags.iter().any(|tag| tag == "comparison"));
}

// ---------------------------------------------------------------------
// H. transient statements are not persisted
// ---------------------------------------------------------------------

#[tokio::test]
async fn transient_statements_are_not_persisted() {
    let provider = ScriptedProvider::new();
    let store = Arc::new(FakePrefStore::default());
    let runtime = learning_runtime(
        provider.clone(),
        store.clone(),
        Arc::new(apeireth_governance::AllowAll),
    )
    .await;
    for text in [
        "I want noodles tonight.",
        "I need to study chemistry today.",
        "I'm tired right now.",
    ] {
        runtime
            .execute_outcome(TurnRequest::new(SessionId::new(), text))
            .await
            .unwrap();
    }
    assert!(
        store.rows().is_empty(),
        "temporary wishes must not become stable preferences"
    );
}

// ---------------------------------------------------------------------
// I/J: reinforcement and contradiction
// ---------------------------------------------------------------------

#[tokio::test]
async fn repeated_evidence_reinforces_without_duplicates() {
    let provider = ScriptedProvider::new();
    let store = Arc::new(FakePrefStore::default());
    let runtime = learning_runtime(
        provider.clone(),
        store.clone(),
        Arc::new(apeireth_governance::AllowAll),
    )
    .await;
    let session = SessionId::new();
    runtime
        .execute_outcome(TurnRequest::new(session, "I like Rust."))
        .await
        .unwrap();
    runtime
        .execute_outcome(TurnRequest::new(session, "I really like Rust."))
        .await
        .unwrap();

    let rows = store.rows();
    assert_eq!(rows.len(), 1, "same topic reinforces, never explodes");
    assert!(
        (rows[0].confidence - 0.8).abs() < 1e-9,
        "confidence takes the max across observations, got {}",
        rows[0].confidence
    );
}

#[tokio::test]
async fn contradiction_flips_stance_deterministically() {
    let provider = ScriptedProvider::new();
    let store = Arc::new(FakePrefStore::default());
    let runtime = learning_runtime(
        provider.clone(),
        store.clone(),
        Arc::new(apeireth_governance::AllowAll),
    )
    .await;
    let session = SessionId::new();
    runtime
        .execute_outcome(TurnRequest::new(session, "I like Rust."))
        .await
        .unwrap();
    runtime
        .execute_outcome(TurnRequest::new(session, "I don't like Rust."))
        .await
        .unwrap();

    let rows = store.rows();
    assert_eq!(rows.len(), 1, "contradiction replaces, not appends");
    assert!(
        rows[0].stance.starts_with("dislikes"),
        "latest evidence wins: got {:?}",
        rows[0].stance
    );
    assert!(rows[0].tags.iter().any(|tag| tag == "negative"));
    assert!(
        (rows[0].confidence - 0.7).abs() < 1e-9,
        "confidence still reflects the evidence scale"
    );
}

// ---------------------------------------------------------------------
// K/L: governance posture (deterministic-only: zero side-calls)
// ---------------------------------------------------------------------

#[tokio::test]
async fn deny_shaped_governance_leaves_learning_intact() {
    let provider = ScriptedProvider::new();
    let store = Arc::new(FakePrefStore::default());
    let runtime = learning_runtime(
        provider.clone(),
        store.clone(),
        Arc::new(SideCallHook {
            verdict: Decision::deny("no side-calls"),
        }),
    )
    .await;
    let outcome = runtime
        .execute_outcome(TurnRequest::new(SessionId::new(), "I like Rust."))
        .await
        .unwrap();
    assert!(matches!(outcome, TurnOutcome::Completed(_)));
    // Deterministic learning never calls the provider beyond the main round.
    assert_eq!(provider.call_count(), 1);
    assert_eq!(store.rows().len(), 1, "explicit evidence is still learned");
}

#[tokio::test]
async fn require_approval_shaped_governance_creates_no_hidden_approval() {
    let provider = ScriptedProvider::new();
    let store = Arc::new(FakePrefStore::default());
    let runtime = learning_runtime(
        provider.clone(),
        store.clone(),
        Arc::new(SideCallHook {
            verdict: Decision::require_approval("escalation needed"),
        }),
    )
    .await;
    let session = SessionId::new();
    let outcome = runtime
        .execute_outcome(TurnRequest::new(session, "I like Rust."))
        .await
        .unwrap();
    assert!(matches!(outcome, TurnOutcome::Completed(_)));
    assert_eq!(provider.call_count(), 1);
    let outcome = runtime
        .execute_outcome(TurnRequest::new(session, "next turn"))
        .await
        .unwrap();
    assert!(matches!(outcome, TurnOutcome::Completed(_)));
    assert_eq!(store.rows().len(), 1);
}

// ---------------------------------------------------------------------
// M/N: transcript cleanliness and session isolation
// ---------------------------------------------------------------------

#[tokio::test]
async fn learning_does_not_pollute_the_primary_transcript() {
    let provider = ScriptedProvider::new();
    let store = Arc::new(FakePrefStore::default());
    let runtime = learning_runtime(
        provider.clone(),
        store.clone(),
        Arc::new(apeireth_governance::AllowAll),
    )
    .await;
    let session = SessionId::new();
    runtime
        .execute_outcome(TurnRequest::new(session, "I like Rust."))
        .await
        .unwrap();

    let stored = runtime.sessions().load(&session).await.unwrap().unwrap();
    assert_eq!(
        stored.len(),
        2,
        "only the canonical user/assistant exchange"
    );
    let joined: String = stored
        .messages
        .iter()
        .map(|m| ContentPart::join_text(&m.content))
        .collect::<Vec<_>>()
        .join("|");
    assert!(
        !joined.contains("preference context") && !joined.contains("likes rust"),
        "no learning artifacts in the primary transcript, got {joined:?}"
    );
}

#[tokio::test]
async fn preferences_do_not_leak_across_sessions() {
    let provider = ScriptedProvider::new();
    let store = Arc::new(FakePrefStore::default());
    let runtime = learning_runtime(
        provider.clone(),
        store.clone(),
        Arc::new(apeireth_governance::AllowAll),
    )
    .await;
    let session_a = SessionId::new();
    runtime
        .execute_outcome(TurnRequest::new(session_a, "I like Rust."))
        .await
        .unwrap();
    assert_eq!(store.rows().len(), 1);

    // Session B: no preference evidence, and its recall must not surface
    // session A's rows.
    runtime
        .execute_outcome(TurnRequest::new(SessionId::new(), "What should I use?"))
        .await
        .unwrap();
    let requests = provider.requests();
    let second = requests
        .iter()
        .find(|text| text.contains("What should I use?"))
        .expect("session B main request");
    assert!(
        !second.contains("likes rust"),
        "session B must not see session A's preferences, got {second:?}"
    );
    assert!(
        store.rows().iter().all(|row| row.session_id == session_a),
        "session B learned nothing and inherited nothing"
    );
}

// ---------------------------------------------------------------------
// O. THE CLOSED LOOP: turn 1 learns → turn 2 recall overlays the request
// ---------------------------------------------------------------------

#[tokio::test]
async fn turn1_learning_reaches_turn2_provider_context() {
    let provider = ScriptedProvider::new();
    let store = Arc::new(FakePrefStore::default());
    let runtime = learning_runtime(
        provider.clone(),
        store.clone(),
        Arc::new(apeireth_governance::AllowAll),
    )
    .await;

    // TURN 1: explicit preferences, committed normally, learned at AfterTurn.
    let session = SessionId::new();
    runtime
        .execute_outcome(TurnRequest::new(
            session,
            "I prefer Rust and I don't really like Python.",
        ))
        .await
        .unwrap();
    let topics: Vec<String> = store.rows().iter().map(|row| row.topic.clone()).collect();
    assert!(
        topics.contains(&"rust".to_string()) && topics.contains(&"python".to_string()),
        "turn 1 learned both explicit preferences, got {topics:?}"
    );

    // TURN 2: an unrelated question — the existing PreferenceRecall overlay
    // must carry the learned preferences into the provider-visible request.
    runtime
        .execute_outcome(TurnRequest::new(
            session,
            "What should I use for a small new project?",
        ))
        .await
        .unwrap();
    let requests = provider.requests();
    let turn2_main = requests
        .iter()
        .find(|text| text.contains("What should I use for a small new project?"))
        .expect("turn 2 main request");
    assert!(
        turn2_main.contains("Retrieved user preference context"),
        "the recall overlay is present on the turn 2 request"
    );
    assert!(
        turn2_main.contains("prefers rust"),
        "learned Rust preference is provider-visible, got {turn2_main:?}"
    );
    assert!(
        turn2_main.contains("dislikes python"),
        "learned Python aversion is provider-visible, got {turn2_main:?}"
    );
}
