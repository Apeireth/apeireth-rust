use std::sync::Arc;

use apeireth_memory::{
    MemoryCoordinator, MemoryGovernanceError, MemoryGovernanceStore, MemoryLayerKind,
    MemoryRecallQuery, MemoryWritebackEntry, SqliteMemoryStore,
};

fn setup_coordinator() -> Arc<MemoryCoordinator> {
    let store = Arc::new(SqliteMemoryStore::open_in_memory().expect("open in-memory sqlite store"));
    let backend = store.clone();
    let governance: Arc<dyn MemoryGovernanceStore> = store;
    Arc::new(MemoryCoordinator::new(backend, governance))
}

#[test]
fn test_writeback_and_multi_layer_recall() {
    let coord = setup_coordinator();
    let session = "test-session-1";

    let entry = MemoryWritebackEntry::new(
        session,
        "user",
        "Rust is our chosen language for performance",
    );
    let ep_id = coord.writeback(&entry).expect("writeback should succeed");
    assert!(!ep_id.is_empty());

    let query = MemoryRecallQuery::new(session, "Rust language")
        .with_limit(5)
        .with_max_chars(1000);

    let result = coord.recall(&query).expect("recall should succeed");
    assert!(
        !result.items.is_empty(),
        "Should recall at least one memory"
    );

    let found = result
        .items
        .iter()
        .any(|item| item.content.contains("Rust"));
    assert!(found, "Should find the recalled content about Rust");
}

#[test]
fn test_governance_forget_strictly_excluded_from_recall() {
    let coord = setup_coordinator();
    let session = "sess-forget";

    let ep1 = coord
        .writeback(&MemoryWritebackEntry::new(
            session,
            "user",
            "Memory to keep",
        ))
        .unwrap();
    let ep2 = coord
        .writeback(&MemoryWritebackEntry::new(
            session,
            "user",
            "Secret info to forget",
        ))
        .unwrap();

    // Verify both exist before forgetting
    let q1 = MemoryRecallQuery::new(session, "info").with_limit(10);
    let r1 = coord.recall(&q1).unwrap();
    assert!(r1.items.iter().any(|i| i.id == ep2));

    // Forget ep2 via governance
    coord
        .forget_episode(&ep2, Some("User requested erasure"), 0)
        .expect("forget should succeed");

    // Clear working memory for session to force episodic layer governance read
    let q2 = MemoryRecallQuery::new(session, "info")
        .with_layers(vec![MemoryLayerKind::Episodic])
        .with_limit(10);
    let r2 = coord.recall(&q2).unwrap();

    // Verify forgotten item is strictly excluded
    assert!(
        !r2.items.iter().any(|i| i.id == ep2),
        "Forgotten episode must NEVER be returned in recall!"
    );
    assert!(
        r2.items.iter().any(|i| i.id == ep1),
        "Non-forgotten episode must remain accessible"
    );
    assert!(
        r2.governance_filtered >= 1,
        "Governance filtered counter must record soft-deleted episodes"
    );
}

#[test]
fn test_governance_protect_blocks_forget() {
    let coord = setup_coordinator();
    let session = "sess-protect";

    let ep = coord
        .writeback(&MemoryWritebackEntry::new(
            session,
            "user",
            "Important constitutional invariant",
        ))
        .unwrap();

    // Protect episode
    let prot = coord
        .protect_episode(&ep, 0)
        .expect("protect should succeed");
    assert!(prot.protected);

    // Attempt to forget protected episode
    let err = coord
        .forget_episode(&ep, Some("try to forget"), 1)
        .unwrap_err();
    assert!(
        matches!(err, MemoryGovernanceError::Protected(_)),
        "Protected episode must reject forget operation"
    );

    // Unprotect and forget should succeed
    coord
        .unprotect_episode(&ep, 1)
        .expect("unprotect should succeed");
    let forgotten = coord
        .forget_episode(&ep, Some("now allowed"), 2)
        .expect("forget should succeed");
    assert_eq!(forgotten.status.as_str(), "forgotten");
}

#[test]
fn test_governance_content_override_reflected() {
    let coord = setup_coordinator();
    let session = "sess-override";

    let ep = coord
        .writeback(&MemoryWritebackEntry::new(
            session,
            "user",
            "User preferred python scripts",
        ))
        .unwrap();

    // User edits memory
    coord
        .update_episode_content(&ep, "User preferred Rust binaries", Some("user"), 0)
        .expect("update content should succeed");

    // Recall from episodic layer
    let q = MemoryRecallQuery::new(session, "preferred")
        .with_layers(vec![MemoryLayerKind::Episodic])
        .with_limit(5);
    let result = coord.recall(&q).unwrap();

    let item = result
        .items
        .iter()
        .find(|i| i.id == ep)
        .expect("item should be found");
    assert_eq!(item.content, "User preferred Rust binaries");
    assert!(!item.content.contains("python"));
}

#[test]
fn test_budget_truncation_and_deduplication() {
    let coord = setup_coordinator();
    let session = "sess-budget";

    // Write duplicates and long entries
    for _ in 0..5 {
        coord
            .writeback(&MemoryWritebackEntry::new(
                session,
                "assistant",
                "Identical duplicate statement",
            ))
            .unwrap();
    }

    let q = MemoryRecallQuery::new(session, "duplicate")
        .with_limit(10)
        .with_max_chars(200);

    let result = coord.recall(&q).unwrap();
    // Duplicates should be suppressed
    assert_eq!(
        result.items.len(),
        1,
        "Duplicate items should be deduplicated"
    );
    assert!(
        result.total_chars <= 200,
        "Should stay strictly within max_chars budget"
    );
}

#[test]
fn test_closed_world_context_compiler_sanitizes_secrets() {
    let coord = setup_coordinator();
    let session = "sess-compiler";

    coord
        .writeback(&MemoryWritebackEntry::new(
            session,
            "tool",
            "Authenticated with token=super_secret_jwt_payload_12345 to host",
        ))
        .unwrap();

    let q = MemoryRecallQuery::new(session, "authenticated").with_limit(5);
    let overlay = coord
        .compile_prompt_overlay(&q)
        .unwrap()
        .expect("overlay should not be empty");

    assert!(overlay.starts_with("<governed_memory"));
    assert!(overlay.contains("Non-authoritative"));
    assert!(overlay.ends_with("</governed_memory>"));
    assert!(
        !overlay.contains("super_secret_jwt_payload_12345"),
        "Raw secrets must never leak to prompt overlay!"
    );
    assert!(
        overlay.contains("[REDACTED]"),
        "Secret should be replaced with redacted marker"
    );
}

#[test]
fn test_continuity_state_compression() {
    let coord = setup_coordinator();
    let session = "sess-continuity";

    coord
        .writeback(&MemoryWritebackEntry::new(
            session,
            "user",
            "We must never run tests on dirty git branches in crates/engine/guard",
        ))
        .unwrap();
    coord
        .writeback(&MemoryWritebackEntry::new(
            session,
            "assistant",
            "Understood. Editing src/main.rs to add verification",
        ))
        .unwrap();

    let continuity = coord.compress_continuity(session, 500).unwrap();
    assert_eq!(continuity.session_id, session);
    assert_eq!(continuity.turn_count, 2);
    assert!(
        !continuity.active_constraints.is_empty(),
        "Should capture 'must' constraints"
    );
    assert!(continuity
        .key_entities
        .iter()
        .any(|e| e.contains("main.rs") || e.contains("guard")));
}

#[test]
fn test_consolidation_job() {
    let coord = setup_coordinator();
    let session = "sess-consolidation";

    coord
        .writeback(&MemoryWritebackEntry::new(
            session,
            "user",
            "Please resolve the compilation error",
        ))
        .unwrap();
    coord
        .writeback(&MemoryWritebackEntry::new(
            session,
            "tool",
            "error: unresolved import",
        ))
        .unwrap();
    coord
        .writeback(&MemoryWritebackEntry::new(
            session,
            "assistant",
            "fixed: imported module correctly",
        ))
        .unwrap();

    let report = coord.run_consolidation(session).unwrap();
    assert_eq!(report.session_id, session);
    assert_eq!(report.episodes_evaluated, 3);
    assert_eq!(report.user_requests, 1);
    assert_eq!(report.tool_invocations, 1);
    assert!(!report.extracted_insights.is_empty());
}
