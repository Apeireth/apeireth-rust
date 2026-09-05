# Apeireth Cognitive Infrastructure vNext

日期：2026-09-05

## 交付边界

工作发生在新分支 `feature/cognitive-infrastructure-vnext`，仓库实际路径为 `apeireth-rust/`。原有未跟踪 RC bundle 与 `fix.patch/` 未纳入本次修改。

状态词含义：

- **IMPLEMENTED**：canonical crate 有真实类型、逻辑和单测。
- **PRODUCTION_WIRED**：从 composition root 能走到真实 runtime / event / storage 路径。
- **VERTICAL_TESTED**：有离线、可重复的真实纵向测试。
- **CI_VERIFIED**：已从远端 CI 取得对应 commit 的成功结果。本次本地分支尚未 push，因此不能标记。

## Guard dataset closure

| 项目 | 状态 | 说明 |
|---|---|---|
| dataset 开关与路径 | PRODUCTION_WIRED | CLI 读取 `APEIRETH_GUARD_DATASET_ENABLED=1` 与 `APEIRETH_GUARD_DATASET_PATH`；默认关闭 |
| 单一 recorder | PRODUCTION_WIRED | guard hook 与 runtime observer 共用同一个 `Arc<DatasetRecorder>` |
| 分类行 | IMPLEMENTED | 增加 `AgentChainFeatureV1`、classifier prediction、weak label；只记录脱敏 feature |
| approval / execution / compensation 行 | IMPLEMENTED | additive v2 records；legacy outcome 仍可读取 |
| action correlation | VERTICAL_TESTED | 精确使用 `(trace_id, action_id)`；多 action trace 不再靠 trace-only 猜标签 |
| export | IMPLEMENTED | `scripts/guard-dataset-export.py`；默认只输出有结果的样本，可显式保留 incomplete |
| gateway / desktop | PRODUCTION_WIRED | gateway 追加 event sink，不覆盖 CLI 已安装的 dataset observer；Desktop 状态页显示 Guard 与 dataset/model 状态 |

## Memory 2.1

已落地：

- `MemoryScope`：Global / User / Persona / Project / Session。
- `MemoryProvenance`：来源、session、trace、request。
- `MemoryRankingConfig` 与 `ScoreComponents`：semantic / lexical / importance / recency / activation / continuity / confidence 集中管理。
- `HybridRetrievalPipeline`：候选生成、scope filter、lexical/vector union、去重、多样性、字符预算和 optional reranker。
- Unicode lexical fallback：英文按词，CJK 按字符 token；无 embedding 仍可检索，但状态明确为 fallback。
- `EmbeddingProvider`、`MemoryReranker`、`MemoryExtractor`：trait 边界已定义；没有把 provider、凭据或 HTTP 客户端塞进 Memory crate。
- Persona profile 的 revision-checked delta 更新。
- `VectorRecord` + SQLite vector metadata migration，包含 model/dimension/content hash。
- continuity state 扩展与 `ContextWindowManager`；compact 只生成 provider-facing projection，永不删除持久 transcript。
- extraction 分类与 consolidation 输出 contract；forget 先于 consolidation / recall 生效。

## Guard 2.0

- `AgentChainFeatureV1` 是固定 schema 名称；不包含 prompt、路径内容、secret 或 raw reasoning。
- graph 增加 ResourceDependency、Retry、AlternativeExecution edge 类型；保留旧 Temporal / Causal / DataFlow / PermissionDependency 兼容读取。
- `ChainRiskClassifier` 可选；默认 `NoClassifier`，因此模型不可用时确定性 Guard 仍成立。
- `DecisionFusion` 规定 deterministic deny 优先，模型必须有对应敏感流/外部 sink 证据才可升级为 deny。
- `EnforcementDirective` 把 block / approval / high-risk mark / session-grant revoke hint 显式化；真正执行仍由 canonical runtime 的 `Decision` 完成。
- Gateway/desktop introspection 输出 classifier available/model version；当前默认生产组装不加载训练模型。

## 生产纵向证据

`cargo test -p apeireth-runtime-assembly --test cognitive_vnext_production --locked`：2 passed。

覆盖：

1. `ProductionCognitiveModules → Runtime → Provider` 的真实 Memory recall；SQLite 重启后保持，forget 后 provider 请求不再包含被遗忘记忆。
2. `BehaviorChainGuardHook → Runtime event sink → GuardDatasetObserver → DatasetRecorder` 的真实 dataset closure；按 action id 关联工具成功结果。

## 本地验证记录

已通过：

- `cargo test -p apeireth-memory --lib --locked`：663 passed。
- `cargo test -p apeireth-guard --lib --locked`：3 passed。
- `cargo test -p apeireth-storage --lib --locked`：110 passed。
- `cargo test -p apeireth-runtime --lib --locked`：55 passed。
- `cargo test -p apeireth-runtime-assembly --test convergence_production_integration --locked`：5 passed。
- `cargo test -p apeireth-runtime-assembly --test cognitive_vnext_production --locked`：2 passed。
- `pnpm test`：7/7 suites passed。
- `pnpm build`：通过。
- `pnpm check`：0 errors，5 个既有 Svelte warning。
- 相关 crate `cargo check --all-targets --locked`：通过。

尚未可标记：

- **CI_VERIFIED：NOT VERIFIED**。本分支未 push，没有对应远端 commit 的 GitHub Actions 结果。
- remote CI、cargo-deny/audit、发布包、真实 provider 网络调用：未声称通过。
- 桌面端改动已接入真实 Guard status API；frontend check/build/test 已通过，仍未把 Tauri release 包构建写成绿色证据。

## 后续明确项

1. 接入经过批准的 embedding provider，并为 model/dimension/content-hash 失效策略增加真实 adapter 测试。
2. 接入真正训练模型前，先用导出脚本检查 schema、action correlation 与脱敏审计。
3. 推送分支后再读取 commit-specific CI；在此之前不把本地验证写成 CI green。
