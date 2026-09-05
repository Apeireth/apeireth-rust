# Yanshu / Memory donor mapping — vNext

日期：2026-09-05

这份映射记录的是“可吸收的语义”和“不能假装已经吸收的实现”。当前仓库的 canonical owner 仍是 `crates/engine/memory`；旧实现只作为行为和兼容性参照。

## 结论

| 能力 | 参照来源 | 处理 | 当前证据 |
|---|---|---|---|
| 情节 / 工作 / 语义 / 关系四层记忆 | `legacy/donor/apeireth-companion` 的 memory 相关模块、现有 `apeireth-memory` | ADAPT | `MemoryCoordinator` 保留现有 SQLite、偏好、图谱边界，并增加 scope / provenance / ranking 合同 |
| 关闭世界记忆注入 | 旧 `memory_injection` 语义 | ADAPT | 统一使用 `<governed_memory>…</governed_memory>`；旧无 coordinator 分支也不再发出 `Retrieved memory context` |
| 偏好与画像 | 旧 preference / proactive memory 语义 | ADAPT | `PersonaMemoryProfile` + revision-checked `PersonaProfileDelta`；规则提取器仅作离线降级，不声称等同模型提取 |
| 混合检索与中文回退 | 旧 BM25 / vector 思路 | ADAPT | `HybridRetrievalPipeline`：scope filter → lexical/vector union → dedup → centralized ranking → budget；无 embedding 时可解释地 lexical fallback |
| 向量持久化 | 旧 vector 仅内存语义 | ADAPT | `VectorRecord` 保存 model、dimension、content hash、更新时间；SQLite migration v3 + metadata store |
| 记忆压缩 / continuity | 旧连续性压缩语义 | ADAPT | `ContinuityState` 扩展 rolling summary、goals、unresolved threads 等；`ContextWindowManager` 只改变 provider projection，不改 transcript |
| 远程 embedding / LLM extraction | 旧扩展或未定 canonical provider | DEFER | 已定义 `EmbeddingProvider` / `MemoryExtractor` trait；没有把 HTTP、密钥或未验证模型写进 Memory owner |
| 外部 memory provider（Mongo / remote 等） | `legacy/donor/apeireth-memory-extensions` | REJECT / DEFER | 当前没有批准的 canonical repository contract，不恢复旧 provider bridge |
| Yanshuai-AI / OnDeviceAI 的 Windows C# UWP / D3D11 实现 | `docs/04-internal/borrow-from-jimmyxiao2009.md` | REJECT | 与跨平台 Rust + provider capability 架构不匹配；不移植 UI / 平台绑定代码 |

## 不变量

- scope 默认是显式可见集合；旧 session-linked episode 没有 metadata 时只按 session scope 解释。
- forget 仍由 `MemoryGovernanceStore` 决定，recall、context compile、consolidation 都先过滤 forgotten 状态。
- provenance 只保存来源身份和 trace/request 引用，不把 prompt、secret、chain-of-thought 放进 Guard feature 或 Memory control plane。
- embedding 与 reranker 是可选适配器；适配器不可用不会让本地 lexical recall 伪装成 semantic recall。

## 证据边界

`crates/engine/runtime-assembly/tests/cognitive_vnext_production.rs` 驱动真实 production module assembly、runtime、scripted provider 和 SQLite backend，证明“写入 → provider 请求注入 → 重启 → forget 后不再召回”。它没有证明远程 embedding 质量、模型抽取质量或跨平台桌面发布包质量；这些保持 DEFER/待 CI。

