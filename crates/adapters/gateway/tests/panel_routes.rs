//! Panel introspection routes: contract shapes with backends attached,
//! and honest `501 unsupported` degradation without them.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use apeireth_gateway::{
    canonical_router_with_panels, AuditDto, EpisodeDto, EpisodeMutationDto, GrantDto,
    GrantMutationDto, GraphEdgeDto, GraphNodeDto, MemoryGraphDto, OrganDto, PanelData,
    SessionSummaryDto, ToolDto, TraceDetailDto, TraceSpanDto, TraceSummaryDto,
};
use apeireth_runtime::canonical::Runtime;
use async_trait::async_trait;
use axum::body::Body;
use axum::http::{Request, StatusCode};
use http_body_util::BodyExt;
use serde_json::json;
use tower::ServiceExt;

struct MockPanels {
    episodes: Mutex<Vec<EpisodeDto>>,
    /// id -> (protected, tombstoned, rev)
    flags: Mutex<HashMap<String, (bool, bool, u64)>>,
}

impl MockPanels {
    fn new() -> Self {
        let seed = EpisodeDto {
            id: "e1".into(),
            timestamp: 10_000,
            role: "assistant".into(),
            content: "阿佩瑞斯记得主人喜欢古风".into(),
            session_id: "s1".into(),
            category: None,
            importance: None,
            protected: Some(false),
            status: Some("active".into()),
        };
        Self {
            episodes: Mutex::new(vec![seed]),
            flags: Mutex::new(HashMap::from([("e1".into(), (false, false, 0))])),
        }
    }

    fn mutate(
        &self,
        id: &str,
        expected_rev: u64,
        apply: impl FnOnce(&mut (bool, bool, u64)),
    ) -> Result<EpisodeMutationDto, String> {
        let mut flags = self.flags.lock().unwrap();
        let entry = flags
            .get_mut(id)
            .ok_or_else(|| format!("episode {id} not found"))?;
        if entry.2 != expected_rev {
            return Err(format!(
                "revision conflict: expected {expected_rev}, current {}",
                entry.2
            ));
        }
        apply(entry);
        entry.2 += 1;
        Ok(EpisodeMutationDto {
            ok: true,
            rev: entry.2,
            id: id.to_string(),
            status: if entry.1 { "forgotten" } else { "active" }.into(),
            protected: entry.0,
            revision: entry.2,
            content: String::new(),
        })
    }
}

#[async_trait]
impl PanelData for MockPanels {
    async fn list_sessions(&self) -> Result<Vec<SessionSummaryDto>, String> {
        Ok(vec![SessionSummaryDto {
            id: "s1".into(),
            title: Some("你好,阿佩瑞斯".into()),
            created_at: 1,
            updated_at: 2,
            message_count: 3,
            revision: 4,
        }])
    }

    async fn list_tools(&self) -> Result<Vec<ToolDto>, String> {
        Ok(vec![ToolDto {
            name: "tool.repo".into(),
            description: "仓库检查".into(),
            args_schema: None,
            source: "builtin".into(),
            permission: "granted".into(),
            available: true,
        }])
    }

    async fn list_traces(&self, _limit: usize) -> Result<Vec<TraceSummaryDto>, String> {
        Ok(vec![TraceSummaryDto {
            trace_id: "t1".into(),
            span_count: 1,
            started_at: 5,
            root_span: TraceSpanDto {
                span_id: "t1-0".into(),
                parent_span_id: None,
                kind: "turn".into(),
                actor: "runtime".into(),
                status: "ok".into(),
                summary: None,
                started_at: 5,
                ended_at: None,
                session_id: None,
            },
        }])
    }

    async fn trace_detail(&self, trace_id: &str) -> Result<Option<TraceDetailDto>, String> {
        if trace_id == "t1" {
            Ok(Some(TraceDetailDto {
                trace_id: "t1".into(),
                spans: vec![TraceSpanDto {
                    span_id: "t1-0".into(),
                    parent_span_id: None,
                    kind: "turn".into(),
                    actor: "runtime".into(),
                    status: "ok".into(),
                    summary: None,
                    started_at: 5,
                    ended_at: None,
                    session_id: None,
                }],
            }))
        } else {
            Ok(None)
        }
    }

    async fn list_audit(&self, _limit: usize) -> Result<Vec<AuditDto>, String> {
        Ok(vec![AuditDto {
            ts: 9,
            event: "chat.turn.completed".into(),
            service: "gateway".into(),
            detail: Some("session=s1".into()),
        }])
    }

    async fn append_audit(&self, _event: &str, _detail: Option<&str>) {}

    async fn append_trace(&self, _trace_id: &str, _spans: Vec<TraceSpanDto>) {}

    fn supports_memory(&self) -> bool {
        true
    }

    async fn list_episodes(
        &self,
        session: Option<&str>,
        query: Option<&str>,
        limit: usize,
    ) -> Result<Vec<EpisodeDto>, String> {
        let flags = self.flags.lock().unwrap();
        let mut out: Vec<EpisodeDto> = self
            .episodes
            .lock()
            .unwrap()
            .iter()
            .filter(|ep| !flags.get(&ep.id).map(|f| f.1).unwrap_or(false))
            .filter(|ep| session.map(|s| ep.session_id == s).unwrap_or(true))
            .filter(|ep| {
                query
                    .map(|q| ep.content.to_lowercase().contains(&q.to_lowercase()))
                    .unwrap_or(true)
            })
            .cloned()
            .collect();
        out.sort_by(|a, b| b.timestamp.cmp(&a.timestamp));
        out.truncate(limit);
        Ok(out)
    }

    async fn append_episode(
        &self,
        session: &str,
        role: &str,
        content: &str,
    ) -> Result<EpisodeDto, String> {
        let id = format!("e{}", self.episodes.lock().unwrap().len() + 1);
        let dto = EpisodeDto {
            id: id.clone(),
            timestamp: 20_000,
            role: role.into(),
            content: content.into(),
            session_id: session.into(),
            category: None,
            importance: None,
            protected: Some(false),
            status: Some("active".into()),
        };
        self.episodes.lock().unwrap().push(dto.clone());
        self.flags.lock().unwrap().insert(id, (false, false, 0));
        Ok(dto)
    }

    async fn protect_episode(
        &self,
        id: &str,
        expected_rev: u64,
    ) -> Result<EpisodeMutationDto, String> {
        self.mutate(id, expected_rev, |entry| entry.0 = true)
    }

    async fn unprotect_episode(
        &self,
        id: &str,
        expected_rev: u64,
    ) -> Result<EpisodeMutationDto, String> {
        self.mutate(id, expected_rev, |entry| entry.0 = false)
    }

    async fn forget_episode(
        &self,
        id: &str,
        expected_rev: u64,
        _reason: Option<&str>,
    ) -> Result<EpisodeMutationDto, String> {
        self.mutate(id, expected_rev, |entry| entry.1 = true)
    }

    async fn memory_graph(&self) -> Result<MemoryGraphDto, String> {
        let episodes = self.episodes.lock().unwrap();
        let flags = self.flags.lock().unwrap();
        let mut nodes = vec![GraphNodeDto {
            id: "session:s1".into(),
            label: "会话一".into(),
            kind: "session".into(),
        }];
        let mut edges = Vec::new();
        for ep in episodes
            .iter()
            .filter(|ep| !flags.get(&ep.id).map(|f| f.1).unwrap_or(false))
        {
            nodes.push(GraphNodeDto {
                id: ep.id.clone(),
                label: ep.content.chars().take(20).collect(),
                kind: "episode".into(),
            });
            edges.push(GraphEdgeDto {
                from: "session:s1".into(),
                to: ep.id.clone(),
                weight: 1.0,
                label: None,
            });
        }
        Ok(MemoryGraphDto { nodes, edges })
    }

    fn supports_permissions(&self) -> bool {
        true
    }

    async fn list_grants(&self) -> Result<Vec<GrantDto>, String> {
        Ok(vec![GrantDto {
            permission: "execute_tool:tool.repo".into(),
            capability: "tool.repo".into(),
            granted_at: None,
        }])
    }

    async fn revoke_grant(&self, capability: &str) -> Result<GrantMutationDto, String> {
        if capability == "tool.repo" {
            Ok(GrantMutationDto { ok: true })
        } else {
            Err(format!("no grant for {capability}"))
        }
    }

    fn supports_organs(&self) -> bool {
        true
    }

    async fn list_organs(&self) -> Result<Vec<OrganDto>, String> {
        Ok(vec![OrganDto {
            id: "W1".into(),
            name: "World Model".into(),
            enabled: false,
            description: None,
        }])
    }
}

fn get(uri: &str) -> Request<Body> {
    Request::builder()
        .uri(uri)
        .body(Body::empty())
        .expect("request")
}

fn post(uri: &str, body: serde_json::Value) -> Request<Body> {
    Request::builder()
        .method("POST")
        .uri(uri)
        .header("content-type", "application/json")
        .body(Body::from(body.to_string()))
        .expect("request")
}

async fn body_json(response: axum::response::Response) -> serde_json::Value {
    let status = response.status();
    let bytes = response.into_body().collect().await.unwrap().to_bytes();
    let text = String::from_utf8_lossy(&bytes).to_string();
    serde_json::from_slice(&bytes)
        .unwrap_or_else(|e| panic!("status={status} body={text:?} parse_err={e}"))
}

#[tokio::test]
async fn panel_routes_serve_contract_shapes() {
    let runtime = Arc::new(Runtime::builder().build().await.unwrap());
    let router = canonical_router_with_panels(runtime, Some(Arc::new(MockPanels::new())));

    let sessions = body_json(
        router
            .clone()
            .oneshot(get("/v1/panel/sessions"))
            .await
            .unwrap(),
    )
    .await;
    assert_eq!(sessions["sessions"][0]["id"], "s1");
    assert_eq!(sessions["sessions"][0]["title"], "你好,阿佩瑞斯");
    assert_eq!(sessions["sessions"][0]["message_count"], 3);

    let tools = body_json(router.clone().oneshot(get("/v1/tools/list")).await.unwrap()).await;
    assert_eq!(tools["tools"][0]["name"], "tool.repo");
    assert_eq!(tools["tools"][0]["permission"], "granted");

    let traces = body_json(
        router
            .clone()
            .oneshot(get("/v1/panel/traces?limit=5"))
            .await
            .unwrap(),
    )
    .await;
    assert_eq!(traces["traces"][0]["trace_id"], "t1");
    assert_eq!(traces["traces"][0]["span_count"], 1);

    let detail = body_json(
        router
            .clone()
            .oneshot(get("/v1/panel/traces/t1"))
            .await
            .unwrap(),
    )
    .await;
    assert_eq!(detail["trace_id"], "t1");
    assert_eq!(detail["spans"][0]["kind"], "turn");

    let missing = router
        .clone()
        .oneshot(get("/v1/panel/traces/nope"))
        .await
        .unwrap();
    assert_eq!(missing.status(), StatusCode::NOT_FOUND);

    let audit = body_json(
        router
            .clone()
            .oneshot(get("/v1/panel/audit?limit=10"))
            .await
            .unwrap(),
    )
    .await;
    assert_eq!(audit["events"][0]["event"], "chat.turn.completed");

    // ---- memory surface ----
    let episodes = body_json(
        router
            .clone()
            .oneshot(get("/v1/panel/memory/episodes?limit=10"))
            .await
            .unwrap(),
    )
    .await;
    assert_eq!(episodes["episodes"].as_array().unwrap().len(), 1);
    assert_eq!(episodes["episodes"][0]["id"], "e1");
    assert_eq!(episodes["episodes"][0]["protected"], false);

    let searched = body_json(
        router
            .clone()
            .oneshot(get("/v1/panel/memory/episodes?q=古风"))
            .await
            .unwrap(),
    )
    .await;
    assert_eq!(searched["episodes"].as_array().unwrap().len(), 1);
    let not_found = body_json(
        router
            .clone()
            .oneshot(get("/v1/panel/memory/episodes?q=不存在"))
            .await
            .unwrap(),
    )
    .await;
    assert_eq!(not_found["episodes"].as_array().unwrap().len(), 0);

    let appended = router
        .clone()
        .oneshot(post(
            "/v1/memory/append",
            json!({ "session": "s1", "role": "user", "content": "新记忆" }),
        ))
        .await
        .unwrap();
    assert_eq!(appended.status(), StatusCode::CREATED);
    let appended = body_json(appended).await;
    assert_eq!(appended["id"], "e2");
    assert_eq!(appended["protected"], false);

    let protected = body_json(
        router
            .clone()
            .oneshot(post(
                "/v1/apeireth/memory/episodes/e2/protect",
                json!({ "expected_rev": 0 }),
            ))
            .await
            .unwrap(),
    )
    .await;
    assert_eq!(protected["ok"], true);
    assert_eq!(protected["rev"], 1);

    let conflict = router
        .clone()
        .oneshot(post(
            "/v1/apeireth/memory/episodes/e2/protect",
            json!({ "expected_rev": 0 }),
        ))
        .await
        .unwrap();
    assert_eq!(conflict.status(), StatusCode::INTERNAL_SERVER_ERROR);

    let forgotten = body_json(
        router
            .clone()
            .oneshot(post(
                "/v1/apeireth/memory/episodes/e2/forget",
                json!({ "expected_rev": 1, "reason": "测试遗忘" }),
            ))
            .await
            .unwrap(),
    )
    .await;
    assert_eq!(forgotten["ok"], true);
    assert_eq!(forgotten["rev"], 2);

    let after = body_json(
        router
            .clone()
            .oneshot(get("/v1/panel/memory/episodes?limit=10"))
            .await
            .unwrap(),
    )
    .await;
    assert_eq!(
        after["episodes"].as_array().unwrap().len(),
        1,
        "forgotten hidden"
    );

    let graph = body_json(
        router
            .clone()
            .oneshot(get("/v1/panel/graph"))
            .await
            .unwrap(),
    )
    .await;
    assert_eq!(graph["nodes"][0]["kind"], "session");
    assert!(graph["edges"].as_array().unwrap().len() >= 1);

    // ---- permissions + organs ----
    let grants = body_json(
        router
            .clone()
            .oneshot(get("/v1/panel/grants"))
            .await
            .unwrap(),
    )
    .await;
    assert_eq!(grants["grants"][0]["capability"], "tool.repo");
    assert_eq!(grants["grants"][0]["permission"], "execute_tool:tool.repo");

    let revoked = body_json(
        router
            .clone()
            .oneshot(post(
                "/v1/panel/grants/revoke",
                json!({ "capability": "tool.repo" }),
            ))
            .await
            .unwrap(),
    )
    .await;
    assert_eq!(revoked["ok"], true);

    let missing_grant = router
        .clone()
        .oneshot(post(
            "/v1/panel/grants/revoke",
            json!({ "capability": "tool.nope" }),
        ))
        .await
        .unwrap();
    assert_eq!(missing_grant.status(), StatusCode::INTERNAL_SERVER_ERROR);

    let organs = body_json(router.clone().oneshot(get("/v1/organs")).await.unwrap()).await;
    assert_eq!(organs["organs"][0]["id"], "W1");
    assert_eq!(organs["organs"][0]["enabled"], false);

    let caps = body_json(
        router
            .clone()
            .oneshot(get("/v1/apeireth/capabilities"))
            .await
            .unwrap(),
    )
    .await;
    let groups = caps["capabilities"].as_array().unwrap();
    let find_group = |name: &str| groups.iter().find(|g| g["name"] == name).unwrap();
    assert_eq!(find_group("health")["capabilities"][0]["id"], "health");
    assert_eq!(find_group("sessions")["capabilities"][0]["supported"], true);
    assert_eq!(find_group("memory")["capabilities"][0]["supported"], true);
    let memory_update = find_group("memory")["capabilities"]
        .as_array()
        .unwrap()
        .iter()
        .find(|cap| cap["id"] == "memory.update")
        .expect("memory.update must remain in the universe");
    assert_eq!(memory_update["supported"], false);
    assert_eq!(memory_update["available"], false);
    assert_eq!(memory_update["reason"], "not_implemented");
    assert_eq!(find_group("memory")["capabilities"][0]["id"], "memory.read");
    let permission_ids: Vec<&str> = find_group("permissions")["capabilities"]
        .as_array()
        .unwrap()
        .iter()
        .map(|cap| cap["id"].as_str().unwrap())
        .collect();
    assert!(
        permission_ids.contains(&"permissions.approval.read"),
        "canonical approval read id missing: {permission_ids:?}"
    );
    assert!(
        permission_ids.contains(&"permissions.approval.resolve"),
        "canonical approval resolve id missing: {permission_ids:?}"
    );
    let approvals_read = find_group("permissions")["capabilities"]
        .as_array()
        .unwrap()
        .iter()
        .find(|cap| cap["id"] == "approvals.read")
        .expect("compatibility alias approvals.read");
    assert_eq!(
        approvals_read["alias_of"], "permissions.approval.read",
        "approvals.read must be an explicit alias, not a second taxonomy"
    );
    assert_eq!(find_group("trace")["capabilities"][0]["supported"], true);
    assert_eq!(
        find_group("activity")["capabilities"][0]["supported"],
        true,
        "the SSE bus is core gateway infrastructure, always available"
    );
    let grants = find_group("permissions")["capabilities"]
        .as_array()
        .unwrap()
        .iter()
        .find(|cap| cap["id"] == "permissions.grants.read")
        .expect("permissions.grants.read");
    assert_eq!(grants["supported"], true);
    let revoke = find_group("permissions")["capabilities"]
        .as_array()
        .unwrap()
        .iter()
        .find(|cap| cap["id"] == "permissions.revoke")
        .expect("permissions.revoke");
    assert_eq!(
        revoke["supported"], true,
        "revoke is available alongside grants.read"
    );
    assert_eq!(find_group("organs")["capabilities"][0]["id"], "organs.list");
    assert_eq!(find_group("organs")["capabilities"][0]["supported"], true);

    let safety = find_group("safety");
    let safety_caps = safety["capabilities"].as_array().unwrap();
    assert!(safety_caps
        .iter()
        .any(|c| c["id"] == "safety.guard.status.read"));
    assert!(safety_caps
        .iter()
        .any(|c| c["id"] == "safety.guard.events.read"));
    assert!(safety_caps
        .iter()
        .any(|c| c["id"] == "safety.guard.evaluate"));

    let workbench = find_group("workbench");
    assert_eq!(workbench["capabilities"][0]["id"], "workbench.turn.read");

    let voice = find_group("voice");
    let voice_duplex = &voice["capabilities"][0];
    assert_eq!(voice_duplex["id"], "voice.duplex");
    assert_eq!(voice_duplex["supported"], false);
    assert_eq!(voice_duplex["available"], false);
    assert_eq!(voice_duplex["reason"], "not_assembled");

    let subagents = find_group("subagents");
    let subagents_cap = &subagents["capabilities"][0];
    assert_eq!(subagents_cap["id"], "subagents.orchestration");
    assert_eq!(subagents_cap["supported"], false);
    assert_eq!(subagents_cap["available"], false);
    assert_eq!(subagents_cap["reason"], "not_assembled");
}

#[tokio::test]
async fn panel_routes_degrade_to_501_without_backends() {
    let runtime = Arc::new(Runtime::builder().build().await.unwrap());
    let router = canonical_router_with_panels(runtime, None);

    let response = router
        .clone()
        .oneshot(get("/v1/panel/sessions"))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::NOT_IMPLEMENTED);
    let body = body_json(response).await;
    assert_eq!(body["error"]["code"], "unsupported");
    assert!(body["error"]["message"]
        .as_str()
        .unwrap()
        .contains("sessions.read"));

    let memory = router
        .clone()
        .oneshot(get("/v1/panel/memory/episodes"))
        .await
        .unwrap();
    assert_eq!(memory.status(), StatusCode::NOT_IMPLEMENTED);
    let body = body_json(memory).await;
    assert_eq!(body["error"]["code"], "unsupported");

    let grants = router
        .clone()
        .oneshot(get("/v1/panel/grants"))
        .await
        .unwrap();
    assert_eq!(grants.status(), StatusCode::NOT_IMPLEMENTED);
    let organs = router.clone().oneshot(get("/v1/organs")).await.unwrap();
    assert_eq!(organs.status(), StatusCode::NOT_IMPLEMENTED);

    let guard_status = router
        .clone()
        .oneshot(get("/v1/safety/guard/status"))
        .await
        .unwrap();
    assert_eq!(guard_status.status(), StatusCode::NOT_IMPLEMENTED);
    let body = body_json(guard_status).await;
    assert_eq!(body["error"]["code"], "unsupported");
    assert!(body["error"]["message"]
        .as_str()
        .unwrap()
        .contains("safety.guard.status.read"));

    let guard_events = router
        .clone()
        .oneshot(get("/v1/safety/guard/events"))
        .await
        .unwrap();
    assert_eq!(guard_events.status(), StatusCode::NOT_IMPLEMENTED);

    let guard_eval = router
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/v1/safety/guard/evaluate")
                .header("content-type", "application/json")
                .body(Body::from(
                    json!({
                        "capability_id": "tool.shell",
                        "arguments": { "command": "ls" }
                    })
                    .to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(guard_eval.status(), StatusCode::NOT_IMPLEMENTED);

    let wb = router
        .clone()
        .oneshot(get("/v1/workbench/turn"))
        .await
        .unwrap();
    assert_eq!(wb.status(), StatusCode::NOT_IMPLEMENTED);
    let body = body_json(wb).await;
    assert_eq!(body["error"]["code"], "unsupported");
    assert!(body["error"]["message"]
        .as_str()
        .unwrap()
        .contains("workbench.turn.read"));

    // The manifest stays available and honest even without backends.
    let caps = body_json(
        router
            .clone()
            .oneshot(get("/v1/apeireth/capabilities"))
            .await
            .unwrap(),
    )
    .await;
    let groups = caps["capabilities"].as_array().unwrap();
    let sessions = groups.iter().find(|g| g["name"] == "sessions").unwrap();
    assert_eq!(sessions["capabilities"][0]["supported"], false);
    let memory_group = groups.iter().find(|g| g["name"] == "memory").unwrap();
    assert_eq!(memory_group["capabilities"][0]["supported"], false);
    let safety_group = groups.iter().find(|g| g["name"] == "safety").unwrap();
    assert_eq!(safety_group["capabilities"][0]["supported"], false);
    let wb_group = groups.iter().find(|g| g["name"] == "workbench").unwrap();
    assert_eq!(wb_group["capabilities"][0]["supported"], false);
}
