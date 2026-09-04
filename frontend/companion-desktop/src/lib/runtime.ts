// Apeireth 桌面伙伴 — Agent Runtime Contract & Adapter (Svelte 5 + Tauri 2)
//
// Reconciled integration baseline: capability-manifest-driven gating (core
// capability expansion) is the canonical contract; upstream master's companion
// presentation event stream is fused in. All V2 mutation endpoints and the
// capability discovery functions must not regress. Security invariant:
// apiKey / masterToken are NEVER persisted to localStorage.
//
// Conflict resolution (merge origin/master into feature): feature's richer
// signatures win for duplicated fetchers (fetchTools / fetchGraphData /
// fetchMemoryStreams / fetchEpisodes / fetchOrgans) because the capability-gated
// views depend on them; master's subscribeCompanionEvents + CompanionPresentationState
// + chatOnce + runtimeStatus are added as pure additions.

import type {
  ApeirethConfig,
  ChatMessage,
  Conversation,
  ModelSetup,
  RuntimeHealthReport,
  SubsystemStatus,
  ToolCallDetails,
  ActivityItem,
  MemoryEpisodeItem,
  ToolItem,
  ApprovalRequestItem,
  CapabilityManifest,
  Capability,
  GuardStatus,
  GuardEvent,
  GuardDryRunRequest,
  GuardDryRunResponse,
  WorkbenchTurn,
} from './types';
import {recordCallLog} from './call-logger.ts';

const STORAGE_KEY = 'apeireth-config';
const SECRET_CONFIG_KEYS = new Set([
  'apiKey',
  'api_key',
  'masterToken',
  'master_token',
  'accessToken',
  'access_token',
  'token',
  'secret',
]);

/** Remove credentials at every nesting level before a config touches storage. */
function purgeConfigSecrets(value: unknown): unknown {
  if (Array.isArray(value)) return value.map(purgeConfigSecrets);
  if (!value || typeof value !== 'object') return value;
  const out: Record<string, unknown> = {};
  for (const [key, child] of Object.entries(value as Record<string, unknown>)) {
    if (SECRET_CONFIG_KEYS.has(key)) continue;
    out[key] = purgeConfigSecrets(child);
  }
  return out;
}

function parseProviderConfig(value: unknown): ApeirethConfig['provider'] {
  if (!value || typeof value !== 'object') return undefined;
  const raw = value as Record<string, unknown>;
  const protocol = raw.protocol === 'anthropic' ? 'anthropic' : 'openai';
  const baseUrl = typeof raw.baseUrl === 'string' ? raw.baseUrl : '';
  const model = typeof raw.model === 'string' ? raw.model : '';
  return {
    protocol,
    preset: typeof raw.preset === 'string' ? raw.preset : 'custom',
    baseUrl,
    apiKey: '',
    model,
    anthropicVersion: typeof raw.anthropicVersion === 'string' ? raw.anthropicVersion : undefined,
    debugDirect: raw.debugDirect === true,
  };
}

function parseModelSetup(value: unknown): ModelSetup | undefined {
  if (!value || typeof value !== 'object') return undefined;
  const raw = value as Record<string, unknown>;
  return {
    baseUrl: typeof raw.baseUrl === 'string' ? raw.baseUrl : '',
    apiKey: '',
    model: typeof raw.model === 'string' ? raw.model : '',
  };
}

function persistedConfig(config: ApeirethConfig): Record<string, unknown> {
  return purgeConfigSecrets({
    baseUrl: config.baseUrl,
    model: config.model,
    theme: config.theme,
    provider: config.provider,
    openaiConfig: config.openaiConfig,
    anthropicConfig: config.anthropicConfig,
    personas: config.personas,
    activePersonaId: config.activePersonaId,
  }) as Record<string, unknown>;
}

function isDirectProviderDebugEnabled(provider: ApeirethConfig['provider']): boolean {
  return provider?.debugDirect === true;
}

/**
 * Canonical Apeireth 2.0 gateway address used when nothing is configured.
 * The gateway binds this port by default (`apeireth gateway serve --port 8080`);
 * in packaged Tauri mode the BackendSupervisor overrides it with the port it
 * actually allocated.
 */
const DEFAULT_BASE_URL = 'http://127.0.0.1:8080';

/**
 * Default model, matching the canonical minimax provider's own default
 * (`DEFAULT_MODELS[0]` in crates/engine/provider/src/canonical_minimax.rs).
 *
 * The provider accepts either the vendor spelling (`MiniMax-M3`) or the
 * canonical id (`minimax-m3`); the vendor spelling is used here to match the
 * backend constant exactly. The retired v1 default `MiniMax-Text-01` matches no
 * canonical provider and is migrated away in `loadConfig`.
 */
const DEFAULT_MODEL = 'MiniMax-M3';

/**
 * 默认伙伴人设 (与设置页"人设与声称约束"文案一致)。
 * 数据驱动: 用户可在设置里随时修改/新增 Agent, 无需重编译。
 */
export const DEFAULT_PERSONA_TEXT =
  '你是「阿佩瑞斯」——Apeireth 基地的主管。正在与你对话的这位是基地的最高指挥（主人）。' +
  '你的默认性别是女性；说话沉稳扎实，带古风韵味，自称「本座」。' +
  '称呼主人为「主人」或「指挥」，庄重而不失温度。';

export const DEFAULT_PERSONAS: import('./types').PersonaProfile[] = [
  {
    id: 'apeireth-default',
    name: '阿佩瑞斯',
    persona: DEFAULT_PERSONA_TEXT,
  },
];

/** 当前激活的人设 (缺省时取列表第一个; 无列表时回退默认人设). */
export function activePersonaOf(
  config: import('./types').ApeirethConfig,
): import('./types').PersonaProfile | null {
  const list = Array.isArray(config.personas) && config.personas.length > 0
    ? config.personas
    : DEFAULT_PERSONAS;
  if (config.activePersonaId) {
    const hit = list.find((p) => p.id === config.activePersonaId);
    if (hit) return hit;
  }
  return list[0] ?? null;
}

// ============================================================
// Runtime Contract Types
// ============================================================

export interface ModelReference {
  id: string;
  provider?: string;
  label?: string;
}

export interface AgentMessage {
  role: 'user' | 'assistant' | 'system';
  content: string;
  id?: string;
  timestamp?: number;
}

export interface AgentRunRequest {
  messages: AgentMessage[];
  model: ModelReference;
  sessionId?: string;
  context?: {
    persona?: string;
    user?: string;
  };
  signal?: AbortSignal;
}

export type RuntimeEvent =
  | {type: 'run-start'; requestId: string}
  | {type: 'message-start'; requestId: string; messageId: string}
  | {type: 'text-delta'; requestId: string; text: string}
  | {type: 'reasoning-delta'; requestId: string; text: string}
  | {type: 'tool-call'; requestId: string; toolCall: ToolCallDetails}
  | {type: 'tool-result'; requestId: string; toolCallId: string; ok: boolean; summary?: string; full?: string; error?: string}
  | {type: 'approval-required'; requestId: string; pending: CanonicalPendingApproval}
  | {type: 'message-end'; requestId: string; messageId: string; fullText: string}
  | {type: 'run-error'; requestId: string; error: RuntimeError}
  | {type: 'run-end'; requestId: string; aborted: boolean};

export interface RuntimeError {
  code:
    | 'http'
    | 'network'
    | 'auth'
    | 'timeout'
    | 'aborted'
    | 'unknown'
    | 'approval_required'
    | 'denied'
    | 'provider'
    | 'backend';
  message: string;
  status?: number;
}

export interface CanonicalPendingApproval {
  session: string;
  approval_id: string;
  request: string;
  trace_id: string;
  capability_id: string;
  tool_name: string;
  governance_hook: string;
  governance_reason: string;
  created_at: string;
  expires_at: string;
}

export interface CanonicalExecutionEvent {
  event: 'tool_started' | 'tool_completed' | 'tool_failed' | 'approval_required' | string;
  tool_name?: string;
  capability_id?: string;
  tool_call_id?: string;
  succeeded?: boolean;
  approval_id?: string;
  round?: number;
}

export class ApprovalRequiredError extends Error {
  pending: CanonicalPendingApproval;
  constructor(pending: CanonicalPendingApproval) {
    super(`需要批准: ${pending.tool_name} — ${pending.governance_reason}`);
    this.name = 'ApprovalRequiredError';
    this.pending = pending;
  }
}

export interface AgentRuntime {
  run(request: AgentRunRequest, onEvent: (event: RuntimeEvent) => void): Promise<string>;
  abort(): void;
  readonly running: boolean;
  health(): Promise<RuntimeHealthReport>;
}

export interface RuntimeStatus {
  connected: boolean;
  baseUrl: string;
  model?: string;
}

export function classifyHttpError(status: number): RuntimeError['code'] {
  if (status === 401 || status === 403) return 'denied';
  if (status === 409) return 'approval_required';
  if (status === 502) return 'provider';
  if (status === 503) return 'backend';
  if (status === 404) return 'http';
  if (status >= 500) return 'backend';
  return 'http';
}

export class HttpError extends Error {
  status: number;
  constructor(status: number, message: string) {
    super(message);
    this.name = 'HttpError';
    this.status = status;
  }
}

/**
 * Render any caught value as human-readable text.
 *
 * The one hard rule: never return `"[object Object]"`. `String(value)` produces
 * exactly that for a plain object, so an object is only stringified through a
 * field known to carry text — otherwise it is JSON-serialised, and failing that
 * described by shape.
 */
export function describeCaught(caught: unknown): string {
  if (caught instanceof Error) return caught.message;
  if (typeof caught === 'string') return caught;
  if (caught === null) return '未知错误 (null)';
  if (caught === undefined) return '未知错误 (undefined)';

  if (typeof caught === 'object') {
    // Common text-bearing shapes, including backend error bodies ({error: "..."}).
    for (const key of ['message', 'error', 'detail', 'description'] as const) {
      const value = (caught as Record<string, unknown>)[key];
      if (typeof value === 'string' && value.trim()) return value;
      // A nested Error or {error:{message}} body.
      if (value instanceof Error && value.message) return value.message;
      if (value && typeof value === 'object') {
        const nested = (value as Record<string, unknown>).message;
        if (typeof nested === 'string' && nested.trim()) return nested;
      }
    }
    // No text field: serialise rather than let String() yield [object Object].
    try {
      const json = JSON.stringify(caught);
      if (json && json !== '{}') return json;
    } catch {
      // Circular or non-serialisable; fall through to the shape description.
    }
    const name = (caught as object).constructor?.name;
    return `未知错误对象${name && name !== 'Object' ? ` (${name})` : ''}`;
  }

  return String(caught);
}

/**
 * Detect unsupported endpoint errors (404/501/503 on known legacy routes) and
 * return a user-friendly message explaining the canonical 2.0 architecture.
 * For other errors, pass through the original message.
 */
export function friendlyErrorMessage(caught: unknown, endpoint?: string): string {
  if (caught instanceof HttpError && endpoint) {
    const isLegacyEndpoint =
      endpoint.includes('/v1/panel/') ||
      endpoint.includes('/v1/apeireth/') ||
      endpoint.includes('/v1/tools/list') ||
      endpoint.includes('/v1/memory/append') ||
      endpoint.includes('/v1/organs');

    if (isLegacyEndpoint && (caught.status === 404 || caught.status === 501 || caught.status === 503)) {
      return '当前运行时不支持此内省功能 (Apeireth 2.0 canonical gateway 无 panel/introspection API)';
    }
  }

  return describeCaught(caught);
}

export function toRuntimeError(caught: unknown): RuntimeError {
  if (caught instanceof DOMException && caught.name === 'AbortError') {
    return {code: 'aborted', message: '已中止请求'};
  }
  if (caught instanceof ApprovalRequiredError) {
    return {code: 'approval_required', message: caught.message, status: 202};
  }
  if (caught instanceof TypeError) {
    return {code: 'network', message: '网络错误：后端不可达或跨域拒绝'};
  }
  if (caught instanceof HttpError) {
    return {
      code: classifyHttpError(caught.status),
      message: caught.message,
      status: caught.status,
    };
  }
  // One shared guarantee: describeCaught never yields "[object Object]".
  return {code: 'unknown', message: describeCaught(caught)};
}

export function loadConfig(): ApeirethConfig {
  try {
    const raw = localStorage.getItem(STORAGE_KEY);
    if (raw) {
      const rawParsed = JSON.parse(raw) as Record<string, unknown>;
      const parsed = purgeConfigSecrets(rawParsed) as Record<string, unknown>;
      // Security migration: purge credentials from every nested provider or
      // legacy object, not only from the top-level config.
      let modified = JSON.stringify(parsed) !== JSON.stringify(rawParsed);
      let baseUrl = typeof parsed.baseUrl === 'string' ? parsed.baseUrl : DEFAULT_BASE_URL;
      // Migrate legacy default placeholder (:3000) to canonical Apeireth Gateway (:8080)
      if (baseUrl === 'http://127.0.0.1:3000') {
        baseUrl = DEFAULT_BASE_URL;
        modified = true;
      }
      let model = typeof parsed.model === 'string' ? parsed.model : DEFAULT_MODEL;
      // Migrate the retired v1 default. No canonical provider matches
      // 'MiniMax-Text-01', so a config carrying it would fail every turn.
      if (model === 'MiniMax-Text-01') {
        model = DEFAULT_MODEL;
        modified = true;
      }
      const provider = parseProviderConfig(parsed.provider);
      const openaiConfig = parseModelSetup(parsed.openaiConfig);
      const anthropicConfig = parseModelSetup(parsed.anthropicConfig);
      let personas: ApeirethConfig['personas'];
      const rawPersonas = parsed.personas;
      if (Array.isArray(rawPersonas)) {
        personas = rawPersonas.filter(
          (p): p is import('./types').PersonaProfile =>
            !!p &&
            typeof p === 'object' &&
            typeof (p as {id?: unknown}).id === 'string' &&
            typeof (p as {name?: unknown}).name === 'string' &&
            typeof (p as {persona?: unknown}).persona === 'string',
        );
      }
      let activePersonaId =
        typeof parsed.activePersonaId === 'string' ? parsed.activePersonaId : undefined;

      const cleaned: ApeirethConfig = {
        baseUrl,
        apiKey: '', // transient in-memory gateway key only; not persisted
        model,
        theme: typeof parsed.theme === 'string' ? (parsed.theme as any) : undefined,
        provider,
        openaiConfig,
        anthropicConfig,
        personas,
        activePersonaId,
      };
      if (modified) {
        localStorage.setItem(STORAGE_KEY, JSON.stringify(persistedConfig(cleaned)));
      }
      return cleaned;
    }
  } catch {
    // ignore corrupted config
  }
  return {
    baseUrl: DEFAULT_BASE_URL,
    apiKey: '',
    model: DEFAULT_MODEL,
    provider: {
      protocol: 'openai',
      preset: 'openai',
      baseUrl: 'https://api.openai.com/v1',
      apiKey: '',
      model: 'gpt-4o',
    },
    openaiConfig: {
      preset: 'openai',
      baseUrl: 'https://api.openai.com/v1',
      apiKey: '',
      model: 'gpt-4o',
    },
    anthropicConfig: {
      preset: 'anthropic',
      baseUrl: 'https://api.anthropic.com',
      apiKey: '',
      model: 'claude-3-7-sonnet-20250219',
      anthropicVersion: '2023-06-01',
    },
    personas: DEFAULT_PERSONAS,
    activePersonaId: 'apeireth-default',
  };
}

export function saveConfig(config: ApeirethConfig): void {
  localStorage.setItem(STORAGE_KEY, JSON.stringify(persistedConfig(config)));
}

/** 测试模型提供商（OpenAI 或 Anthropic 协议）连通性与模型列表获取 */
export async function testProviderConnection(provider: NonNullable<ApeirethConfig['provider']>): Promise<{
  ok: boolean;
  latencyMs: number;
  models?: string[];
  message: string;
}> {
  const start = performance.now();
  const base = normalizeBaseUrl(provider.baseUrl);
  const apiKey = provider.apiKey?.trim() || '';

  try {
    if (provider.protocol === 'openai') {
      const headers: Record<string, string> = {};
      if (apiKey) headers['Authorization'] = `Bearer ${apiKey}`;
      const url = base.endsWith('/v1') ? `${base}/models` : `${base}/v1/models`;
      const res = await fetch(url, {
        headers,
        signal: AbortSignal.timeout(5000),
      });
      const latencyMs = Math.round(performance.now() - start);

      if (res.ok) {
        const data = (await res.json().catch(() => ({}))) as {data?: Array<{id?: string}>};
        const models = Array.isArray(data.data)
          ? (data.data.map((m) => m.id).filter(Boolean) as string[])
          : [];
        return {
          ok: true,
          latencyMs,
          models,
          message: models.length ? `连接成功！发现 ${models.length} 个可用模型` : '连接成功 (HTTP 200 OK)',
        };
      } else {
        const errText = await res.text().catch(() => '');
        return {
          ok: false,
          latencyMs,
          message: `请求返回 HTTP ${res.status}: ${errText.slice(0, 150) || res.statusText}`,
        };
      }
    } else {
      // Anthropic 协议
      const headers: Record<string, string> = {
        'x-api-key': apiKey,
        'anthropic-version': provider.anthropicVersion || '2023-06-01',
      };
      const url = base.endsWith('/v1') ? `${base}/models` : `${base}/v1/models`;
      const res = await fetch(url, {
        headers,
        signal: AbortSignal.timeout(5000),
      });
      const latencyMs = Math.round(performance.now() - start);

      if (res.ok) {
        const data = (await res.json().catch(() => ({}))) as {data?: Array<{id?: string}>};
        const models = Array.isArray(data.data)
          ? (data.data.map((m) => m.id).filter(Boolean) as string[])
          : [];
        return {
          ok: true,
          latencyMs,
          models,
          message: models.length ? `Anthropic 连接成功！发现 ${models.length} 个模型` : 'Anthropic 鉴权成功 (HTTP 200 OK)',
        };
      } else {
        const errText = await res.text().catch(() => '');
        return {
          ok: false,
          latencyMs,
          message: `Anthropic 服务返回 HTTP ${res.status}: ${errText.slice(0, 150) || res.statusText}`,
        };
      }
    }
  } catch (err) {
    const latencyMs = Math.round(performance.now() - start);
    return {
      ok: false,
      latencyMs,
      message: `连接失败: ${err instanceof Error ? err.message : String(err)}`,
    };
  }
}



function normalizeBaseUrl(baseUrl: string): string {
  return baseUrl.replace(/\/+$/, '');
}

async function checkJson(response: Response): Promise<unknown> {
  if (!response.ok) {
    const text = await response.text().catch(() => '');
    throw new HttpError(response.status, `HTTP ${response.status} ${text.slice(0, 300)}`);
  }
  return response.json();
}

/** 基础 /health 端点探测 */
export async function checkHealth(baseUrl: string): Promise<boolean> {
  try {
    const response = await fetch(`${normalizeBaseUrl(baseUrl)}/health`, {signal: AbortSignal.timeout(2500)});
    return response.ok;
  } catch {
    return false;
  }
}

/** 深度健康检测，探测 canonical Apeireth 2.0 gateway 及配置的模型提供商 */
export async function checkHealthDetailed(
  baseUrl: string,
  apiKey: string = '',
  model?: string,
  provider?: ApeirethConfig['provider'],
): Promise<RuntimeHealthReport> {
  const base = normalizeBaseUrl(baseUrl);
  const subsystems: SubsystemStatus[] = [];
  const startAll = performance.now();
  let gatewayOk = false;
  let providerOk = false;

  // 1. Gateway Health (canonical)
  const t0 = performance.now();
  try {
    const res = await fetch(`${base}/health`, {signal: AbortSignal.timeout(2500)});
    const lat = Math.round(performance.now() - t0);
    if (res.ok) {
      gatewayOk = true;
      subsystems.push({name: 'Gateway', key: 'api', status: 'ok', endpoint: '/health', latencyMs: lat, detail: 'HTTP 200 OK'});
    } else {
      subsystems.push({name: 'Gateway', key: 'api', status: 'degraded', endpoint: '/health', latencyMs: lat, detail: `HTTP ${res.status}`});
    }
  } catch (e) {
    subsystems.push({name: 'Gateway', key: 'api', status: 'offline', endpoint: '/health', detail: '连接超时或服务未启动'});
  }

  // 2. Custom Provider or Gateway Models
  const isCustomProvider = !!(
    provider &&
    (provider.apiKey?.trim() ||
      (provider.baseUrl &&
        !provider.baseUrl.includes('127.0.0.1:8080') &&
        !provider.baseUrl.includes('localhost:8080')))
  );

  if (isCustomProvider && provider) {
    const provTest = await testProviderConnection(provider);
    if (provTest.ok) {
      providerOk = true;
      subsystems.push({
        name: `提供商 (${provider.preset === 'custom' ? '自定义' : provider.preset || provider.protocol})`,
        key: 'companion',
        status: 'ok',
        endpoint: provider.baseUrl,
        latencyMs: provTest.latencyMs,
        detail: provTest.message,
      });
    } else {
      subsystems.push({
        name: `提供商 (${provider.preset === 'custom' ? '自定义' : provider.preset || provider.protocol})`,
        key: 'companion',
        status: 'degraded',
        endpoint: provider.baseUrl,
        latencyMs: provTest.latencyMs,
        detail: provTest.message,
      });
    }
  } else {
    // Gateway fallback model probe
    const t1 = performance.now();
    try {
      const res = await fetch(`${base}/v1/models`, {
        signal: AbortSignal.timeout(3000),
      });
      const lat = Math.round(performance.now() - t1);
      if (res.ok) {
        providerOk = true;
        const data = (await res.json().catch(() => ({}))) as {data?: unknown[]};
        const count = Array.isArray(data.data) ? data.data.length : 0;
        subsystems.push({name: '模型/提供商', key: 'companion', status: 'ok', endpoint: '/v1/models', latencyMs: lat, detail: `可用模型: ${count}`});
      } else {
        subsystems.push({name: '模型/提供商', key: 'companion', status: 'degraded', endpoint: '/v1/models', latencyMs: lat, detail: `HTTP ${res.status}`});
      }
    } catch {
      subsystems.push({name: '模型/提供商', key: 'companion', status: 'offline', endpoint: '/v1/models', detail: '模型列表不可用 (提供商未配置或离线)'});
    }
  }

  const overallLat = Math.round(performance.now() - startAll);
  let overall: RuntimeHealthReport['overall'] = 'offline';
  if (gatewayOk || providerOk) {
    overall = (gatewayOk && providerOk) ? 'online' : 'online';
  } else {
    overall = 'offline';
  }

  return {
    overall,
    baseUrl: base,
    latencyMs: overallLat,
    lastChecked: Date.now(),
    subsystems,
    model: model || loadConfig().model,
  };
}

export async function listModels(baseUrl: string, apiKey: string): Promise<string[]> {
  // NOTE: Provider credentials loaded by backend from env vars, not frontend Authorization header.
  // apiKey parameter kept for signature compatibility but not used for canonical endpoint.
  const response = await fetch(`${normalizeBaseUrl(baseUrl)}/v1/models`);
  const data = (await checkJson(response)) as {data?: Array<{id: string}>};
  return (data.data || []).map((item) => item.id);
}

/**
 * 流式聊天: 通过 SSE 请求 OpenAI 兼容 chat completion 端点.
 * 覆盖：text delta, tool calls, reasoning delta, malformed lines, interruptions.
 * Reconciled: feature's structured ToolCallDetails callback + sessionId header
 * (canonical, verified) retained; reasoning_content delta handling retained.
 */
export interface StreamCallbacks {
  onDelta?: (text: string) => void;
  onReasoningDelta?: (text: string) => void;
  onToolCall?: (toolCall: ToolCallDetails) => void;
  onToolResult?: (id: string, ok: boolean, summary?: string) => void;
  onApprovalRequired?: (pending: CanonicalPendingApproval) => void;
}

function applyCanonicalEvents(
  events: CanonicalExecutionEvent[] | undefined,
  callbacks: StreamCallbacks,
): void {
  if (!events) return;
  for (const event of events) {
    const id = event.tool_call_id || event.approval_id || event.capability_id || event.event;
    const name = event.tool_name || event.capability_id || 'tool';
    if (event.event === 'tool_started' || event.event === 'approval_required') {
      callbacks.onToolCall?.({
        id,
        name,
        status: event.event === 'approval_required' ? 'pending' : 'running',
        startTime: Date.now(),
      });
    }
    if (event.event === 'tool_completed' || event.event === 'tool_failed') {
      callbacks.onToolResult?.(id, event.event === 'tool_completed', event.event);
    }
    if (event.event === 'approval_required' && event.approval_id) {
      // The 202 body carries the full pending record; events only hint.
    }
  }
}

function joinOpenAiChatUrl(baseUrl: string): string {
  const base = normalizeBaseUrl(baseUrl);
  if (base.endsWith('/chat/completions')) return base;
  if (base.endsWith('/v1')) return `${base}/chat/completions`;
  return `${base}/v1/chat/completions`;
}

function joinAnthropicMessagesUrl(baseUrl: string): string {
  const base = normalizeBaseUrl(baseUrl);
  if (base.endsWith('/messages')) return base;
  if (base.endsWith('/v1')) return `${base}/messages`;
  return `${base}/v1/messages`;
}

export async function streamChat(
  config: ApeirethConfig,
  messages: Array<{role: 'user' | 'assistant' | 'system'; content: string}>,
  callbacks: StreamCallbacks,
  signal?: AbortSignal,
  sessionId?: string,
): Promise<string> {
  const provider = config.provider;
  const isDirectCustom = !!(
    isDirectProviderDebugEnabled(provider) &&
    provider &&
    (provider.apiKey?.trim() ||
      (provider.baseUrl &&
        !provider.baseUrl.includes('127.0.0.1:8080') &&
        !provider.baseUrl.includes('localhost:8080')))
  );

  const startMs = performance.now();
  let fullText = '';
  let fullReasoning = '';
  let activeProtocol: 'openai' | 'anthropic' | 'gateway' = isDirectCustom
    ? provider.protocol
    : 'gateway';
  let activeEndpoint = isDirectCustom
    ? provider.protocol === 'anthropic'
      ? joinAnthropicMessagesUrl(provider.baseUrl)
      : joinOpenAiChatUrl(provider.baseUrl)
    : `${normalizeBaseUrl(config.baseUrl)}/v1/chat/completions`;
  let activeModel = (isDirectCustom && provider.model) ? provider.model : config.model;

  // W6 CoT 增量分流 (契约 §2.3): 把 CoT 嵌在 delta.content 的
  // <think>…</think> / <!-- … --> 标记里, 边界标记可能跨 chunk 切分.
  const COT_OPEN: Array<readonly [string, 'think' | 'comment']> = [
    ['<think>', 'think'],
    ['<!--', 'comment'],
  ];
  const COT_CLOSE: Record<'think' | 'comment', string> = {think: '</think>', comment: '-->'};
  let cotMode: 'text' | 'think' | 'comment' = 'text';
  let cotHold = '';
  const emitVisible = (text: string): void => {
    if (!text) return;
    fullText += text;
    callbacks.onDelta?.(text);
  };
  const emitReasoning = (text: string): void => {
    if (!text) return;
    fullReasoning += text;
    callbacks.onReasoningDelta?.(text);
  };
  const holdTail = (s: string, marker: string): number => {
    const max = Math.min(marker.length - 1, s.length);
    for (let len = max; len > 0; len--) {
      if (marker.startsWith(s.slice(s.length - len))) return len;
    }
    return 0;
  };
  const holdTailAny = (s: string): number => {
    let best = 0;
    for (const [marker] of COT_OPEN) best = Math.max(best, holdTail(s, marker));
    return best;
  };
  function feedCot(raw: string, flush = false): void {
    let s = cotHold + raw;
    cotHold = '';
    while (s) {
      if (cotMode === 'text') {
        let idx = -1;
        let mode: 'think' | 'comment' = 'think';
        let openLen = 0;
        for (const [marker, m] of COT_OPEN) {
          const i = s.indexOf(marker);
          if (i >= 0 && (idx < 0 || i < idx)) {
            idx = i;
            mode = m;
            openLen = marker.length;
          }
        }
        if (idx < 0) {
          let emit = s;
          if (!flush) {
            const hold = holdTailAny(s);
            if (hold > 0) {
              cotHold = s.slice(s.length - hold);
              emit = s.slice(0, s.length - hold);
            }
          }
          emitVisible(emit);
          return;
        }
        emitVisible(s.slice(0, idx));
        cotMode = mode;
        s = s.slice(idx + openLen);
      } else {
        const close = COT_CLOSE[cotMode];
        const i = s.indexOf(close);
        if (i < 0) {
          let emit = s;
          if (!flush) {
            const hold = holdTail(s, close);
            if (hold > 0) {
              cotHold = s.slice(s.length - hold);
              emit = s.slice(0, s.length - hold);
            }
          }
          emitReasoning(emit);
          return;
        }
        emitReasoning(s.slice(0, i));
        cotMode = 'text';
        s = s.slice(i + close.length);
      }
    }
  }

  try {
    // 1. Direct Anthropic Custom Provider
    if (isDirectCustom && provider.protocol === 'anthropic') {
      const url = activeEndpoint;
      const apiKey = provider.apiKey?.trim() || '';
      const headers: Record<string, string> = {
        'Content-Type': 'application/json',
        'anthropic-version': provider.anthropicVersion || '2023-06-01',
        'anthropic-dangerous-direct-browser-access': 'true',
      };
      if (apiKey) headers['x-api-key'] = apiKey;

      const systemParts = messages
        .filter((m) => m.role === 'system')
        .map((m) => m.content)
        .join('\n');
      const anthropicMessages = messages
        .filter((m) => m.role === 'user' || m.role === 'assistant')
        .map((m) => ({role: m.role, content: m.content}));

      const response = await fetch(url, {
        method: 'POST',
        headers,
        body: JSON.stringify({
          model: activeModel,
          max_tokens: 4096,
          messages: anthropicMessages,
          system: systemParts || undefined,
          stream: true,
        }),
        signal,
      });

      if (!response.ok) {
        const text = await response.text().catch(() => '');
        let detail = text.slice(0, 300);
        try {
          const parsed = JSON.parse(text);
          if (parsed?.error?.message) detail = parsed.error.message;
        } catch {}
        throw new HttpError(response.status, `Anthropic HTTP ${response.status}: ${detail}`);
      }
      if (!response.body) throw new Error('Anthropic 响应流为空');

      const reader = response.body.getReader();
      const decoder = new TextDecoder();
      let buffer = '';

      try {
        while (true) {
          const {done, value} = await reader.read();
          if (done) break;
          buffer += decoder.decode(value, {stream: true});
          const lines = buffer.split('\n');
          buffer = lines.pop() || '';

          for (const line of lines) {
            const trimmed = line.trim();
            if (!trimmed || !trimmed.startsWith('data:')) continue;
            const payload = trimmed.slice(5).trim();
            if (!payload) continue;

            try {
              const json = JSON.parse(payload) as {
                type?: string;
                delta?: {
                  type?: string;
                  text?: string;
                };
              };

              if (json.type === 'content_block_delta' && json.delta?.text) {
                feedCot(json.delta.text);
              }
            } catch {}
          }
        }
      } finally {
        reader.releaseLock();
      }

      recordCallLog({
        conversationId: sessionId,
        protocol: 'anthropic',
        endpoint: url,
        model: activeModel,
        status: 'success',
        latencyMs: Math.round(performance.now() - startMs),
        requestMessages: messages,
        systemPrompt: systemParts || undefined,
        responseContent: fullText,
        reasoningContent: fullReasoning || undefined,
      });
      return fullText;
    }

    // 2. Direct OpenAI Custom Provider
    if (isDirectCustom && provider.protocol === 'openai') {
      const url = activeEndpoint;
      const apiKey = provider.apiKey?.trim() || '';
      const headers: Record<string, string> = {
        'Content-Type': 'application/json',
      };
      if (apiKey) headers['Authorization'] = `Bearer ${apiKey}`;

      const response = await fetch(url, {
        method: 'POST',
        headers,
        body: JSON.stringify({
          model: activeModel,
          messages,
          stream: true,
        }),
        signal,
      });

      if (!response.ok) {
        const text = await response.text().catch(() => '');
        let detail = text.slice(0, 300);
        try {
          const parsed = JSON.parse(text);
          if (parsed?.error?.message) detail = parsed.error.message;
          else if (typeof parsed?.error === 'string') detail = parsed.error;
        } catch {}
        throw new HttpError(response.status, `OpenAI HTTP ${response.status}: ${detail}`);
      }
      if (!response.body) throw new Error('OpenAI 响应流为空');

      const reader = response.body.getReader();
      const decoder = new TextDecoder();
      let buffer = '';

      try {
        while (true) {
          const {done, value} = await reader.read();
          if (done) break;
          buffer += decoder.decode(value, {stream: true});
          const lines = buffer.split('\n');
          buffer = lines.pop() || '';

          for (const line of lines) {
            const trimmed = line.trim();
            if (!trimmed || !trimmed.startsWith('data:')) continue;
            const payload = trimmed.slice(5).trim();
            if (payload === '[DONE]') break;

            try {
              const json = JSON.parse(payload) as {
                choices?: Array<{
                  delta?: {
                    content?: string;
                    reasoning_content?: string;
                  };
                }>;
              };

              const choice = json.choices?.[0];
              const delta = choice?.delta;

              if (delta?.content) {
                feedCot(delta.content);
              }
              if (delta?.reasoning_content) {
                emitReasoning(delta.reasoning_content);
              }
            } catch {}
          }
        }
      } finally {
        reader.releaseLock();
      }

      recordCallLog({
        conversationId: sessionId,
        protocol: 'openai',
        endpoint: url,
        model: activeModel,
        status: 'success',
        latencyMs: Math.round(performance.now() - startMs),
        requestMessages: messages,
        responseContent: fullText,
        reasoningContent: fullReasoning || undefined,
      });
      return fullText;
    }

    // 3. Apeireth Gateway Local Daemon Fallback
    const base = normalizeBaseUrl(config.baseUrl);
    const headers: Record<string, string> = {
      'Content-Type': 'application/json',
    };

    const response = await fetch(`${base}/v1/chat/completions`, {
      method: 'POST',
      headers,
      body: JSON.stringify({
        model: config.model,
        messages,
        session_id: sessionId,
        stream: true,
      }),
      signal,
    });

    if (response.status === 202) {
      const pending = (await response.json()) as CanonicalPendingApproval;
      callbacks.onApprovalRequired?.(pending);
      throw new ApprovalRequiredError(pending);
    }
    if (!response.ok) {
      const text = await response.text().catch(() => '');
      let detail = text.slice(0, 300);
      try {
        const parsed = JSON.parse(text);
        if (parsed && typeof parsed.error === 'string') {
          detail = parsed.error;
        }
      } catch {}
      throw new HttpError(response.status, `HTTP ${response.status}: ${detail}`);
    }
    if (!response.body) throw new Error('响应流为空');

    const reader = response.body.getReader();
    const decoder = new TextDecoder();
    let buffer = '';

    try {
      while (true) {
        const {done, value} = await reader.read();
        if (done) break;
        buffer += decoder.decode(value, {stream: true});
        const lines = buffer.split('\n');
        buffer = lines.pop() || '';

        for (const line of lines) {
          const trimmed = line.trim();
          if (!trimmed || !trimmed.startsWith('data:')) continue;
          const payload = trimmed.slice(5).trim();
          if (payload === '[DONE]') break;

          try {
            const json = JSON.parse(payload) as {
              choices?: Array<{
                delta?: {
                  content?: string;
                  reasoning_content?: string;
                };
                finish_reason?: string;
              }>;
              apeireth?: {events?: CanonicalExecutionEvent[]};
            };

            const choice = json.choices?.[0];
            const delta = choice?.delta;

            if (delta?.content) {
              feedCot(delta.content);
            }
            if (delta?.reasoning_content) {
              emitReasoning(delta.reasoning_content);
            }

            applyCanonicalEvents(json.apeireth?.events, callbacks);
          } catch {}
        }
      }
    } finally {
      reader.releaseLock();
    }

    recordCallLog({
      conversationId: sessionId,
      protocol: 'gateway',
      endpoint: `${base}/v1/chat/completions`,
      model: config.model,
      status: 'success',
      latencyMs: Math.round(performance.now() - startMs),
      requestMessages: messages,
      responseContent: fullText,
      reasoningContent: fullReasoning || undefined,
    });
    return fullText;
  } catch (err) {
    const isAbort =
      (err instanceof Error && err.name === 'AbortError') ||
      (typeof err === 'object' && err !== null && (err as any).code === 'aborted');
    recordCallLog({
      conversationId: sessionId,
      protocol: activeProtocol,
      endpoint: activeEndpoint,
      model: activeModel,
      status: isAbort ? 'aborted' : 'error',
      latencyMs: Math.round(performance.now() - startMs),
      requestMessages: messages,
      responseContent: fullText || undefined,
      reasoningContent: fullReasoning || undefined,
      errorMessage: err instanceof Error ? err.message : String(err),
      httpStatus: err instanceof HttpError ? err.status : undefined,
    });
    throw err;
  }
}

/** 非流式聊天 (用于简单问答/健康检查). Reconciled from master. */
export async function chatOnce(config: ApeirethConfig, prompt: string): Promise<string> {
  const provider = config.provider;
  const isDirectCustom = !!(
    isDirectProviderDebugEnabled(provider) &&
    provider &&
    (provider.apiKey?.trim() ||
      (provider.baseUrl &&
        !provider.baseUrl.includes('127.0.0.1:8080') &&
        !provider.baseUrl.includes('localhost:8080')))
  );

  if (isDirectCustom && provider.protocol === 'anthropic') {
    const url = joinAnthropicMessagesUrl(provider.baseUrl);
    const headers: Record<string, string> = {
      'Content-Type': 'application/json',
      'anthropic-version': provider.anthropicVersion || '2023-06-01',
      'anthropic-dangerous-direct-browser-access': 'true',
    };
    if (provider.apiKey) headers['x-api-key'] = provider.apiKey.trim();
    const response = await fetch(url, {
      method: 'POST',
      headers,
      body: JSON.stringify({
        model: provider.model || config.model,
        max_tokens: 4096,
        messages: [{role: 'user', content: prompt}],
      }),
    });
    const data = (await checkJson(response)) as {
      content?: Array<{type?: string; text?: string}>;
    };
    return data.content?.find((c) => c.type === 'text')?.text || '';
  }

  if (isDirectCustom && provider.protocol === 'openai') {
    const url = joinOpenAiChatUrl(provider.baseUrl);
    const headers: Record<string, string> = {
      'Content-Type': 'application/json',
    };
    if (provider.apiKey) headers['Authorization'] = `Bearer ${provider.apiKey.trim()}`;
    const response = await fetch(url, {
      method: 'POST',
      headers,
      body: JSON.stringify({
        model: provider.model || config.model,
        messages: [{role: 'user', content: prompt}],
        stream: false,
      }),
    });
    const data = (await checkJson(response)) as {
      choices?: Array<{message?: {content?: string}}>;
    };
    return data.choices?.[0]?.message?.content || '';
  }

  const response = await fetch(`${normalizeBaseUrl(config.baseUrl)}/v1/chat/completions`, {
    method: 'POST',
    headers: {
      'Content-Type': 'application/json',
    },
    body: JSON.stringify({
      model: config.model,
      messages: [{role: 'user', content: prompt}],
      stream: false,
    }),
  });
  const data = (await checkJson(response)) as {
    choices?: Array<{message?: {content?: string}}>;
  };
  return data.choices?.[0]?.message?.content || '';
}

export function runtimeStatus(baseUrl: string, model?: string): RuntimeStatus {
  return {connected: false, baseUrl, model};
}

export function createAgentRuntime(config: ApeirethConfig): AgentRuntime {
  let abortController: AbortController | null = null;
  let _running = false;

  const runtime: AgentRuntime = {
    get running() {
      return _running;
    },

    async run(request, onEvent) {
      _running = true;
      abortController = new AbortController();
      const requestId = crypto.randomUUID();

      try {
        onEvent({type: 'run-start', requestId});
        onEvent({type: 'message-start', requestId, messageId: requestId});

        // 数据驱动人设注入: 激活 Agent 的人设作为 system 消息前置 (空人设/已有 system 则跳过).
        const persona = activePersonaOf(config);
        const wireMessages = request.messages.map((m) => ({role: m.role, content: m.content}));
        const hasSystem = wireMessages.some((m) => m.role === 'system');
        const personaText = persona?.persona?.trim() || '';
        const effectiveMessages =
          !hasSystem && personaText
            ? [{role: 'system' as const, content: personaText}, ...wireMessages]
            : wireMessages;

        const full = await streamChat(
          config,
          effectiveMessages,
          {
            onDelta: (delta) => onEvent({type: 'text-delta', requestId, text: delta}),
            onReasoningDelta: (delta) => onEvent({type: 'reasoning-delta', requestId, text: delta}),
            onToolCall: (toolCall) => onEvent({type: 'tool-call', requestId, toolCall}),
            onToolResult: (toolCallId, ok, summary) =>
              onEvent({type: 'tool-result', requestId, toolCallId, ok, summary}),
            onApprovalRequired: (pending) =>
              onEvent({type: 'approval-required', requestId, pending}),
          },
          request.signal ?? abortController.signal,
          request.sessionId,
        );

        onEvent({type: 'message-end', requestId, messageId: requestId, fullText: full});
        onEvent({type: 'run-end', requestId, aborted: false});
        return full;
      } catch (caught) {
        if (caught instanceof ApprovalRequiredError) {
          onEvent({type: 'run-end', requestId, aborted: false});
          throw caught;
        }
        const error = toRuntimeError(caught);
        if (error.code !== 'aborted') {
          onEvent({type: 'run-error', requestId, error});
        }
        onEvent({type: 'run-end', requestId, aborted: error.code === 'aborted'});
        throw error;
      } finally {
        _running = false;
        abortController = null;
      }
    },

    abort() {
      abortController?.abort();
    },

    async health() {
      return checkHealthDetailed(config.baseUrl, config.apiKey);
    },
  };

  return runtime;
}

// ============================================================
// Backend Real API Fetchers (Activity, Memory, Tools, Sessions)
// ============================================================

// ------------------------------------------------------------
// Runtime Capability Manifest — 能力发现 (不再 404-probing)
// ------------------------------------------------------------

/**
 * 拉取 Runtime Capability Manifest.
 *
 * 启动流程: 先 health → 再 capabilities.
 * 当 runtime 无原生 `/v1/apeireth/capabilities` 端点 (旧 runtime) 时,
 * 回落到保守的 legacy profile — 只声明历史契约证明存在的只读/对话能力,
 * 绝不推测 mutation. UI 据此降级 (mutation 按钮 disabled/隐藏).
 *
 * 404 仅作为 legacy fallback 触发条件, 不作为长期协议设计.
 */
export async function fetchCapabilities(_config: ApeirethConfig): Promise<CapabilityManifest> {
  // Canonical 2.0 now serves a dynamic manifest. Ask the runtime first; only
  // fall back to the static release contract when the endpoint is unreachable.
  const baseUrl = normalizeBaseUrl(_config.baseUrl);
  try {
    const res = await fetch(`${baseUrl}/v1/apeireth/capabilities`);
    if (res.ok) {
      const data = (await res.json()) as CapabilityManifest;
      if (data && Array.isArray(data.capabilities)) return data;
    }
  } catch {
    // fall through to the static contract
  }
  return releaseContractManifest();
}

/**
 * 查询 manifest 是否支持某 capability ID. 未知 ID 一律返回 false (保守).
 * null manifest (尚未加载) 也返回 false.
 *
 * 注意: supported 是静态语义 (runtime 是否实现该能力). 要判断「现在能否调用」
 * 应使用 capabilityAvailable() — 它反映 provider/凭据状态.
 */
export function capabilitySupported(manifest: CapabilityManifest | null, id: string): boolean {
  const cap = findCapability(manifest, id);
  return cap?.supported === true;
}

/**
 * Throw if a capability is not supported, preventing HTTP calls to known-404 endpoints.
 * Use this at the start of every function that calls a non-canonical introspection endpoint.
 *
 * @param manifest - current capability manifest (null = no capabilities)
 * @param capabilityId - the capability ID required for this operation
 * @param operation - human-readable operation name for error message
 * @throws Error with user-friendly message if capability unsupported
 */
export function requireCapability(manifest: CapabilityManifest | null, capabilityId: string, operation: string): void {
  if (!capabilitySupported(manifest, capabilityId)) {
    throw new Error(`${operation} 不支持: 当前运行时未实现 ${capabilityId} (Apeireth 2.0 canonical gateway 无此内省 API)`);
  }
}

/**
 * 查询某 capability 是否**当前可用** (动态语义, 受 provider/凭据影响).
 *
 * 语义:
 * - available === true → 可用
 * - available === false → 不可用 (reason 给出 machine-readable 原因)
 * - available === undefined (旧 manifest 无此字段) → 回落 supported (向后兼容)
 *
 * Runtime Decoupling: 桌面端 gating 应优先用 capabilityAvailable 判断「现在能否用」,
 * 用 capabilitySupported 判断「runtime 是否实现」, 两者 UI 可区分表达
 * (Unsupported vs Provider not configured).
 */
export function capabilityAvailable(manifest: CapabilityManifest | null, id: string): boolean {
  if (!manifest) return false;
  const cap = findCapability(manifest, id);
  if (!cap) return false;
  // 回落: 旧 manifest 无 available → 按 supported 解释.
  return cap.supported === true && (cap.available === undefined || cap.available === true);
}

/**
 * 查询某 capability 不可用的 machine-readable 原因 (仅当 available === false).
 * 可用或旧 manifest 回落时返回 null.
 */
export function capabilityUnavailableReason(
  manifest: CapabilityManifest | null,
  id: string,
): import('./types').CapabilityAvailabilityReason | null {
  if (!manifest) return null;
  const cap = findCapability(manifest, id);
  if (!cap) return null;
  if (cap.available === false) return cap.reason ?? null;
  return null;
}

/** 查找某 capability 完整声明 (跨组). */
const CAPABILITY_ALIASES: Record<string, string> = {
  'approvals.read': 'permissions.approval.read',
  'approvals.resolve': 'permissions.approval.resolve',
};

function canonicalCapabilityId(id: string): string {
  return CAPABILITY_ALIASES[id] ?? id;
}

export function findCapability(manifest: CapabilityManifest | null, id: string): Capability | null {
  if (!manifest) return null;
  const want = canonicalCapabilityId(id);
  let aliasMatch: Capability | null = null;
  for (const group of manifest.capabilities) {
    for (const cap of group.capabilities) {
      if (cap.id === want || cap.id === id) return cap;
      if (cap.alias_of === want || cap.alias_of === id) aliasMatch = cap;
    }
  }
  return aliasMatch;
}

/**
 * Static Apeireth 2.0 release contract, used when the runtime exposes no
 * capability-manifest endpoint.
 *
 * This is **frontend knowledge of its own release contract**, not runtime
 * introspection. It declares supported ONLY what the canonical gateway is
 * proven to serve (verified in crates/adapters/gateway/src/canonical_entry.rs):
 *
 *     GET  /health
 *     GET  /v1/models
 *     POST /v1/chat
 *     POST /v1/chat/completions
 *     POST /v1/approvals/resolve
 *
 * Everything else is unsupported. Unknown is unsupported — a capability is
 * never assumed present because an older backend once served it.
 *
 * History: this function previously declared panel introspection and SSE
 * capabilities optimistically. The fallback is intentionally smaller than the
 * live manifest so an older/unreachable gateway cannot make the desktop call a
 * route it has not proved to support.
 */
export function releaseContractManifest(): CapabilityManifest {
  const cap = (id: string, read: boolean, write: boolean, ops: string[]): Capability => ({
    id,
    supported: true,
    read,
    write,
    version: 1,
    operations: ops,
  });
  return {
    schema_version: 1,
    runtime: {service: 'apeireth-gateway-2.0', version: 'release-contract'},
    // `legacy` marks a manifest that did not come from the runtime itself.
    legacy: true,
    capabilities: [
      {name: 'health', capabilities: [cap('health', true, false, ['check'])]},
      {name: 'models', capabilities: [cap('models.list', true, false, ['list'])]},
      {name: 'chat', capabilities: [cap('chat.completions', true, true, ['stream'])]},
      {name: 'permissions', capabilities: [cap('permissions.approval.resolve', false, true, ['resolve'])]},
    ],
  };
}

/**
 * @deprecated Renamed to {@link releaseContractManifest}. The old name implied
 * a permissive legacy profile; the behaviour is now a conservative release
 * contract. Retained so existing imports keep compiling.
 */
export const legacyCapabilityManifest = releaseContractManifest;

/** 获取真实后端会话列表 (只读数据) */
export async function fetchBackendSessions(config: ApeirethConfig): Promise<Array<{id: string; title?: string; started_at: number; last_active_at: number; closed_at?: number; episode_count: number}>> {
  const res = await fetch(`${normalizeBaseUrl(config.baseUrl)}/v1/panel/sessions`, {
    headers: config.apiKey ? {Authorization: `Bearer ${config.apiKey}`} : {},
  });
  // Canonical contract shape: {sessions: [{id, title, created_at, updated_at, message_count, revision}]}
  const data = (await checkJson(res)) as {
    sessions?: Array<{
      id: string;
      title?: string | null;
      created_at?: number;
      updated_at?: number;
      message_count?: number;
      revision?: number;
    }>;
  };
  return (data.sessions || []).map((s) => ({
    id: s.id,
    title: typeof s.title === 'string' ? s.title : undefined,
    started_at: s.created_at ?? 0,
    last_active_at: s.updated_at ?? 0,
    episode_count: s.message_count ?? 0,
  }));
}

/** 搜索记忆条目 */
export async function fetchMemoryEpisodes(config: ApeirethConfig, query = '', limit = 100): Promise<MemoryEpisodeItem[]> {
  const url = `${normalizeBaseUrl(config.baseUrl)}/v1/panel/memory/episodes?limit=${limit}${query ? `&q=${encodeURIComponent(query)}` : ''}`;
  const res = await fetch(url, {
    headers: config.apiKey ? {Authorization: `Bearer ${config.apiKey}`} : {},
  });
  const data = (await checkJson(res)) as {
    episodes?: Array<{
      id: string;
      timestamp: number;
      role: string;
      content: string;
      session_id: string;
      category?: string | null;
      importance?: number | null;
      protected?: boolean | null;
      status?: string | null;
    }>;
  };
  return (data.episodes || []).map((e) => ({
    id: e.id,
    timestamp: e.timestamp,
    role: e.role,
    content: e.content,
    sessionId: e.session_id,
    category: typeof e.category === 'string' ? e.category : undefined,
    importance: typeof e.importance === 'number' ? e.importance : undefined,
    protected: typeof e.protected === 'boolean' ? e.protected : undefined,
    status: e.status === 'forgotten' ? 'forgotten' : e.status === 'active' ? 'active' : undefined,
  }));
}

/** 获取知识图谱节点和边 */
export async function fetchGraphData(config: ApeirethConfig): Promise<{facts: MemoryEpisodeItem[]; links: MemoryEpisodeItem[]}> {
  const res = await fetch(`${normalizeBaseUrl(config.baseUrl)}/v1/panel/graph`, {
    headers: config.apiKey ? {Authorization: `Bearer ${config.apiKey}`} : {},
  });
  // Canonical contract shape: {nodes: [{id, label, kind}], edges: [{from, to, weight, label?}]}
  const data = (await checkJson(res)) as {
    nodes?: Array<{id?: string; label?: string; kind?: string}>;
    edges?: Array<{from?: string; to?: string; weight?: number; label?: string | null}>;
  };
  return {
    facts: (data.nodes || [])
      .filter((n) => n.kind === 'episode')
      .map((n, i) => ({
        id: n.id || `factg-${i}`,
        timestamp: 0, // 图谱节点无时间戳, view 侧按 0 隐藏时间行
        role: 'fact',
        content: n.label || '(空节点)',
        sessionId: 'graph',
        category: 'fact',
      })),
    links: (data.edges || []).map((l, i) => ({
      id: `${l.from}-${l.to}-${i}`,
      timestamp: 0,
      role: 'link',
      content: `${l.from ?? '?'} → ${l.to ?? '?'}${typeof l.weight === 'number' ? ` (权重 ${l.weight})` : ''}`,
      sessionId: 'graph',
      category: 'link',
    })),
  };
}

/** 获取持久化审计记录 */
export async function fetchAuditLogs(config: ApeirethConfig, limit = 100): Promise<ActivityItem[]> {
  const res = await fetch(`${normalizeBaseUrl(config.baseUrl)}/v1/panel/audit?limit=${limit}`, {
    headers: config.apiKey ? {Authorization: `Bearer ${config.apiKey}`} : {},
  });
  // Canonical contract shape: {events: [{ts, event, service, detail?}]}
  const data = (await checkJson(res)) as {
    events?: Array<{ts?: number; event?: string; service?: string; detail?: string | null}>;
  };
  return (data.events || []).map((e, i) => ({
    id: `${e.event || 'audit'}-${e.ts || i}`,
    timestamp: e.ts ?? Date.now(),
    category: 'runtime' as ActivityItem['category'],
    title: e.event || '系统事件',
    summary: e.detail || e.event || '系统操作留痕',
    source: 'audit' as ActivityItem['source'],
    severity: 'info' as ActivityItem['severity'],
    detail: JSON.stringify(e, null, 2),
    raw: e,
  }));
}

/** 获取工具列表 (严格请求后端真实注册表端点) */
export async function fetchTools(config: ApeirethConfig): Promise<ToolItem[]> {
  const baseUrl = normalizeBaseUrl(config.baseUrl);
  // 先尝试 /v1/tools/list，再尝试 /v1/panel/tools
  let res = await fetch(`${baseUrl}/v1/tools/list`, {
    headers: config.apiKey ? {Authorization: `Bearer ${config.apiKey}`} : {},
  }).catch(() => null);

  if (!res || !res.ok) {
    res = await fetch(`${baseUrl}/v1/panel/tools`, {
      headers: config.apiKey ? {Authorization: `Bearer ${config.apiKey}`} : {},
    }).catch(() => null);
  }

  if (!res || !res.ok) {
    throw new HttpError(
      res ? res.status : 503,
      `后端工具注册表端点不可用 (${res ? `HTTP ${res.status}` : '连接失败'})`,
    );
  }

  const data = (await res.json()) as {
    tools?: Array<{
      name: string;
      description?: string;
      args_schema?: unknown;
      source?: string;
      permission?: string;
      available?: boolean;
    }>;
  };
  if (!Array.isArray(data.tools)) return [];
  return data.tools.map((t) => ({
    name: t.name,
    description: t.description || '无描述信息',
    argsSchema: t.args_schema,
    source: (t.source as ToolItem['source']) || 'builtin',
    permission: (t.permission as ToolItem['permission']) || 'prompt',
    available: t.available !== false,
  }));
}

/** Canonical pending-approval inbox. Session-scoped; never hits v1 grant APIs. */
export async function fetchCanonicalApprovals(
  config: ApeirethConfig,
  sessionId: string,
): Promise<CanonicalPendingApproval[]> {
  const res = await fetch(
    `${normalizeBaseUrl(config.baseUrl)}/v1/approvals?session=${encodeURIComponent(sessionId)}`,
  );
  const data = (await checkJson(res)) as {approvals?: CanonicalPendingApproval[]};
  return Array.isArray(data.approvals) ? data.approvals : [];
}

/** Resolve one pending approval through canonical Governance. */
export async function resolveCanonicalApproval(
  config: ApeirethConfig,
  pending: CanonicalPendingApproval,
  decision: 'approve' | 'reject' | 'cancel',
  reason?: string,
): Promise<{kind: 'completed'; text: string; events: CanonicalExecutionEvent[]} | {kind: 'pending'; pending: CanonicalPendingApproval}> {
  const res = await fetch(`${normalizeBaseUrl(config.baseUrl)}/v1/approvals/resolve`, {
    method: 'POST',
    headers: {'Content-Type': 'application/json'},
    body: JSON.stringify({
      session: pending.session,
      approval: pending.approval_id,
      decision,
      reason,
    }),
  });
  if (res.status === 202) {
    const next = (await res.json()) as CanonicalPendingApproval;
    return {kind: 'pending', pending: next};
  }
  const data = (await checkJson(res)) as {
    text?: string;
    events?: CanonicalExecutionEvent[];
  };
  return {kind: 'completed', text: data.text || '', events: data.events || []};
}

/** @deprecated v1 grant inbox. Does not request a dead URL. */
export async function fetchApprovalRequests(
  _config: ApeirethConfig,
): Promise<ApprovalRequestItem[]> {
  return [];
}

/** @deprecated v1 master-token grant. Does not request a dead URL. */
export async function grantToolPermission(
  _config: ApeirethConfig,
  _tool: string,
  _hours: number = 1,
  _masterToken: string = '',
): Promise<{ok: boolean; error?: string}> {
  return {ok: false, error: 'legacy grant path removed; use /v1/approvals/resolve'};
}

/** 写入记忆条目 */
export async function appendMemoryEpisode(
  config: ApeirethConfig,
  content: string,
  category: string = 'fact',
  sessionId: string = 'me',
): Promise<boolean> {
  // The canonical gateway requires a UUID session. The panel keeps one stable
  // "panel memory" session so scattered writes stay discoverable.
  const MEMORY_SESSION_KEY = 'apeireth-panel-memory-session';
  let target = sessionId && sessionId !== 'me' ? sessionId : '';
  if (!target) {
    try {
      target = localStorage.getItem(MEMORY_SESSION_KEY) || '';
      if (!target) {
        target = crypto.randomUUID();
        localStorage.setItem(MEMORY_SESSION_KEY, target);
      }
    } catch {
      target = crypto.randomUUID();
    }
  }
  const res = await fetch(`${normalizeBaseUrl(config.baseUrl)}/v1/memory/append`, {
    method: 'POST',
    headers: {
      'Content-Type': 'application/json',
      Authorization: config.apiKey ? `Bearer ${config.apiKey}` : '',
    },
    body: JSON.stringify({
      session: target,
      role: 'user',
      content: `[${category}] ${content}`,
    }),
  });
  return res.ok;
}

/** 本地会话持久化与容错迁移 (客户端专用) */
export function loadConversations(): Conversation[] {
  try {
    const raw = localStorage.getItem('apeireth-conversations');
    if (!raw) return [];
    const parsed = JSON.parse(raw);
    if (!Array.isArray(parsed)) return [];
    return parsed.map((item: any) => ({
      id: typeof item.id === 'string' ? item.id : crypto.randomUUID(),
      title: typeof item.title === 'string' ? item.title : '新对话',
      createdAt: typeof item.createdAt === 'number' ? item.createdAt : Date.now(),
      updatedAt: typeof item.updatedAt === 'number' ? item.updatedAt : Date.now(),
      messages: Array.isArray(item.messages) ? item.messages : [],
      scope: item.scope === 'project' ? 'project' : 'global',
      pinned: !!item.pinned,
      archived: !!item.archived,
      model: typeof item.model === 'string' ? item.model : undefined,
    }));
  } catch {
    return [];
  }
}


export function saveConversations(conversations: Conversation[]): void {
  localStorage.setItem('apeireth-conversations', JSON.stringify(conversations));
}

// Backward-compatible aliases for legacy / transition imports
export type MemoryEpisode = MemoryEpisodeItem;
export type ToolInfo = ToolItem;

// ============================================================
// Companion Presentation Events — reconciled from upstream master
// 后端信号驱动的伴随体表现态 (严禁前端造假). Raw CoT 仍不持久化;
// 这是 SSE 事件流的 presentation 层, 与 trace 持久化无关.
// ============================================================

export type CompanionPresentationState =
  | 'idle'
  | 'thinking'
  | 'speaking'
  | 'working'
  | 'reflecting'
  | 'concerned'
  | 'happy';

export interface CompanionEvent {
  text: string;
  ts: number;
  kind?: string;
}

/**
 * 订阅 Apeireth 网关事件流 (GET /v1/apeireth/events)
 * 接收 backend_ready / approval_required / approval_resolved 等网关级事件,
 * 转成轻量文本行交给调用方 (toast / 对话流纪律)。
 * 支持断线指数退避自动重连 (2s → 30s)。
 */
export function subscribeCompanionEvents(
  config: ApeirethConfig,
  onEvent: (event: CompanionEvent) => void,
): () => void {
  if (typeof EventSource === 'undefined') return () => {};
  const url = `${normalizeBaseUrl(config.baseUrl)}/v1/apeireth/events`;
  let active = true;
  let source: EventSource | null = null;
  let retryDelay = 2000;
  let retryTimer: ReturnType<typeof setTimeout> | null = null;

  const handler = (kind: string) => (msg: MessageEvent) => {
    let payload: Record<string, unknown> | null = null;
    try {
      payload = JSON.parse(typeof msg.data === 'string' ? msg.data : '');
    } catch {
      payload = null;
    }
    let text = '';
    if (kind === 'backend_ready') {
      text = '后端已就绪';
    } else if (kind === 'approval_required') {
      const tool = typeof payload?.tool_name === 'string' ? payload.tool_name : '工具';
      text = `[需要批准] ${tool}`;
    } else if (kind === 'approval_resolved') {
      text = '[审批已处理]';
    }
    if (text) onEvent({text, ts: Date.now(), kind});
  };

  const connect = () => {
    if (!active) return;
    const es = new EventSource(url);
    source = es;
    es.addEventListener('backend_ready', handler('backend_ready') as EventListener);
    es.addEventListener('approval_required', handler('approval_required') as EventListener);
    es.addEventListener('approval_resolved', handler('approval_resolved') as EventListener);
    es.onerror = () => {
      if (source === es) source = null;
      es.close();
      if (!active) return;
      retryTimer = setTimeout(connect, retryDelay);
      retryDelay = Math.min(retryDelay * 1.5, 30000);
    };
  };

  connect();

  return () => {
    active = false;
    if (retryTimer !== null) {
      clearTimeout(retryTimer);
      retryTimer = null;
    }
    source?.close();
    source = null;
  };
}

// ============================================================
// Core Capability Expansion Phase 6 — 后端 mutation 真实接入
// 所有调用都应先由 capabilitySupported() gate (UI 按钮). 不 fake.
// ============================================================

/** 后端会话生命周期记录 (对应 Rust SessionLifecycleRecord). */
export interface BackendSessionRecord {
  id: string;
  title: string | null;
  scope: 'global' | 'project';
  project_id: string | null;
  state: 'active' | 'archived' | 'closed';
  started_at: number;
  last_active_at: number;
  updated_at: number | null;
  archived_at: number | null;
  closed_at: number | null;
  revision: number;
  metadata: unknown;
}

/** 治理后的 episode (含 forgotten/protected/override). */
export interface GovernedEpisodeItem extends MemoryEpisodeItem {
  status: 'active' | 'forgotten';
  protected: boolean;
  content_override: string | null;
  revision: number;
  updated_at: number | null;
  updated_by: string | null;
  forgotten_at: number | null;
}

/** Grant 视图 (对应 Rust GrantView). */
export interface GrantView {
  id: string;
  name: string;
  tools: string[];
  paths: string[];
  expiry: string;
  op_budget: number | null;
  used_ops: number;
  spend_budget: number | null;
  spend_used: number;
  activated_at_ms: number;
  created_at_ms: number;
  active: boolean;
  expired: boolean;
}

/** Trace span (对应 Rust TraceSpan). */
export interface TraceSpanItem {
  span_id: string;
  trace_id: string;
  parent_span_id: string | null;
  kind: string;
  actor: string;
  status: string;
  summary: string | null;
  attributes: unknown;
  started_at: number;
  ended_at: number | null;
  session_id: string | null;
}

async function postJson(config: ApeirethConfig, path: string, body: unknown): Promise<{ok: boolean; status: number; data?: unknown; error?: string}> {
  try {
    const res = await fetch(`${normalizeBaseUrl(config.baseUrl)}${path}`, {
      method: 'POST',
      headers: {
        'Content-Type': 'application/json',
        Authorization: config.apiKey ? `Bearer ${config.apiKey}` : '',
      },
      body: JSON.stringify(body),
    });
    const data = await res.json().catch(() => null);
    if (!res.ok) {
      return {ok: false, status: res.status, error: (data && (data as {message?: string}).message) || `HTTP ${res.status}`};
    }
    return {ok: true, status: res.status, data};
  } catch (caught) {
    return {ok: false, status: 0, error: caught instanceof Error ? caught.message : String(caught)};
  }
}

async function patchJson(config: ApeirethConfig, path: string, body: unknown): Promise<{ok: boolean; status: number; data?: unknown; error?: string}> {
  try {
    const res = await fetch(`${normalizeBaseUrl(config.baseUrl)}${path}`, {
      method: 'PATCH',
      headers: {
        'Content-Type': 'application/json',
        Authorization: config.apiKey ? `Bearer ${config.apiKey}` : '',
      },
      body: JSON.stringify(body),
    });
    const data = await res.json().catch(() => null);
    if (!res.ok) {
      return {ok: false, status: res.status, error: (data && (data as {message?: string}).message) || `HTTP ${res.status}`};
    }
    return {ok: true, status: res.status, data};
  } catch (caught) {
    return {ok: false, status: 0, error: caught instanceof Error ? caught.message : String(caught)};
  }
}

// --- Memory governance ---

export async function forgetMemoryEpisode(config: ApeirethConfig, id: string, expectedRev: number, reason?: string): Promise<GovernedEpisodeItem | {error: string}> {
  const r = await postJson(config, `/v1/apeireth/memory/episodes/${encodeURIComponent(id)}/forget`, {expected_rev: expectedRev, reason});
  return r.ok ? (r.data as GovernedEpisodeItem) : {error: r.error || 'forget failed'};
}

export async function protectMemoryEpisode(config: ApeirethConfig, id: string, expectedRev: number): Promise<GovernedEpisodeItem | {error: string}> {
  const r = await postJson(config, `/v1/apeireth/memory/episodes/${encodeURIComponent(id)}/protect`, {expected_rev: expectedRev});
  return r.ok ? (r.data as GovernedEpisodeItem) : {error: r.error || 'protect failed'};
}

export async function unprotectMemoryEpisode(config: ApeirethConfig, id: string, expectedRev: number): Promise<GovernedEpisodeItem | {error: string}> {
  const r = await postJson(config, `/v1/apeireth/memory/episodes/${encodeURIComponent(id)}/unprotect`, {expected_rev: expectedRev});
  return r.ok ? (r.data as GovernedEpisodeItem) : {error: r.error || 'unprotect failed'};
}

// --- Permission grants (list + revoke) ---

export async function fetchGrants(config: ApeirethConfig): Promise<GrantView[]> {
  const res = await fetch(`${normalizeBaseUrl(config.baseUrl)}/v1/panel/grants`, {
    headers: config.apiKey ? {Authorization: `Bearer ${config.apiKey}`} : {},
  });
  if (!res.ok) return [];
  const data = (await res.json().catch(() => null)) as {
    grants?: Array<{permission?: string; capability?: string; granted_at?: number | null}>;
  } | null;
  return (data?.grants || []).map((g) => ({
    id: g.capability || g.permission || 'grant',
    name: g.permission || g.capability || 'grant',
    tools: g.capability ? [g.capability] : [],
    paths: [],
    expiry: '',
    op_budget: null,
    used_ops: 0,
    spend_budget: null,
    spend_used: 0,
    activated_at_ms: g.granted_at ?? 0,
    created_at_ms: g.granted_at ?? 0,
    active: true,
    expired: false,
  }));
}

export async function revokeGrant(
  config: ApeirethConfig,
  id: string,
  _masterToken: string = '',
): Promise<{ok: boolean; error?: string}> {
  const res = await fetch(`${normalizeBaseUrl(config.baseUrl)}/v1/panel/grants/revoke`, {
    method: 'POST',
    headers: {'Content-Type': 'application/json'},
    body: JSON.stringify({capability: id}),
  });
  if (!res.ok) {
    const data = (await res.json().catch(() => null)) as {error?: {message?: string}; message?: string} | null;
    return {ok: false, error: data?.error?.message || data?.message || `HTTP ${res.status}`};
  }
  return {ok: true};
}

// --- Trace ---

export async function fetchTraceDetail(config: ApeirethConfig, traceId: string): Promise<TraceSpanItem[] | {error: string}> {
  try {
    const res = await fetch(`${normalizeBaseUrl(config.baseUrl)}/v1/panel/traces/${encodeURIComponent(traceId)}`, {
      headers: config.apiKey ? {Authorization: `Bearer ${config.apiKey}`} : {},
    });
    if (!res.ok) {
      const err = (await res.json().catch(() => ({}))) as {message?: string};
      return {error: err.message || `HTTP ${res.status}`};
    }
    const data = (await res.json()) as {spans?: TraceSpanItem[]};
    return data.spans || [];
  } catch (caught) {
    return {error: caught instanceof Error ? caught.message : String(caught)};
  }
}

// --- Safety Guard & Workbench ---

export async function fetchGuardStatus(config: ApeirethConfig): Promise<GuardStatus | {error: string}> {
  try {
    const res = await fetch(`${normalizeBaseUrl(config.baseUrl)}/v1/safety/guard/status`, {
      headers: config.apiKey ? {Authorization: `Bearer ${config.apiKey}`} : {},
    });
    if (!res.ok) {
      const err = (await res.json().catch(() => ({}))) as {error?: {message?: string}};
      return {error: err.error?.message || `HTTP ${res.status}`};
    }
    return (await res.json()) as GuardStatus;
  } catch (caught) {
    return {error: caught instanceof Error ? caught.message : String(caught)};
  }
}

export async function fetchGuardEvents(config: ApeirethConfig, limit = 50): Promise<GuardEvent[] | {error: string}> {
  try {
    const res = await fetch(`${normalizeBaseUrl(config.baseUrl)}/v1/safety/guard/events?limit=${limit}`, {
      headers: config.apiKey ? {Authorization: `Bearer ${config.apiKey}`} : {},
    });
    if (!res.ok) {
      const err = (await res.json().catch(() => ({}))) as {error?: {message?: string}};
      return {error: err.error?.message || `HTTP ${res.status}`};
    }
    const data = (await res.json()) as {events?: GuardEvent[]};
    return data.events || [];
  } catch (caught) {
    return {error: caught instanceof Error ? caught.message : String(caught)};
  }
}

export async function evaluateGuard(config: ApeirethConfig, req: GuardDryRunRequest): Promise<GuardDryRunResponse | {error: string}> {
  try {
    const res = await fetch(`${normalizeBaseUrl(config.baseUrl)}/v1/safety/guard/evaluate`, {
      method: 'POST',
      headers: {
        'Content-Type': 'application/json',
        ...(config.apiKey ? {Authorization: `Bearer ${config.apiKey}`} : {}),
      },
      body: JSON.stringify(req),
    });
    if (!res.ok) {
      const err = (await res.json().catch(() => ({}))) as {error?: {message?: string}};
      return {error: err.error?.message || `HTTP ${res.status}`};
    }
    return (await res.json()) as GuardDryRunResponse;
  } catch (caught) {
    return {error: caught instanceof Error ? caught.message : String(caught)};
  }
}

export async function fetchWorkbenchTurn(config: ApeirethConfig, sessionId?: string): Promise<WorkbenchTurn | null | {error: string}> {
  try {
    const query = sessionId ? `?session=${encodeURIComponent(sessionId)}` : '';
    const res = await fetch(`${normalizeBaseUrl(config.baseUrl)}/v1/workbench/turn${query}`, {
      headers: config.apiKey ? {Authorization: `Bearer ${config.apiKey}`} : {},
    });
    if (!res.ok) {
      const err = (await res.json().catch(() => ({}))) as {error?: {message?: string}};
      return {error: err.error?.message || `HTTP ${res.status}`};
    }
    const data = (await res.json()) as {turn?: WorkbenchTurn | null};
    return data.turn ?? null;
  } catch (caught) {
    return {error: caught instanceof Error ? caught.message : String(caught)};
  }
}
