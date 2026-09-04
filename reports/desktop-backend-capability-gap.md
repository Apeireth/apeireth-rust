# Apeireth 2.0 Desktop-Backend Capability Gap Matrix Report

本文档全面盘点当前 Desktop（`frontend/companion-desktop`）与后端（`apeireth-gateway` / `apeireth-runtime` / `apeireth-guard` / `apeireth-memory`）之间的所有交互入口、依赖端点、Capability 映射及真实性治理结果。

---

## 1. 架构原则与真实性约束 (Truthful Contract)

1. **Single Source of Truth (`/v1/apeireth/capabilities`)**：前端视图、操作按钮与能力门禁必须严格依据后端 Capability Manifest 动态决定其展示和交互状态，严禁在前端硬编码假象。
2. **严禁虚假多代理 (No Fake Subagents)**：工作台中不得将单次工具调用或内部步骤冠以“子代理”之名伪装成自主协作多代理系统；当前版本统一归纳为“代理与执行（Agent & Execution）”，如实呈现主 Agent 状态与实际工具调用流。
3. **未组装服务诚实降级 (Honest Degradation for Unassembled Services)**：
   - 全双工双向实时语音流：当前发行版运行时未组装流式 ASR/TTS 与全双工服务网关，Capability Manifest 显式声明 `voice.duplex` 为 `supported: false, available: false, reason: "not_assembled"`；前端语音通话入口降级为带有明确说明的未就绪态与禁用指引。
   - 自主多代理编排系统：显式声明 `subagents.orchestration` 为 `supported: false, available: false, reason: "not_assembled"`。
4. **零 CoT 泄露与零明文凭据 (Zero CoT Leaks & Zero Raw Secrets)**：活动与调用日志严禁包含未脱敏的 Prompt、CoT 思考流或明文密码与 API Key。

---

## 2. 详细能力对照矩阵 (Capability Gap Matrix)

| 前端入口 / 视图 | 依赖后端端点 | Capability 标识 | 实现状态 | 状态说明与真实行为描述 |
| :--- | :--- | :--- | :--- | :--- |
| **工作台 (Workbench) - 目标** | 会话标题 / `/v1/workbench/turn` | `workbench.turn.read` | **已补全** | 真实映射当前会话的目标标题与首条用户意图，消除空洞占位。 |
| **工作台 (Workbench) - 代理与执行** | `/v1/workbench/turn` | `workbench.turn.read` | **已补全 / 已治理** | **彻底剔除“子代理”误导性文案**；更名为“代理与执行”，真实展示“主 Agent”运行状态与当前轮次的工具调用轨迹。 |
| **工作台 (Workbench) - 工具调用卡片** | `/v1/tools/list`, `/v1/workbench/turn` | `tools.list`, `workbench.turn.read` | **已实现** | 呈现真实注册工具清单与运行中/成功/失败状态。 |
| **工作台 (Workbench) - 记忆追溯** | 会话上下文 / `/v1/workbench/turn` | `workbench.turn.read`, `memory.read` | **已实现** | 呈现本轮交互检索与注入的情节记忆与工作上下文。 |
| **安全守门 (Safety Guard) - 状态与拦截** | `/v1/safety/guard/status`, `/v1/safety/guard/events` | `safety.guard.status.read`, `safety.guard.events.read` | **已补全 (生产级)** | 投射 Phase 1 的二阶段（Fast Guard + Behavior Chain Guard）执行统计、拦截记录、风险评分与归因证据。 |
| **安全守门 (Safety Guard) - 干预评估** | `/v1/safety/guard/evaluate` | `safety.guard.evaluate` | **已补全 (生产级)** | 提供 Dry-run 行为评估接口，供工具调用预检与高危动作研判。 |
| **全双工语音通话 (VoiceCallModal)** | `voiceCallManager` / `/v1/apeireth/capabilities` | `voice.duplex` | **已降级 (诚实标记)** | 后端声明 `supported: false, available: false, reason: "not_assembled"`；前端徽标降级为 `● 未组装 (not_assembled)`，弹窗展示未组装说明横幅，杜绝假波形假流式欺骗。 |
| **对话流 (Chat / Companion)** | `/v1/chat/completions` | `chat.completions` | **已实现** | 真实驱动的大模型流式推理与 Canonical Runtime 执行。 |
| **会话历史 (ConversationsView)** | `/v1/panel/sessions` | `sessions.read` | **已实现** | 基于 `SqliteSessionStore` 读取持久化会话概要。 |
| **记忆管理 (MemoryView)** | `/v1/panel/memory/episodes`, `/v1/memory/append` | `memory.read`, `memory.write` | **已实现** | 基于统一记忆架构与 `SqliteMemoryStore` 查询与写入记忆。 |
| **记忆治理 (MemoryView)** | `/v1/apeireth/memory/episodes/:id/*` | `memory.protect`, `memory.unprotect`, `memory.forget` | **已实现** | 基于乐观锁版本（revision）实现保护、取消保护与遗忘/墓碑机制。 |
| **记忆图谱 (MemoryView - Graph)** | `/v1/panel/graph` | `memory.graph.read` | **已实现** | 构建真实存储会话与情节记忆的节点与边拓扑图。 |
| **工具管理与权限 (ToolsView)** | `/v1/tools/list`, `/v1/panel/grants` | `tools.list`, `permissions.grants.read` | **已实现** | 投影运行时实时注册工具与当前有效权限授权清单。 |
| **高危调用审批 (ApprovalInbox)** | `/v1/approvals`, `/v1/approvals/resolve` | `permissions.approval.read`, `permissions.approval.resolve` | **已实现** | 支持运行时挂起的高危操作的主人确认与拒绝闭环。 |
| **权限热撤销 (ToolsView - Revoke)** | `/v1/panel/grants/revoke` | `permissions.revoke` | **已实现** | 支持会话维度的热撤销动态生效。 |
| **行为模块 (Modules / Organs)** | `/v1/modules`, `/v1/organs` | `modules.list`, `organs.list` | **已实现** | 实时读取 Runtime 挂载的认知与行为模块状态。 |
| **调用追踪 (ActivityView - Traces)** | `/v1/panel/traces`, `/v1/panel/traces/:id` | `trace.read` | **已实现** | 检索已完成交互轮次的执行 Span 树形轨迹。 |
| **审计日志 (ActivityView - Audit)** | `/v1/panel/audit` | `audit.read` | **已实现** | 检索网关与运行时审计事件日志。 |
| **实时事件总线 (ActivityView - SSE)** | `/v1/apeireth/events` | `activity.sse` | **已实现** | 实时广播执行生命周期事件，脱敏处理零 CoT 泄露。 |
| **系统设置 (SettingsView)** | `/v1/models`, `/v1/providers/list` | `models.list`, `providers.list` | **已实现** | 模型提供商、人设偏好、网关地址与持久化管理。 |

---

## 3. 补全与降级细节

### 3.1 补全部分
1. **Safety Guard 观测与内省网关端口**：
   - 增加 `SafetyGuardQuery` bounded-context port，并在 `GatewayServices` 中注册 `safety_guard` 字段。
   - 提供 `/v1/safety/guard/status`、`/v1/safety/guard/events`、`/v1/safety/guard/evaluate` 真实端点。
   - `BehaviorChainGuardHook` 深度接入 CLI 运行时的治理管道 (`GovernancePipeline`)，全面监控工具调用与行为链，拦截违规并记录脱敏事件。
2. **工作台内省与执行链追踪 (Workbench)**：
   - 增加 `WorkbenchQuery` bounded-context port，在 `GatewayServices` 中注册 `workbench` 字段。
   - 提供 `/v1/workbench/turn` 端点，基于底层 SessionStore、TraceDetailDto 及 BehaviorChainGuardHook 聚合当前交互轮次的目标、工具执行时延与状态、记忆检索来源与安全研判结论。

### 3.2 诚实降级部分
1. **工作台 UI 诚实化**：
   - 原文案“子代理 (Subagents)”容易误导用户以为存在多智能体自主分派编排，现纠正为“代理与执行 (Agent & Execution)”。
   - 明确突出主 Agent 角色，工具调用标示为工具本身（如 `tool.repo`, `tool.filesystem`），杜绝把单步工具调用包装成假 Agent 的虚构行为。
2. **全双工语音服务**：
   - `/v1/apeireth/capabilities` 明确下发：
     ```json
     {
       "id": "voice.duplex",
       "supported": false,
       "available": false,
       "reason": "not_assembled",
       "operations": ["duplex", "stream"]
     }
     ```
   - 桌面端 VoiceCallModal 顶栏徽标标记为 `● 未组装 (not_assembled)`，中间呈现未组装提示横幅，不再进行虚假录音捕获或播放波形欺骗。
3. **活动日志文案脱敏**：
   - 抽屉与活动视图文案由“每一轮交互的延迟、Token、Prompt 与 CoT 思考流”更正为“每一轮交互的延迟、Token、执行事件与工具调用轨迹”，完全符合生产级零 CoT 与敏感信息防泄露规范。
