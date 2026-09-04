// Apeireth 桌面伙伴 — 核心共享类型定义 (Svelte 5 + Tauri 2)

export type ViewId = 'chat' | 'conversations' | 'activity' | 'tools' | 'memory' | 'settings';
export type Theme = 'night' | 'day' | 'ocean' | 'forest' | 'paper' | 'essence';
export type MemoryCategory =
  | '工作记忆'
  | '近期记忆'
  | '长期记忆'
  | '用户画像'
  | '知识'
  | '事实'
  | '偏好'
  | '事件'
  | '反馈'
  | '参考';

export type ConversationScope = 'global' | 'project';

export interface ToolCallDetails {
  id: string;
  name: string;
  args?: unknown;
  rawArgs?: string;
  status: 'pending' | 'running' | 'succeeded' | 'failed' | 'cancelled';
  resultSummary?: string;
  resultFull?: string;
  error?: string;
  durationMs?: number;
  startTime?: number;
  endTime?: number;
}

export interface ChatMessageEvent {
  id: string;
  kind: 'status' | 'tool' | 'task' | 'mcp' | 'memory' | 'agent' | 'error' | string;
  text: string;
  ts?: number;
  status?: 'pending' | 'running' | 'done' | 'failed' | 'skipped' | 'awaiting_approval' | string;
  action?: string;
  /** 工具风险等级 (T1-T3) */
  tier?: number;
  receipt?: string;
  taskId?: string;
  stepId?: string;
  toolCall?: ToolCallDetails;
}

export interface TaskCardInfo {
  taskId: string;
  title: string;
  status: string;
  detail?: string;
}

export interface ApprovalRequest {
  id: string;
  chain: string;
  rev: number;
  tool: string;
  args_preview: string;
  reason: string;
  status: 'pending' | 'approved' | 'expired';
  created_at: number;
  updated_at: number;
}

export interface ChatMessage {
  id: string;
  role: 'user' | 'assistant' | 'system';
  text: string;
  time: string;
  timestamp?: number;
  proactive?: string;
  events?: ChatMessageEvent[];
  error?: string;
  streaming?: boolean;
  aborted?: boolean;
  reasoning?: string;
  reasoningDurationMs?: number;
  provenance?: {
    count?: number;
    memories?: string[];
  };
  taskCard?: TaskCardInfo;
  toolCalls?: ToolCallDetails[];
  modelInfo?: {
    id: string;
    provider?: string;
  };
}

export interface Conversation {
  id: string;
  title: string;
  createdAt: number;
  updatedAt: number;
  messages: ChatMessage[];
  archived?: boolean;
  pinned?: boolean;
  scope: ConversationScope;
  projectId?: string;
  model?: string;
}

export interface ModelSetup {
  baseUrl: string;
  apiKey: string;
  model: string;
}

export interface SubsystemStatus {
  name: string;
  key: 'api' | 'companion' | 'memory' | 'tools' | 'events' | 'sessions';
  status: 'ok' | 'degraded' | 'offline' | 'unknown';
  endpoint: string;
  detail?: string;
  latencyMs?: number;
}

export interface RuntimeHealthReport {
  overall: 'connecting' | 'online' | 'degraded' | 'offline' | 'error';
  baseUrl: string;
  latencyMs?: number;
  lastChecked?: number;
  subsystems: SubsystemStatus[];
  model: string;
  error?: string;
}

export type HealthState = 'connecting' | 'online' | 'ready' | 'degraded' | 'generating' | 'error' | 'offline';

// ============================================================
// Runtime Capability Manifest — 后端能力发现契约
// Desktop 据此 gate UI 按钮, 不再 404-probing. 这是 information 不是 authorization.
// 未知字段保留 (forward compat); 未知 capability id 一律视为 unsupported.
// ============================================================

/** 单条能力声明 (稳定能力 ID 如 sessions.create). */
export interface Capability {
  id: string;
  supported: boolean;
  read?: boolean;
  write?: boolean;
  version?: number;
  operations?: string[];
  /**
   * 该能力**此时此刻**是否真正可调用 (动态, 受 provider/凭据/平台影响).
   * 向后兼容: 旧 manifest 无此字段 → 客户端按 available = supported 解释
   * (见 runtime.ts capabilityAvailable).
   */
  available?: boolean;
  /** 不可用原因 (machine-readable, 仅 available === false 时存在). */
  reason?: CapabilityAvailabilityReason;
  /** Explicit compatibility alias of another canonical capability id. */
  alias_of?: string;
}

/** Capability 不可用的 machine-readable 原因 (镜像 Rust AvailabilityReason). */
export type CapabilityAvailabilityReason =
  | 'provider_not_configured'
  | 'provider_unavailable'
  | 'platform_unsupported'
  | 'disabled_by_policy'
  | 'not_implemented'
  | 'not_exposed';

/** 一个能力组 (如 sessions / memory / permissions / trace). */
export interface CapabilityGroup {
  name: string;
  capabilities: Capability[];
}

/** Runtime 元信息 (仅 public 信息, 绝不含 secret/路径). */
export interface RuntimeInfo {
  service: string;
  version: string;
}

/** Capability Manifest — runtime 能力契约. */
export interface CapabilityManifest {
  schema_version: number;
  runtime: RuntimeInfo;
  capabilities: CapabilityGroup[];
  /** 是否为 legacy 兼容 profile (runtime 无原生 manifest 端点时客户端构造的保守声明). */
  legacy?: boolean;
}

export type ProviderProtocol = 'openai' | 'anthropic';

export interface ProviderConfig {
  protocol: ProviderProtocol;
  preset: string;
  baseUrl: string;
  apiKey: string;
  model: string;
  anthropicVersion?: string;
  /** Explicit diagnostics-only escape hatch; normal chat always uses Gateway. */
  debugDirect?: boolean;
}

/**
 * 伙伴人设 (Agent Persona) — 数据驱动身份, 可在设置里随时增删改, 无需重编译。
 * persona 文本作为 system 消息注入每次对话请求 (runtime.ts run() 统一注入)。
 */
export interface PersonaProfile {
  id: string;
  /** 显示名, 如 "阿佩瑞斯" */
  name: string;
  /** system 人设文本 (可为空 = 不注入) */
  persona: string;
  /** 可选: 该 Agent 固定使用的模型 (切换人设时同步切换模型) */
  model?: string;
}

export interface ApeirethConfig {
  baseUrl: string;
  apiKey: string;
  model: string;
  theme?: Theme;
  provider?: ProviderConfig;
  openaiConfig?: {
    preset?: string;
    baseUrl: string;
    apiKey: string;
    model: string;
  };
  anthropicConfig?: {
    preset?: string;
    baseUrl: string;
    apiKey: string;
    model: string;
    anthropicVersion?: string;
  };
  /** 多 Agent 人设列表 (持久化于本地配置, 不含 secret) */
  personas?: PersonaProfile[];
  /** 当前激活人设 id (缺省时取列表第一个) */
  activePersonaId?: string;
}

export interface ActivityItem {
  id: string;
  timestamp: number;
  category: 'conversation' | 'agent' | 'tool' | 'memory' | 'workflow' | 'runtime' | 'error';
  title: string;
  summary: string;
  source: 'sse' | 'audit' | 'runtime' | 'local';
  severity: 'info' | 'success' | 'warning' | 'error';
  detail?: string;
  raw?: unknown;
  /** Phase 5: 关联的 trace_id (SSE trace 事件携带). */
  traceId?: string;
}

export interface MemoryEpisodeItem {
  id: string;
  timestamp: number;
  role: string;
  content: string;
  sessionId: string;
  category?: string;
  stream?: string;
  importance?: number;
  tags?: string[];
  /** Phase 3 governance: 是否受保护 (防自动遗忘). */
  protected?: boolean;
  /** Phase 3 governance: active / forgotten. */
  status?: 'active' | 'forgotten';
}

export interface ToolItem {
  name: string;
  description?: string;
  argsSchema?: unknown;
  source?: 'builtin' | 'mcp' | 'extension';
  permission?: 'none' | 'prompt' | 'granted' | 'restricted';
  lastUsed?: number;
  available: boolean;
}

export interface ApprovalRequestItem {
  id: string;
  chain?: string;
  rev?: number;
  tool: string;
  reason?: string;
  args_preview?: string;
  summary?: string;
  requestedAt?: number;
  params?: unknown;
  status: 'pending' | 'approved' | 'expired' | 'rejected';
}


export function categoryToWire(category: MemoryCategory | string): string {
  const map: Record<string, string> = {
    工作记忆: 'working',
    近期记忆: 'recent',
    长期记忆: 'long_term',
    用户画像: 'profile',
    知识: 'knowledge',
    事实: 'fact',
    偏好: 'preference',
    事件: 'event',
    反馈: 'feedback',
    参考: 'reference',
  };
  return map[category] || category;
}

export function categoryFromWire(wire: string): MemoryCategory {
  const map: Record<string, MemoryCategory> = {
    working: '工作记忆',
    recent: '近期记忆',
    long_term: '长期记忆',
    profile: '用户画像',
    knowledge: '知识',
    fact: '长期记忆',
    preference: '用户画像',
    event: '近期记忆',
    feedback: '近期记忆',
    reference: '知识',
  };
  return map[wire] || '长期记忆';
}

export function importanceStars(value: number): 1 | 2 | 3 {
  if (value >= 0.75) return 3;
  if (value >= 0.4) return 2;
  return 1;
}

export interface GuardStatus {
  enabled: boolean;
  fast_guard_active: boolean;
  chain_guard_active: boolean;
  active_chains: number;
  total_evaluations: number;
  total_allowed: number;
  total_denied: number;
  total_approval_required: number;
  dataset_recording_enabled: boolean;
}

export interface GuardEvent {
  timestamp_ms: number;
  session_id: string;
  trace_id: string;
  round: number;
  capability_id: string;
  stage: 'fast_guard' | 'chain_guard';
  decision: string;
  risk_score: number;
  reasons: string[];
  evidence: string[];
}

export interface GuardDryRunRequest {
  session_id?: string;
  capability_id: string;
  arguments: unknown;
  declared_scope?: string;
}

export interface GuardDryRunResponse {
  decision: string;
  stage: 'fast_guard' | 'chain_guard';
  risk_score: number;
  reasons: string[];
  evidence: string[];
}

export interface WorkbenchToolExecution {
  id: string;
  name: string;
  status: string;
  latency_ms?: number;
  error?: string;
}

export interface WorkbenchMemoryProvenance {
  recalled_count: number;
  governance_filtered: number;
  layers: string[];
}

export interface WorkbenchTurn {
  session_id: string;
  goal: string;
  agent_status: string;
  tools: WorkbenchToolExecution[];
  memory: WorkbenchMemoryProvenance;
  guard_verdict?: string;
  updated_at: number;
}
