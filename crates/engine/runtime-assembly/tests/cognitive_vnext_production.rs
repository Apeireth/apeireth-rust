//! Production composition proofs for the Cognitive Infrastructure vNext.
//!
//! These tests drive the real runtime loop. They do not call the Guard
//! observer by hand: a provider emits a tool call, the runtime evaluates and
//! dispatches it, and the event fan-out closes the dataset row.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use apeireth_core::kernel::{CapabilityId, ModelId, PluginId, SessionId};
use apeireth_governance::GovernancePipeline;
use apeireth_guard::{BehaviorChainGuardHook, DatasetRecorder};
use apeireth_memory::{MemoryGovernanceStore, MemoryWritebackEntry, SqliteMemoryStore};
use apeireth_plugin::{
    CapabilityKind, Plugin, PluginContext, PluginManifest, PluginResult, ProviderCapability,
    ProviderError, ToolCapability,
};
use apeireth_protocol::canonical::{
    ModelDescriptor, ModelFeature, NormalizedRequest, NormalizedResponse, NormalizedTool,
    NormalizedUsage, ToolCall, ToolResult,
};
use apeireth_runtime::canonical::{CompositeRuntimeEventSink, Runtime, TurnRequest};
use apeireth_runtime_assembly::{
    CognitiveBackends, CognitiveModuleConfig, GuardDatasetObserver, ProductionCognitiveModules,
};
use async_trait::async_trait;
use tempfile::tempdir;

const MODEL: &str = "cognitive-vnext-test-model";

struct ScriptedProvider {
    id: CapabilityId,
    calls: AtomicUsize,
    seen: Mutex<Vec<NormalizedRequest>>,
}

impl ScriptedProvider {
    fn new() -> Arc<Self> {
        Arc::new(Self {
            id: CapabilityId::new("provider.cognitive-test").unwrap(),
            calls: AtomicUsize::new(0),
            seen: Mutex::new(Vec::new()),
        })
    }

    fn requests(&self) -> Vec<NormalizedRequest> {
        self.seen.lock().unwrap().clone()
    }
}

#[async_trait]
impl ProviderCapability for ScriptedProvider {
    fn id(&self) -> &CapabilityId {
        &self.id
    }

    fn models(&self) -> Vec<ModelDescriptor> {
        vec![
            ModelDescriptor::new(ModelId::new(MODEL).unwrap(), self.id.clone())
                .with_feature(ModelFeature::ToolCalls),
        ]
    }

    async fn complete(
        &self,
        request: &NormalizedRequest,
    ) -> Result<NormalizedResponse, ProviderError> {
        let index = self.calls.fetch_add(1, Ordering::SeqCst);
        self.seen.lock().unwrap().push(request.clone());
        let base = NormalizedResponse {
            id: format!("response-{index}"),
            model: request.model.clone(),
            content: String::new(),
            finish_reason: Some(apeireth_protocol::canonical::NormalizedFinishReason::Stop),
            usage: NormalizedUsage::default(),
            tool_calls: Vec::new(),
            raw_metadata: serde_json::Map::new(),
        };
        if index == 0 {
            Ok(NormalizedResponse {
                finish_reason: Some(
                    apeireth_protocol::canonical::NormalizedFinishReason::ToolCalls,
                ),
                tool_calls: vec![ToolCall {
                    id: "call-cognitive-1".into(),
                    name: "echo".into(),
                    arguments: serde_json::json!({"value": "ok"}),
                }],
                ..base
            })
        } else {
            Ok(NormalizedResponse {
                content: "done".into(),
                ..base
            })
        }
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
                PluginId::new("plugin.cognitive-test").unwrap(),
                "1.0.0",
                "Cognitive vNext test provider",
            )
            .declare_capability(
                provider.id().clone(),
                CapabilityKind::Provider,
                "Scripted provider",
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

struct EchoTool {
    id: CapabilityId,
}

#[async_trait]
impl ToolCapability for EchoTool {
    fn id(&self) -> &CapabilityId {
        &self.id
    }

    fn declaration(&self) -> NormalizedTool {
        NormalizedTool::new("echo")
    }

    async fn invoke(&self, call: &ToolCall) -> ToolResult {
        ToolResult::ok(call.id.clone(), serde_json::json!({"echoed": true})).with_name("echo")
    }
}

fn memory_config() -> CognitiveModuleConfig {
    CognitiveModuleConfig {
        memory_recall: true,
        memory_writeback: false,
        preference_recall: false,
        self_assessment: false,
        filesystem: false,
        search: false,
        repo: false,
        ..CognitiveModuleConfig::default()
    }
}

#[tokio::test]
async fn production_memory_module_recalls_after_restart_and_honors_forget() {
    let dir = tempdir().unwrap();
    let db_path = dir.path().join("cognitive.sqlite3");
    let session = SessionId::new();

    let store = Arc::new(SqliteMemoryStore::open(&db_path).unwrap());
    let backend = store.clone() as Arc<dyn apeireth_plugin::memory_backend::MemoryBackend>;
    let governance = store.clone() as Arc<dyn MemoryGovernanceStore>;
    let coordinator = apeireth_memory::MemoryCoordinator::new(backend.clone(), governance.clone());
    let episode_id = coordinator
        .writeback(&MemoryWritebackEntry::new(
            session.to_string(),
            "user",
            "Apeireth uses a canonical runtime kernel",
        ))
        .unwrap();

    let provider = ScriptedProvider::new();
    let modules = ProductionCognitiveModules::build(
        memory_config(),
        CognitiveBackends {
            memory: Some(backend.clone()),
            memory_governance: Some(governance.clone()),
            ..CognitiveBackends::default()
        },
        apeireth_core::kernel::system_clock(),
    )
    .unwrap();
    let runtime = modules
        .register_into(
            Runtime::builder()
                .with_plugin(ProviderPlugin::new(provider.clone()))
                .with_default_model(MODEL),
        )
        .build()
        .await
        .unwrap();

    let first = runtime
        .execute(TurnRequest::new(session, "What is Apeireth?"))
        .await
        .unwrap();
    let first_request = provider.requests().into_iter().next().unwrap();
    let first_text = first_request
        .messages
        .iter()
        .flat_map(|message| message.content.iter())
        .map(|part| match part {
            apeireth_protocol::canonical::ContentPart::Text { text } => text.as_str(),
            _ => "",
        })
        .collect::<String>();
    assert!(first_text.contains("<governed_memory"));
    assert!(first_text.contains("canonical runtime kernel"));
    assert!(!first_text.contains("Retrieved memory context"));
    assert_eq!(first.text, "done");

    governance
        .forget_episode(&episode_id, Some("vnext test"), 0)
        .unwrap();
    drop(runtime);

    let reopened = Arc::new(SqliteMemoryStore::open(&db_path).unwrap());
    let reopened_backend =
        reopened.clone() as Arc<dyn apeireth_plugin::memory_backend::MemoryBackend>;
    let reopened_governance = reopened.clone() as Arc<dyn MemoryGovernanceStore>;
    let reopened_modules = ProductionCognitiveModules::build(
        memory_config(),
        CognitiveBackends {
            memory: Some(reopened_backend),
            memory_governance: Some(reopened_governance),
            ..CognitiveBackends::default()
        },
        apeireth_core::kernel::system_clock(),
    )
    .unwrap();
    let reopened_provider = ScriptedProvider::new();
    let reopened_runtime = reopened_modules
        .register_into(
            Runtime::builder()
                .with_plugin(ProviderPlugin::new(reopened_provider.clone()))
                .with_default_model(MODEL),
        )
        .build()
        .await
        .unwrap();
    reopened_runtime
        .execute(TurnRequest::new(session, "What is Apeireth?"))
        .await
        .unwrap();
    let reopened_text = reopened_provider.requests()[0]
        .messages
        .iter()
        .flat_map(|message| message.content.iter())
        .map(|part| match part {
            apeireth_protocol::canonical::ContentPart::Text { text } => text.as_str(),
            _ => "",
        })
        .collect::<String>();
    assert!(!reopened_text.contains("canonical runtime kernel"));
}

#[tokio::test]
async fn production_runtime_events_close_guard_dataset_by_action_id() {
    let dir = tempdir().unwrap();
    let recorder = Arc::new(DatasetRecorder::new(dir.path().join("guard.jsonl")));
    recorder.set_enabled(true);
    let guard = Arc::new(BehaviorChainGuardHook::new().with_dataset_recorder(recorder.clone()));
    let provider = ScriptedProvider::new();
    let sink = Arc::new(CompositeRuntimeEventSink::new(vec![Arc::new(
        GuardDatasetObserver::new(recorder.clone()),
    )]));
    let runtime = Runtime::builder()
        .with_governance(Arc::new(GovernancePipeline::new().with(guard)))
        .with_event_sink(sink)
        .with_plugin(ProviderPlugin::new(provider))
        .with_capability(Arc::new(EchoTool {
            id: CapabilityId::new("tool.echo").unwrap(),
        }))
        .with_default_model(MODEL)
        .build()
        .await
        .unwrap();

    let outcome = runtime
        .execute(TurnRequest::new(SessionId::new(), "echo"))
        .await
        .unwrap();
    assert!(outcome.trace.events().any(|event| matches!(
        event,
        apeireth_runtime::canonical::TraceEvent::CapabilityCompleted {
            succeeded: true,
            ..
        }
    )));

    let samples = recorder.load_supervised_samples();
    let tool_sample = samples
        .iter()
        .find(|sample| sample.action_id == "call-cognitive-1")
        .expect("tool classification must correlate with capability completion");
    assert_eq!(tool_sample.execution_outcome.as_deref(), Some("success"));
    assert!(tool_sample.classifier_prediction.is_some());
    assert!(samples
        .iter()
        .any(|sample| { sample.features["schema_version"] == "AgentChainFeatureV1" }));
}
