//! Production Convergence Integration Tests (Section E).
//!
//! Verifies:
//! 1. Memory 2.0 assembly & forget roundtrip:
//!    - Built from `ProductionCognitiveModules`.
//!    - Recalled context contains `<governed_memory>` and never the legacy prefix.
//!    - Forgotten memory is excluded upon runtime reconstruction from same DB.
//! 2. Closed-world context injection tag contract.
//! 3. Guard action ID deterministic format (`act:{req}:{round}:{seq}`) and trace boundary isolation.
//! 4. Timestamp contract: writeback ms -> s, recall s -> ms, recency scoring decay.
//! 5. Dataset loop closure: event-sourced outcome correlation via `GuardDatasetObserver`.

use std::sync::Arc;

use apeireth_core::clock::SystemClock;
use apeireth_core::kernel::{ApprovalId, CapabilityId, Clock, SessionId, Timestamp, TraceId};
use apeireth_governance::{Action, GovernanceHook, GovernanceRequest};
use apeireth_guard::{
    BehaviorChain, BehaviorChainGuardHook, DatasetRecorder, GuardDecision, SafetyObservation,
};
use apeireth_memory::{
    ClosedWorldContextCompiler, EpisodeStore, MemoryCoordinator, MemoryGovernanceStore,
    MemoryRecallQuery, MemoryRecallResult, MemoryWritebackEntry, RecalledMemoryItem,
    SqliteMemoryStore,
};
use apeireth_plugin::memory_backend::MemoryBackend;
use apeireth_runtime::canonical::{RuntimeEvent, RuntimeEventSink, TraceEvent};
use apeireth_runtime_assembly::{
    CognitiveBackends, CognitiveModuleConfig, GuardDatasetObserver, ProductionCognitiveModules,
};
use tempfile::tempdir;

// -------------------------------------------------------------------------
// 1. Memory Assembly & Forget Roundtrip
// -------------------------------------------------------------------------

#[tokio::test]
async fn test_memory_assembly_forget_roundtrip() {
    let dir = tempdir().unwrap();
    let db_path = dir.path().join("memory.sqlite3");

    // Phase 1: Create DB and populate initial episode
    let ep_id = {
        let store = Arc::new(SqliteMemoryStore::open(&db_path).unwrap());
        let backend: Arc<dyn MemoryBackend> = store.clone();
        let governance: Arc<dyn MemoryGovernanceStore> = store.clone();
        let clock: Arc<dyn Clock> = Arc::new(SystemClock);

        let coordinator = Arc::new(MemoryCoordinator::new(backend.clone(), governance.clone()));

        // Writeback an episode via Coordinator
        let mut entry = MemoryWritebackEntry::new(
            "session-alpha",
            "user",
            "Apeireth is a microkernel AI agent runtime",
        );
        entry.timestamp_ms = Some(1_700_000_000_000);
        let ep_id = coordinator.writeback(&entry).unwrap();
        assert!(!ep_id.is_empty());

        // Construct production cognitive modules with coordinator
        let config = CognitiveModuleConfig {
            memory_recall: true,
            memory_writeback: false,
            preference_recall: false,
            self_assessment: false,
            filesystem: false,
            search: false,
            repo: false,
            ..CognitiveModuleConfig::default()
        };
        let backends = CognitiveBackends {
            memory: Some(backend.clone()),
            memory_governance: Some(governance.clone()),
            ..CognitiveBackends::default()
        };
        let _modules = ProductionCognitiveModules::build(config, backends, clock).unwrap();

        // Check recall overlay
        let query = MemoryRecallQuery::new("session-alpha", "Apeireth").with_limit(5);
        let recalled = coordinator.recall(&query).unwrap();
        assert!(
            !recalled.items.is_empty(),
            "must recall the written episode"
        );

        let compiler = ClosedWorldContextCompiler::default();
        let prompt_injection = compiler
            .compile(&recalled, "session-alpha", 4000)
            .expect("must compile prompt injection");

        // Assert contract: <governed_memory> tag present, legacy prefix absent
        assert!(
            prompt_injection.contains("<governed_memory"),
            "overlay must use <governed_memory> wrapper"
        );
        assert!(
            prompt_injection.contains("</governed_memory>"),
            "overlay must close </governed_memory> wrapper"
        );
        assert!(
            !prompt_injection.contains("Retrieved memory context"),
            "legacy prefix 'Retrieved memory context' must never be emitted"
        );

        // Soft-delete / forget the episode
        governance
            .forget_episode(&ep_id, Some("user requested forget"), 0)
            .unwrap();
        ep_id
    };

    // Phase 2: Destroy runtime & reconstruct on the SAME database file
    {
        let store = Arc::new(SqliteMemoryStore::open(&db_path).unwrap());
        let backend: Arc<dyn MemoryBackend> = store.clone();
        let governance: Arc<dyn MemoryGovernanceStore> = store.clone();
        let coordinator = Arc::new(MemoryCoordinator::new(backend.clone(), governance.clone()));

        // Recall again on session-alpha
        let query = MemoryRecallQuery::new("session-alpha", "Apeireth").with_limit(5);
        let recalled = coordinator.recall(&query).unwrap();

        // Forgotten memory MUST be excluded
        let found = recalled
            .items
            .iter()
            .any(|item| item.content.contains("Apeireth is a microkernel") || item.id == ep_id);
        assert!(
            !found,
            "forgotten memory must be excluded from subsequent recall"
        );
    }
}

// -------------------------------------------------------------------------
// 2. Closed-World Memory Contract
// -------------------------------------------------------------------------

#[test]
fn test_closed_world_memory_contract() {
    let compiler = ClosedWorldContextCompiler::default();

    // With empty candidates -> None
    let empty_result = MemoryRecallResult::default();
    assert!(
        compiler.compile(&empty_result, "session-1", 4000).is_none(),
        "empty recall must produce zero prompt injection"
    );

    // With candidates -> strict XML enclosure
    let items = vec![RecalledMemoryItem {
        id: "test-item-1".into(),
        layer: apeireth_memory::MemoryLayerKind::Working,
        content: "Important user instruction".into(),
        timestamp_ms: 1_700_000_000_000,
        score: 0.95,
        importance: 0.8,
        source_ref: Some("working:session-1".into()),
    }];
    let recalled = MemoryRecallResult {
        items,
        total_candidates: 1,
        governance_filtered: 0,
        total_chars: 100,
    };
    let compiled = compiler
        .compile(&recalled, "session-1", 4000)
        .expect("must compile prompt injection");

    assert!(compiled.starts_with("<governed_memory"));
    assert!(compiled.ends_with("</governed_memory>"));
    assert!(compiled.contains("Important user instruction"));
    assert!(!compiled.contains("Retrieved memory context"));
}

// -------------------------------------------------------------------------
// 3. Guard Action ID & Trace Boundary Isolation
// -------------------------------------------------------------------------

#[tokio::test]
async fn test_guard_action_id_and_trace_boundary() {
    let guard_hook = BehaviorChainGuardHook::default();
    let session = SessionId::new();
    let trace_a = TraceId::new();

    // Evaluate 3 sequential completion actions for Trace A
    for _ in 0..3 {
        let action = Action::Completion {
            model: "model-test",
            message_count: 3,
        };
        let req = GovernanceRequest::new(action, session, trace_a, 1);
        let _decision = guard_hook.evaluate(&req).await;
    }

    // Verify Action IDs generated on Trace A: deterministic format `act:{req}:{round}:{seq}`
    let chain_a = guard_hook
        .chain_for_trace(&session, &trace_a.to_string())
        .expect("chain A must exist");
    assert_eq!(chain_a.actions().len(), 3);
    for (i, action) in chain_a.actions().iter().enumerate() {
        let expected_suffix = format!(":1:{}", i);
        assert!(
            action.id.ends_with(&expected_suffix),
            "action id {} must end with :round:seq ({})",
            action.id,
            expected_suffix
        );
        assert!(
            action.id.starts_with("act:"),
            "action id {} must start with act:",
            action.id
        );
    }

    // Now evaluate on Trace B (different trace)
    let trace_b = TraceId::new();
    let action_b = Action::Completion {
        model: "model-test",
        message_count: 1,
    };
    let req_b = GovernanceRequest::new(action_b, session, trace_b, 1);
    let _decision_b = guard_hook.evaluate(&req_b).await;

    // Verify Trace B has its own independent chain DAG
    let chain_b = guard_hook
        .chain_for_trace(&session, &trace_b.to_string())
        .expect("chain B must exist");
    assert_eq!(chain_b.actions().len(), 1);
    assert!(chain_b.actions()[0].id.ends_with(":1:0"));

    // Verify SessionRiskHistory recorded all 4 evaluations (bounded up to 10)
    let history = guard_hook.session_risk_history(&session);
    assert_eq!(
        history.len(),
        4,
        "history must record summary for each evaluation up to cap"
    );
    assert_eq!(history[0].trace_id, trace_a.to_string());
    assert_eq!(history[3].trace_id, trace_b.to_string());
}

// -------------------------------------------------------------------------
// 4. Timestamp Contract & Recency Decay
// -------------------------------------------------------------------------

#[test]
fn test_timestamp_ms_to_s_contract() {
    let store = Arc::new(SqliteMemoryStore::open_in_memory().unwrap());
    let backend: Arc<dyn MemoryBackend> = store.clone();
    let governance: Arc<dyn MemoryGovernanceStore> = store.clone();
    let coordinator = MemoryCoordinator::new(backend.clone(), governance.clone());

    // Writeback with timestamp in ms: 1_700_000_000_123 ms -> should be stored in seconds: 1_700_000_000 s
    let mut write_entry =
        MemoryWritebackEntry::new("ts-session", "user", "Timestamp scaling verification");
    write_entry.timestamp_ms = Some(1_700_000_000_123);
    let ep_id = coordinator.writeback(&write_entry).unwrap();

    // Query raw episode directly from store using EpisodeStore trait
    let raw_ep = EpisodeStore::get_episode(store.as_ref(), &ep_id)
        .unwrap()
        .expect("episode must exist");
    assert_eq!(
        raw_ep.timestamp, 1_700_000_000,
        "Episode.timestamp in DB must be in epoch seconds"
    );

    // Recall via coordinator: should convert seconds -> milliseconds (s * 1000)
    let query = MemoryRecallQuery::new("ts-session", "Timestamp").with_limit(1);
    let result = coordinator.recall(&query).unwrap();
    assert_eq!(result.items.len(), 1);
    assert_eq!(
        result.items[0].timestamp_ms, 1_700_000_000_000,
        "Recall item timestamp_ms must be normalized back to milliseconds"
    );

    // Verify recency decay score: 1 hour old vs 1 year old
    let now_ms = chrono::Utc::now().timestamp_millis();
    let one_hour_ago = now_ms - (3600 * 1000);
    let one_year_ago = now_ms - (365 * 24 * 3600 * 1000);

    let score_recent = apeireth_memory::recency_score(one_hour_ago / 1000, now_ms / 1000);
    let score_old = apeireth_memory::recency_score(one_year_ago / 1000, now_ms / 1000);
    assert!(
        score_recent > score_old * 2.0,
        "1-hour old memory ({}) must score significantly higher than 1-year old memory ({})",
        score_recent,
        score_old
    );
}

// -------------------------------------------------------------------------
// 5. Guard Dataset Loop Closure
// -------------------------------------------------------------------------

#[test]
fn test_guard_dataset_loop_closure() {
    let dir = tempdir().unwrap();
    let dataset_path = dir.path().join("guard-dataset-v1.jsonl");

    let recorder = Arc::new(DatasetRecorder::new(&dataset_path));
    recorder.set_enabled(true);

    let observer = GuardDatasetObserver::new(recorder.clone());

    let trace = TraceId::new();
    let trace_str = trace.to_string();
    let approval_id = ApprovalId::new();

    // 1. Simulate pre-dispatch classification record
    let req = GovernanceRequest::new(
        Action::Completion {
            model: "model-test",
            message_count: 1,
        },
        SessionId::new(),
        trace,
        1,
    );
    let obs = SafetyObservation::from_governance_request(&req, 0, false, Vec::new());
    let chain = BehaviorChain::new("test-session", trace_str.clone());
    let fast_res = apeireth_guard::FastGuardResult::allow();
    let decision = GuardDecision::allow_fast();
    recorder.record_classification("act:test:1:0", &obs, &chain, &fast_res, &decision);

    // 2. Observer receives runtime events
    observer.emit(RuntimeEvent::Trace {
        session: SessionId::new(),
        trace,
        at: Timestamp::now(),
        event: TraceEvent::ApprovalResolved {
            approval_id,
            decision: "approved".into(),
            round: 1,
        },
    });

    observer.emit(RuntimeEvent::Trace {
        session: SessionId::new(),
        trace,
        at: Timestamp::now(),
        event: TraceEvent::CapabilityCompleted {
            capability: CapabilityId::new("tools.test").unwrap(),
            tool_call_id: "call_abc".into(),
            succeeded: true,
            round: 1,
        },
    });

    // 3. Load supervised samples and verify loop closure
    let samples = recorder.load_supervised_samples();
    assert_eq!(
        samples.len(),
        1,
        "must produce 1 correlated supervised sample"
    );
    let sample = &samples[0];
    assert_eq!(sample.trace_id, trace_str);
    assert_eq!(sample.action_id, "act:test:1:0");
    assert_eq!(sample.human_approval.as_deref(), Some("approved"));
    assert_eq!(sample.execution_outcome.as_deref(), Some("success"));
}
