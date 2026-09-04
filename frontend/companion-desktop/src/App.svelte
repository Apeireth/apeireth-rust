<script lang="ts">
  import {onMount, tick, untrack} from 'svelte';
  import {
    Plus,
    ArrowUp,
    Square,
    ChevronDown,
    Sparkles,
    AlertCircle,
    PhoneCall,
    Sofa,
    Gauge,
    Eclipse,
    MessageCircleMore,
    History,
    Layers3,
    Wrench,
    Activity,
    ScrollText,
    Settings,
    PanelRight,
    X,
    Search,
  } from 'lucide-svelte';
  import MessageContent from './lib/MessageContent.svelte';
  import RuntimeModal from './lib/components/RuntimeModal.svelte';
  import VoiceCallModal from './components/VoiceCallModal.svelte';
  import { voiceCallManager } from './lib/voice';

  import ConfirmDialog from './lib/components/ConfirmDialog.svelte';
  import SceneLayer from './lib/scene/SceneLayer.svelte';
  import PlanetLayer from './lib/scene/PlanetLayer.svelte';
  import BridgeLayer from './lib/bridge/BridgeLayer.svelte';
  import DeepCabinLayer from './lib/cabin/DeepCabinLayer.svelte';
  import IntroLayer from './lib/intro/IntroLayer.svelte';
  import {localClockHour} from './lib/scene/timeline';
  import ConversationsView from './lib/ConversationsView.svelte';
  import ActivityView from './lib/views/ActivityView.svelte';
  import ToolsView from './lib/views/ToolsView.svelte';
  import MemoryView from './lib/MemoryView.svelte';
  import SettingsView from './lib/views/SettingsView.svelte';
  import Workbench from './lib/components/Workbench.svelte';
  import {applyDocumentTheme, resolveTheme} from './lib/theme';
  import type {Theme} from './lib/types';

  import type {
    ApeirethConfig,
    ApprovalRequestItem,
    CapabilityManifest,
    ChatMessage,
    Conversation,
    HealthState,
    RuntimeHealthReport,
    ToolCallDetails,
  } from './lib/types';
  import {
    checkHealthDetailed,
    createAgentRuntime,
    fetchCanonicalApprovals,
    resolveCanonicalApproval,
    ApprovalRequiredError,
    loadConfig,
    loadConversations,
    saveConfig,
    saveConversations,
    listModels,
    fetchCapabilities,
    subscribeCompanionEvents,
    capabilityAvailable,
    capabilitySupported,
    activePersonaOf,
    DEFAULT_PERSONAS,
    type CompanionPresentationState,
    type CanonicalPendingApproval,
  } from './lib/runtime';
  import {presenceStore, subscribePresence} from './lib/presence';
  import {isDesktop, resolveBackendEndpoint} from './lib/desktop-bridge';

  type DrawerId = 'history' | 'memory' | 'tools' | 'status' | 'logs' | 'settings';
  const DRAWER_META: Record<DrawerId, {eyebrow: string; title: string; sub: string; action: string}> = {
    history: {
      eyebrow: '管理',
      title: '历史',
      sub: '本地对话上下文与后端持久账本；删除需确认，归档不丢记录。',
      action: '新对话',
    },
    memory: {
      eyebrow: '认知',
      title: '记忆与知识库',
      sub: '持久化情节记忆、六历史流与结构化知识图谱。',
      action: '',
    },
    tools: {
      eyebrow: '能力',
      title: '工具管理与权限',
      sub: '注册工具、参数规范及待主人批准的高危调用。',
      action: '',
    },
    status: {
      eyebrow: '微内核',
      title: '系统状态',
      sub: '网关、模型服务、账本与记忆流的实时探测。',
      action: '深度诊断',
    },
    logs: {
      eyebrow: '观察与审计',
      title: '活动与调用日志',
      sub: '每一轮交互的延迟、Token、执行事件与工具调用轨迹。',
      action: '',
    },
    settings: {
      eyebrow: '首选项',
      title: '设置',
      sub: '模型提供商、人设、记忆策略、权限与数据。',
      action: '',
    },
  };

  // ---------- 波次 4：三模式骨架 ----------
  // companion=陪伴（舰桥+对话，默认）｜engineering=工程（深舱+页面层）｜focus=专注（临渊机位+chrome 淡出）
  type ModeId = 'companion' | 'engineering' | 'focus';
  // 开发覆写 ?mode=focus|engineering（与 ?hour= 同纪律：初始模式按参数设定，供无头截图验证）
  const modeQuery =
    typeof window !== 'undefined' ? new URLSearchParams(window.location.search).get('mode') : null;
  const initialMode: ModeId =
    modeQuery === 'engineering' || modeQuery === 'focus' ? modeQuery : 'companion';
  let mode = $state<ModeId>(initialMode);

  const modes = [
    {id: 'companion' as const, label: '陪伴 · 舰桥', icon: Sofa},
    {id: 'engineering' as const, label: '工程 · 深舱', icon: Gauge},
    {id: 'focus' as const, label: '专注 · 临渊', icon: Eclipse},
  ];

  const themeQuery =
    typeof window !== 'undefined' ? new URLSearchParams(window.location.search).get('theme') : null;
  let activeTheme = $state<Theme>(resolveTheme(loadConfig().theme, themeQuery));
  const isEssenceTheme = $derived(activeTheme === 'essence');

  // ---------- 开场动画（火之文明史序章）门禁 ----------
  // 【2026-08-22 封存】v1 审美验收未过（主人评：一言难尽），默认关闭不再自动播放，
  // 保留全部引擎代码待额度充足后重启打磨（详见 docs/design/intro-animation.md）。
  // 重看方式：?intro=1 强制重播；?it=<秒> 冻结开场时钟供无头截图（永不自然播完）；
  // prefers-reduced-motion → 强制参数也不播，直接进产品。
  // 播放期间 SceneLayer/PlanetLayer/BridgeLayer 全程挂载绝不卸载（无缝接缝的物理基础），
  // 全部 chrome 以 .intro-playing class 隐藏；落幅 1.5s IntroLayer 淡出，活舰桥显形。
  const introQuery =
    typeof window !== 'undefined' ? new URLSearchParams(window.location.search) : null;
  const introForced = introQuery?.get('intro') === '1' || (introQuery?.has('it') ?? false);
  const introReduceMotion =
    typeof window !== 'undefined' &&
    typeof window.matchMedia === 'function' &&
    window.matchMedia('(prefers-reduced-motion: reduce)').matches;
  // 封存期：仅显式强制参数才播（reduced-motion 仍有最终否决权）
  let introPlaying = $state(introForced && !introReduceMotion);

  function handleIntroComplete(): void {
    introPlaying = false;
    try {
      localStorage.setItem('ap-intro-seen', '1');
    } catch {
      /* 存储不可用时静默跳过 */
    }
  }

  // 初始视图：对话始终居中；工程/专注只切场景层，不再把主区换成页面层
  let drawerSec = $state<DrawerId | null>(null);
  let wbOpen = $state(false);
  let openPanel = $state<'model' | 'ctx' | null>(null);
  let availableModels = $state<string[]>([]);
  let modelsLoading = $state(false);
  let modelQuery = $state('');

  // 场景受控机位：专注=临渊(1)，陪伴=远眺(0)，工程=null（深舱不透明盖住场景，引擎自管理）
  const sceneCamera = $derived(mode === 'focus' ? 1 : mode === 'engineering' ? null : 0);

  function setMode(next: ModeId): void {
    if (next === mode) return;
    mode = next;
    if (next !== 'focus') {
      drawerSec = null;
    }
  }

  // 点黑洞 = 进入专注模式（§4.2 临渊机位由引擎承担，此处管模式）；
  // 工程模式下深舱盖住黑洞，忽略穿透到场景的点击
  function handleBlackholeClick(): void {
    if (mode === 'engineering') return;
    setMode('focus');
  }

  // Esc 退出专注回陪伴
  function handleModeKeydown(e: KeyboardEvent): void {
    if (e.key === 'Escape' && mode === 'focus') setMode('companion');
  }

  // 舰内时刻（规范 §3 时间线照明）：默认跟随本地时钟，30s 心跳刷新；
  // 开发调试覆写 ?hour=22 强制指定时刻（截图验证各时间档用），覆写时不走时钟。
  const hourQuery =
    typeof window !== 'undefined' ? new URLSearchParams(window.location.search).get('hour') : null;
  const hourOverride =
    hourQuery !== null && hourQuery.trim() !== '' && Number.isFinite(Number(hourQuery))
      ? Number(hourQuery)
      : null;
  let timelineHour = $state(hourOverride ?? localClockHour());
  let config = $state<ApeirethConfig>(loadConfig());
  const activePersona = $derived(activePersonaOf(config));
  const personaList = $derived(
    config.personas && config.personas.length > 0 ? config.personas : DEFAULT_PERSONAS,
  );
  let personaMenuOpen = $state(false);

  function setActivePersona(id: string): void {
    const target = personaList.find((p) => p.id === id);
    if (!target) return;
    const next: ApeirethConfig = {...config, activePersonaId: id};
    if (target.model) next.model = target.model;
    config = next;
    saveConfig(next);
    agentRuntime = createAgentRuntime(next);
    personaMenuOpen = false;
  }

  let conversations = $state<Conversation[]>(loadConversations());
  let activeId = $state<string | null>(null);
  let draft = $state('');
  let busy = $state(false);
  let error = $state('');
  let pendingApprovals = $state<ApprovalRequestItem[]>([]);
  let pendingCanonical = $state<CanonicalPendingApproval | null>(null);
  let approvalBusy = $state(false);
  let isReasoning = $state(false);
  let isExecutingTool = $state(false);
  let legacyToast = $state('');
  let agentRuntime = $state(createAgentRuntime(loadConfig()));

  // 深度运行时报告与健康状态
  let healthState = $state<HealthState>('connecting');
  let healthReport = $state<RuntimeHealthReport>({
    overall: 'connecting',
    baseUrl: loadConfig().baseUrl,
    subsystems: [],
    model: loadConfig().model,
  });
  let showRuntimeModal = $state(false);
  let showVoiceCall = $state(false);
  let isRefreshingHealth = $state(false);

  async function openVoiceCall() {
    showVoiceCall = true;
    await voiceCallManager.startCall();
  }

  async function handleVoiceMessage(userText: string): Promise<string> {
    if (!userText.trim()) return '';
    const userMsg: ChatMessage = {
      id: crypto.randomUUID(),
      role: 'user',
      text: userText,
      time: new Date().toLocaleTimeString('zh-CN', {hour: '2-digit', minute: '2-digit'}),
      timestamp: Date.now(),
    };
    if (activeConversation) {
      activeConversation.messages.push(userMsg);
      activeConversation.updatedAt = Date.now();
      saveConversations(conversations);
    }
    try {
      const agent = createAgentRuntime(config);
      const resp = await agent.run(
        {
          messages: activeConversation?.messages.map((m) => ({
            role: m.role,
            content: m.text,
          })) || [{role: 'user', content: userText}],
          model: {id: config.model},
          sessionId: activeConversation?.id,
        },
        () => {},
      );
      const assistantMsg: ChatMessage = {
        id: crypto.randomUUID(),
        role: 'assistant',
        text: resp,
        time: new Date().toLocaleTimeString('zh-CN', {hour: '2-digit', minute: '2-digit'}),
        timestamp: Date.now(),
      };
      if (activeConversation) {
        activeConversation.messages.push(assistantMsg);
        activeConversation.updatedAt = Date.now();
        saveConversations(conversations);
      }
      return resp;
    } catch (e) {
      console.error('Voice turn failed:', e);
      return '抱歉，实时对话连接暂时中断。';
    }
  }

  // Runtime Capability Manifest — gate UI 按钮的依据 (不再 404-probing).
  let capabilities = $state<CapabilityManifest | null>(null);


  // 智能滚动状态管理
  let messagesContainer = $state<HTMLElement | null>(null);
  let isNearBottom = $state(true);
  let showScrollBottomBtn = $state(false);

  // 星尘条（规范 §5.3：memory_recall → 对话流中的「他想起了 N 段记忆」，脱敏，不含原文）。
  // 会话内瞬态：不持久化——星尘是「此刻」的痕迹，刷新即散。按会话 id 分桶。
  interface Stardust {
    id: string;
    found: number;
    keywords: string[];
    ts: number;
  }
  let stardusts = $state<Record<string, Stardust[]>>({});
  let lastDustAt = 0; // 非响应式记账：已消费到的 memory_recall receivedAt

  // 后端信号驱动的伴随体表现态 (严禁前端造假). Reconciled from master.
  const companionPresentation = $derived.by<CompanionPresentationState>(() => {
    if (pendingApprovals.length > 0) return 'concerned';
    if (isExecutingTool) return 'working';
    if (isReasoning) return 'thinking';
    if (busy) return 'speaking';
    return 'idle';
  });

  const activeConversation = $derived(
    conversations.find((item) => item.id === activeId) || null,
  );

  const activeMessages = $derived(activeConversation?.messages || []);

  // 对话流 = 消息 + 星尘条，按时间戳归并（同刻消息优先于星尘）
  type FlowItem =
    | {kind: 'msg'; id: string; ts: number; message: ChatMessage}
    | {kind: 'dust'; id: string; ts: number; dust: Stardust};

  const flowItems = $derived.by<FlowItem[]>(() => {
    const items: FlowItem[] = activeMessages.map((m) => ({
      kind: 'msg',
      id: m.id,
      ts: m.timestamp ?? 0,
      message: m,
    }));
    const dusts = (activeId ? stardusts[activeId] : undefined) ?? [];
    for (const d of dusts) items.push({kind: 'dust', id: d.id, ts: d.ts, dust: d});
    items.sort((a, b) => a.ts - b.ts || (a.kind === b.kind ? 0 : a.kind === 'msg' ? -1 : 1));
    return items;
  });

  // 他的卡片左缘光晕强度：由真实 presence 状态驱动（规范 §5.3 光晕随 bright 呼吸）；
  // 无数据时取静息微光 —— 金线本身不消失，消失的只是呼吸。
  const presenceGlow = $derived.by(() => {
    const cur = $presenceStore.current;
    if (!cur) return 0.14;
    const base = cur.mode === 'speaking' ? 0.5 : cur.mode === 'thinking' ? 0.32 : 0.16;
    return Math.min(0.65, base + Math.max(0, cur.p) * 0.12);
  });

  const healthLabel: Record<HealthState, string> = {
    connecting: '连接中…',
    online: '后端已连接',
    ready: '后端已连接',
    degraded: '降级运行',
    generating: '正在生成…',
    error: '运行异常',
    offline: '后端离线',
  };

  const quickPrompts = [
    '聊聊今天',
    '查看我的记忆',
    '帮我处理一件事',
    '检查系统状态',
  ];

  function ensureConversation(): Conversation {
    if (activeConversation) return activeConversation;
    const now = Date.now();
    const conversation: Conversation = {
      id: crypto.randomUUID(),
      title: '新对话',
      createdAt: now,
      updatedAt: now,
      messages: [],
      scope: 'global',
      model: config.model,
    };
    conversations = [conversation, ...conversations];
    activeId = conversation.id;
    persist();
    return conversation;
  }

  function persist(): void {
    saveConversations(conversations);
  }

  function updateConversation(id: string, patch: Partial<Conversation>): void {
    conversations = conversations.map((item) =>
      item.id === id ? {...item, ...patch, updatedAt: Date.now()} : item,
    );
    persist();
  }

  function updateMessage(id: string, messageId: string, patch: Partial<ChatMessage>): void {
    conversations = conversations.map((item) => {
      if (item.id !== id) return item;
      return {
        ...item,
        updatedAt: Date.now(),
        messages: item.messages.map((m) => (m.id === messageId ? {...m, ...patch} : m)),
      };
    });
    persist();
  }

  function pushMessage(conversationId: string, message: ChatMessage): void {
    conversations = conversations.map((item) => {
      if (item.id !== conversationId) return item;
      return {...item, updatedAt: Date.now(), messages: [...item.messages, message]};
    });
    persist();
  }

  /** 按 id 原子拼接流式文本 delta. */
  function appendDelta(conversationId: string, messageId: string, delta: string): void {
    conversations = conversations.map((item) => {
      if (item.id !== conversationId) return item;
      return {
        ...item,
        updatedAt: Date.now(),
        messages: item.messages.map((m) =>
          m.id === messageId ? {...m, text: m.text + delta} : m,
        ),
      };
    });
    persist();
  }

  /** 按 id 原子拼接推理思考 delta. Reconciled from master. */
  function appendReasoningDelta(conversationId: string, messageId: string, delta: string): void {
    conversations = conversations.map((item) => {
      if (item.id !== conversationId) return item;
      return {
        ...item,
        updatedAt: Date.now(),
        messages: item.messages.map((m) => (m.id === messageId ? {...m, reasoning: (m.reasoning || '') + delta} : m)),
      };
    });
    persist();
  }

  function updateMessageToolCall(
    conversationId: string,
    messageId: string,
    toolCall: ToolCallDetails,
  ): void {
    conversations = conversations.map((item) => {
      if (item.id !== conversationId) return item;
      return {
        ...item,
        updatedAt: Date.now(),
        messages: item.messages.map((m) => {
          if (m.id !== messageId) return m;
          const list = m.toolCalls ? [...m.toolCalls] : [];
          const idx = list.findIndex((t) => t.id === toolCall.id);
          if (idx >= 0) {
            list[idx] = toolCall;
          } else {
            list.push(toolCall);
          }
          return {...m, toolCalls: list};
        }),
      };
    });
    persist();
  }

  /**
   * 他主动开口（legacy `[他说] …` 行，契约 §5.1；initiative/spoke 的完整话术由此送达）：
   * 按规范 §5.3 走与「他的消息」相同的卡片语言进入对话流。
   */
  function appendProactiveMessage(text: string): void {
    const conversation = ensureConversation();
    pushMessage(conversation.id, {
      id: crypto.randomUUID(),
      role: 'assistant',
      text,
      time: new Date().toLocaleTimeString('zh-CN', {hour: '2-digit', minute: '2-digit'}),
      timestamp: Date.now(),
      proactive: 'initiative',
    });
  }

  // 滚动位置监听与控制
  function handleScroll() {
    if (!messagesContainer) return;
    const {scrollTop, scrollHeight, clientHeight} = messagesContainer;
    const distanceToBottom = scrollHeight - scrollTop - clientHeight;
    isNearBottom = distanceToBottom < 80;
    showScrollBottomBtn = distanceToBottom > 150;
  }

  function scrollToBottom(smooth = false) {
    if (!messagesContainer) return;
    if (smooth) {
      messagesContainer.scrollTo({
        top: messagesContainer.scrollHeight,
        behavior: 'smooth',
      });
    } else {
      messagesContainer.scrollTop = messagesContainer.scrollHeight;
    }
    isNearBottom = true;
    showScrollBottomBtn = false;
  }

  async function triggerAutoScroll() {
    if (isNearBottom) {
      await tick();
      scrollToBottom(false);
    }
  }

  async function refreshConnection(): Promise<void> {
    isRefreshingHealth = true;
    try {
      await adoptSupervisorEndpoint();
      const report = await checkHealthDetailed(config.baseUrl, config.apiKey, config.model, config.provider);
      healthReport = report;
      if (!busy) {
        healthState = report.overall;
      }
      // health 之后拉取 capability manifest (runtime version 变化/重连时刷新).
      // 不每次 render 重复 fetch — 仅在 refreshConnection (节拍/手动) 时.
      if (report.overall !== 'offline') {
        const prevVersion = capabilities?.runtime.version;
        const fresh = await fetchCapabilities(config);
        // 仅在 version 变化或首次加载时更新 (避免节拍无谓刷新覆盖).
        if (!capabilities || fresh.runtime.version !== prevVersion || fresh.legacy !== capabilities.legacy) {
          capabilities = fresh;
        }
        if (activeId) {
          const inbox = await fetchCanonicalApprovals(config, activeId).catch(() => []);
          pendingApprovals = inbox.map((item) => ({
            id: item.approval_id,
            tool: item.tool_name,
            reason: item.governance_reason,
            status: 'pending' as const,
          }));
        } else {
          pendingApprovals = [];
        }
      } else {
        pendingApprovals = [];
      }
    } finally {
      isRefreshingHealth = false;
    }
  }

  async function send(customText?: string): Promise<void> {
    const text = (customText ?? draft).trim();
    if (!text || busy) return;
    const conversation = ensureConversation();
    const conversationId = conversation.id;
    const history = conversation.messages
      .filter((m) => m.role === 'user' || m.role === 'assistant')
      .map((m) => ({role: m.role, content: m.text}));

    if (!customText) draft = '';
    busy = true;
    isReasoning = false;
    isExecutingTool = false;
    healthState = 'generating';
    error = '';
    // presence 遗留整合点 2：对话请求开始 → thinking（等首字节）；首段文本到达 → speaking
    presenceStore.setChatActive(true);

    const userMessage: ChatMessage = {
      id: crypto.randomUUID(),
      role: 'user',
      text,
      time: new Date().toLocaleTimeString('zh-CN', {hour: '2-digit', minute: '2-digit'}),
      timestamp: Date.now(),
    };
    const assistantMessage: ChatMessage = {
      id: crypto.randomUUID(),
      role: 'assistant',
      text: '',
      time: new Date().toLocaleTimeString('zh-CN', {hour: '2-digit', minute: '2-digit'}),
      timestamp: Date.now(),
      streaming: true,
      toolCalls: [],
      reasoning: '',
      modelInfo: {id: config.model, provider: 'apeireth'},
    };

    pushMessage(conversationId, userMessage);
    pushMessage(conversationId, assistantMessage);

    if (conversation.messages.length <= 2) {
      updateConversation(conversationId, {title: text.slice(0, 24)});
    }

    await tick();
    scrollToBottom(true);

    try {
      const full = await agentRuntime.run(
        {
          messages: [...history, {role: 'user', content: text}],
          model: {id: config.model, provider: 'apeireth'},
          sessionId: conversationId,
          context: {user: '主人'},
        },
        (event) => {
          if (event.type === 'text-delta') {
            isReasoning = false;
            presenceStore.setSpeaking(true); // 流式输出进行中 = 他在说话
            appendDelta(conversationId, assistantMessage.id, event.text);
            void triggerAutoScroll();
          } else if (event.type === 'reasoning-delta') {
            isReasoning = true;
            appendReasoningDelta(conversationId, assistantMessage.id, event.text);
          } else if (event.type === 'tool-call') {
            isExecutingTool = true;
            updateMessageToolCall(conversationId, assistantMessage.id, event.toolCall);
            void triggerAutoScroll();
          } else if (event.type === 'tool-result') {
            isExecutingTool = false;
            void triggerAutoScroll();
          } else if (event.type === 'approval-required') {
            pendingCanonical = event.pending;
          }
        },
      );
      updateMessage(conversationId, assistantMessage.id, {
        text: full || '(空响应)',
        streaming: false,
      });
    } catch (caught) {
      if (caught instanceof ApprovalRequiredError) {
        pendingCanonical = caught.pending;
        updateMessage(conversationId, assistantMessage.id, {
          streaming: false,
          text: `等待批准：${caught.pending.tool_name}`,
        });
        return;
      }
      const isAborted =
        (caught instanceof Error && caught.name === 'AbortError') ||
        (typeof caught === 'object' && caught !== null && (caught as any).code === 'aborted');
      const msg =
        typeof caught === 'string'
          ? caught
          : caught instanceof Error
            ? caught.message
            : typeof caught === 'object' && caught !== null && 'message' in caught
              ? String((caught as any).message)
              : String(caught);
      if (isAborted) {
        updateMessage(conversationId, assistantMessage.id, {streaming: false, aborted: true});
      } else {
        error = msg;
        updateMessage(conversationId, assistantMessage.id, {
          text: '',
          streaming: false,
          error: msg,
        });
        healthState = 'error';
      }
    } finally {
      busy = false;
      isReasoning = false;
      isExecutingTool = false;
      presenceStore.setSpeaking(false);
      presenceStore.setChatActive(false);
      // 生成结束: 恢复真实 health (backend 可能已离线)
      await refreshConnection();
      await tick();
      void triggerAutoScroll();
    }
  }

  async function resolvePending(decision: 'approve' | 'reject'): Promise<void> {
    if (!pendingCanonical || approvalBusy) return;
    approvalBusy = true;
    const conversationId = activeId;
    try {
      const result = await resolveCanonicalApproval(config, pendingCanonical, decision);
      if (result.kind === 'pending') {
        pendingCanonical = result.pending;
        return;
      }
      pendingCanonical = null;
      if (conversationId) {
        const conversation = conversations.find((item) => item.id === conversationId);
        const last = conversation?.messages.filter((m) => m.role === 'assistant').at(-1);
        if (last) {
          updateMessage(conversationId, last.id, {
            text: result.text || (decision === 'approve' ? '(空响应)' : '已拒绝该工具调用'),
            streaming: false,
          });
        }
      }
    } catch (caught) {
      error = describeCaughtSafe(caught);
    } finally {
      approvalBusy = false;
      await refreshConnection();
    }
  }

  function describeCaughtSafe(caught: unknown): string {
    if (caught instanceof Error) return caught.message;
    return String(caught);
  }

  function stop(): void {
    agentRuntime.abort();
  }

  /** 重试一条 assistant 消息: 找到上一条用户消息重新发送 */
  function retryAssistantMessage(messageId: string): void {
    if (busy || !activeConversation) return;
    const msgs = activeConversation.messages;
    const idx = msgs.findIndex((m) => m.id === messageId);
    if (idx < 0) return;
    let userText = '';
    for (let i = idx - 1; i >= 0; i--) {
      if (msgs[i].role === 'user') {
        userText = msgs[i].text;
        break;
      }
    }
    // 截断该 assistant 消息及之后的消息
    const filtered = msgs.slice(0, idx);
    updateConversation(activeConversation.id, {messages: filtered});
    if (userText) {
      void send(userText);
    }
  }

  /** 编辑用户消息仅保存 */
  function editUserMessageSave(messageId: string, newText: string): void {
    if (!activeConversation) return;
    updateMessage(activeConversation.id, messageId, {text: newText});
  }

  /** 编辑用户消息并重新生成回答（截断后续回答从新文本开始） */
  function editUserMessageAndRegenerate(messageId: string, newText: string): void {
    if (busy || !activeConversation) return;
    const msgs = activeConversation.messages;
    const idx = msgs.findIndex((m) => m.id === messageId);
    if (idx < 0) return;
    // 截断该用户消息及之后的所有消息
    const filtered = msgs.slice(0, idx);
    updateConversation(activeConversation.id, {messages: filtered});
    void send(newText);
  }

  /** 分支会话：从 messageId 处截取历史，创建新分支会话并跳转 */
  function branchFromMessage(messageId: string): void {
    if (!activeConversation) return;
    const msgs = activeConversation.messages;
    const idx = msgs.findIndex((m) => m.id === messageId);
    if (idx < 0) return;
    const sliced = JSON.parse(JSON.stringify(msgs.slice(0, idx + 1))) as ChatMessage[];
    const now = Date.now();
    const branchConv: Conversation = {
      id: crypto.randomUUID(),
      title: `${activeConversation.title.replace(/\s*\(分支.*\)$/, '')} (分支 ${new Date(now).toLocaleTimeString('zh-CN', {hour: '2-digit', minute: '2-digit'})})`,
      createdAt: now,
      updatedAt: now,
      messages: sliced,
      scope: 'global',
      model: config.model,
    };
    conversations = [branchConv, ...conversations];
    activeId = branchConv.id;
    persist();
  }

  function newConversation(): void {
    const now = Date.now();
    const conversation: Conversation = {
      id: crypto.randomUUID(),
      title: '新对话',
      createdAt: now,
      updatedAt: now,
      messages: [],
      scope: 'global',
      model: config.model,
    };
    conversations = [conversation, ...conversations];
    activeId = conversation.id;
    drawerSec = null;
    persist();
  }

  function openConversation(id: string): void {
    activeId = id;
    drawerSec = null;
  }

  function archiveConversation(id: string): void {
    const conv = conversations.find((item) => item.id === id);
    if (conv) updateConversation(id, {archived: !conv.archived});
  }

  function deleteConversation(id: string): void {
    conversations = conversations.filter((item) => item.id !== id);
    if (activeId === id) activeId = null;
    persist();
  }

  function applyQuickPrompt(promptText: string) {
    draft = promptText;
  }

  function relativeTime(ts: number): string {
    const d = Date.now() - ts;
    if (d < 60_000) return '刚刚';
    if (d < 3_600_000) return `${Math.max(1, Math.round(d / 60_000))} 分钟前`;
    if (d < 86_400_000) return '今天';
    if (d < 172_800_000) return '昨天';
    return `${Math.round(d / 86_400_000)} 天前`;
  }

  const drawerMeta = $derived(drawerSec ? DRAWER_META[drawerSec] : null);
  const modelLetter = $derived(
    (config.model.match(/[A-Za-z]/)?.[0] ?? 'M').toUpperCase(),
  );
  const hdState = $derived(
    busy
      ? '正在输出'
      : healthState === 'offline'
        ? '离线'
        : healthState === 'error'
          ? '异常'
          : healthState === 'degraded'
            ? '降级'
            : '在线',
  );
  const suggestions = $derived(
    conversations
      .filter((c) => !c.archived)
      .slice(0, 3)
      .map((c) => ({id: c.id, title: c.title, src: relativeTime(c.updatedAt)})),
  );
  const ctxUsage = $derived.by(() => {
    const chars = activeMessages.reduce((n, m) => n + (m.text?.length ?? 0), 0);
    const tokens = Math.max(0, Math.round(chars / 4));
    const cap = 200_000;
    const pct = Math.min(100, Math.round((tokens / cap) * 100));
    const circ = 2 * Math.PI * 9;
    return {tokens, cap, pct, dashoffset: circ * (1 - pct / 100), dasharray: circ};
  });
  const filteredModels = $derived(
    availableModels.filter((id) =>
      modelQuery.trim() ? id.toLowerCase().includes(modelQuery.trim().toLowerCase()) : true,
    ),
  );

  function openDrawer(id: DrawerId): void {
    drawerSec = id;
    openPanel = null;
  }

  function closeDrawer(): void {
    drawerSec = null;
  }

  function toggleRail(id: 'chat' | DrawerId): void {
    if (id === 'chat') {
      closeDrawer();
      return;
    }
    if (drawerSec === id) closeDrawer();
    else openDrawer(id);
  }

  function onDrawerAction(): void {
    if (drawerSec === 'history') newConversation();
    else if (drawerSec === 'status') showRuntimeModal = true;
  }

  function toggleWb(force?: boolean): void {
    wbOpen = force === undefined ? !wbOpen : force;
  }

  function closePanels(): void {
    openPanel = null;
  }

  function togglePanel(id: 'model' | 'ctx'): void {
    openPanel = openPanel === id ? null : id;
    if (openPanel === 'model') void loadModelList();
  }

  async function loadModelList(): Promise<void> {
    modelsLoading = true;
    try {
      const ids = await listModels(config.baseUrl, config.apiKey);
      availableModels = ids.length ? ids : [config.model];
    } catch {
      availableModels = config.model ? [config.model] : [];
    } finally {
      modelsLoading = false;
    }
  }

  function selectModel(id: string): void {
    if (!id || id === config.model) {
      closePanels();
      return;
    }
    config = {...config, model: id};
    saveConfig(config);
    agentRuntime = createAgentRuntime(config);
    closePanels();
  }

  function handleComposerInput(event: Event): void {
    const el = event.currentTarget as HTMLTextAreaElement;
    el.style.height = 'auto';
    el.style.height = `${el.scrollHeight}px`;
  }

  function handleChromeKey(e: KeyboardEvent): void {
    handleModeKeydown(e);
    if (e.key === 'Escape') {
      closePanels();
      closeDrawer();
    }
  }

  function pickSuggestion(item: {id: string; title: string}): void {
    draft = item.title;
  }

  const overallLabel = $derived(
    healthReport.overall === 'online'
      ? '正常'
      : healthReport.overall === 'degraded'
        ? '降级'
        : healthReport.overall === 'offline'
          ? '离线'
          : healthReport.overall === 'error'
            ? '异常'
            : '连接中',
  );

  // 星尘条：监听 presenceStore.recentEvents，新 memory_recall 事件落进当前会话流。
  // recentEvents 已由 store 按 (type, at) 去重；此处按 receivedAt 水位线消费，幂等。
  $effect(() => {
    const records = $presenceStore.recentEvents;
    const fresh: Stardust[] = [];
    let maxAt = lastDustAt;
    for (const r of records) {
      if (r.event.type !== 'memory_recall') continue;
      if (r.receivedAt <= lastDustAt) continue;
      maxAt = Math.max(maxAt, r.receivedAt);
      fresh.push({
        id: `dust-${r.receivedAt}`,
        found: r.event.found,
        keywords: Array.isArray(r.event.keywords) ? r.event.keywords : [],
        ts: r.receivedAt,
      });
    }
    if (!fresh.length) return;
    lastDustAt = maxAt;
    untrack(() => {
      const convId = activeId;
      if (!convId) return; // 无活动会话时不落（边缘：事件发生在对话外）
      const list = stardusts[convId] ?? [];
      stardusts = {...stardusts, [convId]: [...list, ...fresh]};
      void triggerAutoScroll();
    });
  });

  /**
   * In packaged desktop mode the BackendSupervisor allocates an ephemeral port
   * at each launch, so a persisted baseUrl points at a port nothing is
   * listening on. The supervisor is authoritative; adopt its endpoint before
   * the first health probe. No-op in web mode.
   */
  async function adoptSupervisorEndpoint(): Promise<void> {
    if (!isDesktop()) return;
    const endpoint = await resolveBackendEndpoint(config.baseUrl);
    if (endpoint && endpoint !== config.baseUrl) {
      config = {...config, baseUrl: endpoint};
      // Rebuild the runtime so in-flight transport targets the live port.
      agentRuntime = createAgentRuntime(config);
      healthReport = {...healthReport, baseUrl: endpoint};
    }
  }

  onMount(() => {
    applyDocumentTheme(activeTheme);
    if (!activeId && conversations.length) activeId = conversations[0].id;
    if (window.innerWidth < 1180) wbOpen = false;
    // Resolve the real endpoint first, then probe: in packaged mode a probe
    // against the stale persisted port would report a false offline state.
    void adoptSupervisorEndpoint().then(() => refreshConnection());

    // 舰内时刻心跳：无 ?hour= 覆写时每 30s 对齐本地时钟（照明过渡由 CSS/rAF 慢性子承担）
    const hourTimer =
      hourOverride === null
        ? window.setInterval(() => {
            timelineHour = localClockHour();
          }, 30000)
        : null;

    // Capability gate for the two /v1/apeireth/events subscribers below.
    // subscribeCompanionEvents retries on an exponential backoff loop that
    // never gives up, so gate it on both static support and live availability.
    const eventStreamSupported =
      capabilitySupported(capabilities, 'activity.sse') &&
      capabilityAvailable(capabilities, 'activity.sse');

    // presence 频道主订阅（波次 2 壳层整合点）：EventSource + 指数退避 + SIM 纪律。
    // 与下方 legacy 订阅并存是设计内行为——store 按 (type, at) 去重（presence.ts dedupKey）。
    const unsubscribePresence = eventStreamSupported
      ? subscribePresence(config.baseUrl)
      : () => {};

    // 订阅 SSE 伴随体事件通道 (主动涌现与反思通知). Reconciled from master.
    // G5 修复: 频道现为 legacy 文本行 + presence JSON 行共流 (契约 §5.1/§8.1) —
    // 先经 presence 分流: JSON 行进 presenceStore, 仅 legacy 文本行继续下行。
    // 波次 2：`[他说]` 行 = 他主动开口 → 进入对话流（规范 §5.3）；
    // 其余 legacy 行（如测试事件）→ 轻量 toast，不进对话。
    const unsubscribeEvents = !eventStreamSupported ? () => {} : subscribeCompanionEvents(config, (event) => {
      if (presenceStore.ingestLine(event.text) !== 'legacy') return;
      const text = event.text.trim();
      if (text.startsWith('[他说]')) {
        const said = text.slice('[他说]'.length).trim();
        if (said) {
          appendProactiveMessage(said);
          void triggerAutoScroll();
        }
        return;
      }
      legacyToast = text;
      window.setTimeout(() => {
        if (legacyToast === text) legacyToast = '';
      }, 12000);
    });

    // 后台健康轮询与审批请求同步 (真实 HTTP /health + capability manifest).
    const timer = window.setInterval(() => {
      void refreshConnection();
    }, 15000);

    return () => {
      window.clearInterval(timer);
      if (hourTimer !== null) window.clearInterval(hourTimer);
      unsubscribeEvents();
      unsubscribePresence();
    };
  });
</script>

<svelte:window onkeydown={handleChromeKey} />

<div
  class="app-root"
  class:busy
  class:theme-essence={isEssenceTheme}
  class:mode-focus={mode === 'focus'}
  class:mode-engineering={mode === 'engineering'}
  class:intro-playing={introPlaying}
>
  <div class="essence-scene" aria-hidden="true"></div>
  <div class="scene-underlay">
    <SceneLayer
      presence={$presenceStore.current}
      hour={timelineHour}
      interactive={!drawerSec && !showRuntimeModal && !openPanel}
      cameraIndex={sceneCamera}
      onBlackholeClick={handleBlackholeClick}
    />
    <div class="planet-xfade" class:layer-off={mode === 'focus'}>
      <PlanetLayer hour={timelineHour} />
    </div>
    <div class="layer-xfade" class:layer-off={mode !== 'companion'}>
      <BridgeLayer hour={timelineHour} />
    </div>
    <div class="layer-xfade" class:layer-off={mode !== 'engineering'}>
      <DeepCabinLayer hour={timelineHour} />
    </div>
  </div>

  <div id="presence" aria-hidden="true"></div>
  <div id="vignette" aria-hidden="true"></div>

  <div class="shell">
    <nav class="rail" aria-label="主导航">
      <div class="rail-brand" title="Apeireth">燧</div>
      <div class="rail-nav">
        <button
          class="rail-btn"
          class:active={!drawerSec}
          onclick={() => toggleRail('chat')}
          title="当前对话"
        >
          <MessageCircleMore size={17} class="shell-icon" />
          <span class="rail-label">对话</span>
        </button>
        <button
          class="rail-btn"
          class:active={drawerSec === 'history'}
          onclick={() => toggleRail('history')}
          title="历史"
        >
          <History size={17} class="shell-icon" />
          <span class="rail-label">历史</span>
        </button>
        <button
          class="rail-btn"
          class:active={drawerSec === 'memory'}
          onclick={() => toggleRail('memory')}
          title="认知 / 记忆"
        >
          <Layers3 size={17} class="shell-icon" />
          <span class="rail-label">记忆</span>
        </button>
        <button
          class="rail-btn"
          class:active={drawerSec === 'tools'}
          onclick={() => toggleRail('tools')}
          title="工具管理"
        >
          <Wrench size={17} class="shell-icon" />
          <span class="rail-label">工具</span>
        </button>
      </div>
      <div class="rail-foot">
        <div class="rail-sep"></div>
        <button
          class="rail-btn"
          class:active={drawerSec === 'status'}
          onclick={() => toggleRail('status')}
          title="系统状态"
        >
          <Activity size={17} class="shell-icon" />
          <span class="rail-label">状态</span>
        </button>
        <button
          class="rail-btn"
          class:active={drawerSec === 'logs'}
          onclick={() => toggleRail('logs')}
          title="日志"
        >
          <ScrollText size={17} class="shell-icon" />
          <span class="rail-label">日志</span>
        </button>
        <button
          class="rail-btn"
          class:active={drawerSec === 'settings'}
          onclick={() => toggleRail('settings')}
          title="设置"
        >
          <Settings size={17} class="shell-icon" />
          <span class="rail-label">设置</span>
        </button>
      </div>
    </nav>

    <div
      id="drawerScrim"
      class:on={drawerSec !== null}
      role="button"
      tabindex="-1"
      aria-label="关闭侧边面板"
      onclick={closeDrawer}
      onkeydown={(e) => {
        if (e.key === 'Enter' || e.key === ' ') closeDrawer();
      }}
    ></div>
    <div
      id="scrim"
      class:show={openPanel !== null}
      role="button"
      tabindex="-1"
      aria-label="关闭弹出层"
      onclick={closePanels}
      onkeydown={(e) => {
        if (e.key === 'Enter' || e.key === ' ') closePanels();
      }}
    ></div>

    <aside class="drawer" class:open={drawerSec !== null} aria-label="侧边面板">
      {#if drawerMeta}
        <div class="drawer-head">
          <div>
            <p class="eyebrow">{drawerMeta.eyebrow}</p>
            <h2>{drawerMeta.title}</h2>
            <p class="sub">{drawerMeta.sub}</p>
          </div>
          <div class="drawer-actions">
            {#if drawerMeta.action}
              <button class="quiet-btn" onclick={onDrawerAction}>{drawerMeta.action}</button>
            {/if}
            <button class="mini" onclick={closeDrawer} aria-label="关闭">
              <X size={15} class="shell-icon-sm" />
            </button>
          </div>
        </div>
      {/if}
      <div class="drawer-body" class:embed={drawerSec !== 'status' && drawerSec !== null}>
        {#if drawerSec === 'history'}
          <ConversationsView
            {conversations}
            activeId={activeId || ''}
            {config}
            {capabilities}
            onOpen={openConversation}
            onCreate={newConversation}
            onArchive={archiveConversation}
            onDelete={deleteConversation}
            onRename={(id, title) => updateConversation(id, {title})}
            onPin={(id) => {
              const conv = conversations.find((item) => item.id === id);
              if (conv) updateConversation(id, {pinned: !conv.pinned});
            }}
          />
        {:else if drawerSec === 'memory'}
          <MemoryView {config} {capabilities} />
        {:else if drawerSec === 'tools'}
          <ToolsView {config} {capabilities} />
        {:else if drawerSec === 'logs'}
          <ActivityView {config} {capabilities} />
        {:else if drawerSec === 'settings'}
          <SettingsView
            {config}
            onSave={(newCfg) => {
              config = newCfg;
              saveConfig(newCfg);
              agentRuntime = createAgentRuntime(newCfg);
              const nextTheme = resolveTheme(newCfg.theme, themeQuery);
              activeTheme = nextTheme;
              applyDocumentTheme(nextTheme);
              void refreshConnection();
            }}
            onClearLocalData={() => {
              conversations = [];
              activeId = null;
              persist();
            }}
          />
        {:else if drawerSec === 'status'}
          <div class="stats">
            <div class="stat">
              <div
                class="num"
                class:ok={overallLabel === '正常'}
                class:bad={overallLabel === '离线' || overallLabel === '异常'}
              >
                {overallLabel}
              </div>
              <div class="lbl">总体状态</div>
            </div>
            <div class="stat">
              <div class="num">
                {healthReport.latencyMs ?? '—'}{#if healthReport.latencyMs}<small> ms</small>{/if}
              </div>
              <div class="lbl">总延迟</div>
            </div>
            <div class="stat">
              <div class="num">{config.model}</div>
              <div class="lbl">活动模型</div>
            </div>
            <div class="stat">
              <div class="num">{healthReport.subsystems.length}</div>
              <div class="lbl">子系统</div>
            </div>
          </div>
          <h3 class="sec-title">子系统</h3>
          {#each healthReport.subsystems as sub (sub.key)}
            <div class="rowline">
              <span
                class="dot-st"
                class:ok={sub.status === 'ok'}
                class:warn={sub.status === 'degraded'}
                class:bad={sub.status === 'offline'}
              ></span>
              <span class="k">{sub.name}</span>
              <code>{sub.endpoint}</code>
              <span class="v">{sub.latencyMs != null ? `${sub.latencyMs} ms` : sub.detail || sub.status}</span>
            </div>
          {:else}
            <p class="wb-empty">尚未完成探测。点「深度诊断」查看详情。</p>
          {/each}
        {/if}
      </div>
    </aside>

    <div class="main">
      <button
        class="wb-toggle"
        class:on={wbOpen}
        title="工作台"
        aria-label="工作台"
        aria-pressed={wbOpen}
        onclick={() => toggleWb()}
      >
        <PanelRight size={14} class="shell-icon-sm" />
        <span>工作台</span>
      </button>
      <div
        class="scroll"
        id="chatScroll"
        bind:this={messagesContainer}
        onscroll={handleScroll}
        style:--presence-glow={presenceGlow.toFixed(3)}
      >
        {#if !flowItems.length}
          <section class="home col">
            <svg class="ember" viewBox="0 0 56 56" aria-hidden="true">
              <circle class="halo" cx="28" cy="30" r="19"></circle>
              <circle class="core" cx="28" cy="30" r="7"></circle>
              <path class="halo" d="M28 6v9"></path>
            </svg>
            <h1 class="ask">今天想干些什么？</h1>
            <p class="lede">与 Apeireth 交流你的想法、创意与工作。</p>
            <div class="sugs">
              {#if suggestions.length}
                {#each suggestions as item (item.id)}
                  <button class="sug" onclick={() => pickSuggestion(item)}>
                    <Sparkles size={13} class="shell-icon-sm" />
                    <span>{item.title}</span>
                    <span class="src">{item.src}</span>
                  </button>
                {/each}
              {:else}
                {#each quickPrompts as prompt}
                  <button class="sug" onclick={() => applyQuickPrompt(prompt)}>
                    <Plus size={13} class="shell-icon-sm" />
                    <span>{prompt}</span>
                    <span class="src">开始</span>
                  </button>
                {/each}
              {/if}
            </div>
            <p class="sug-note">若干个性化条目 · 由近期记忆与对话生成</p>
            <div class="orbar">或者</div>
            <div class="calls">
              <button
                class="call"
                onclick={openVoiceCall}
                title={capabilityAvailable(capabilities, 'voice.duplex') ? '开始语音通话' : '全双工语音服务尚未组装 (not_assembled)'}
              >
                <PhoneCall size={13} />
                语音通话
                {#if !capabilityAvailable(capabilities, 'voice.duplex')}
                  <span style="opacity: 0.6; font-size: 11px;">(未组装)</span>
                {/if}
              </button>
            </div>
          </section>
        {:else}
          <section class="col">
            <div class="chat-head">
              <div>
                <h2 class="chat-title">{activeConversation?.title || '新对话'}</h2>
                <div class="statusline">
                  <div class="persona-menu">
                    <button
                      class="persona-trigger"
                      onclick={() => (personaMenuOpen = !personaMenuOpen)}
                      title="切换伙伴身份"
                      aria-label="切换伙伴身份"
                      aria-expanded={personaMenuOpen}
                    >
                      <span>{activePersona?.name || '伙伴'}</span>
                      <ChevronDown size={12} />
                    </button>
                    {#if personaMenuOpen}
                      <div class="persona-pop" role="menu">
                        {#each personaList as p (p.id)}
                          <button
                            class="persona-item"
                            class:active={p.id === activePersona?.id}
                            role="menuitem"
                            onclick={() => setActivePersona(p.id)}
                          >
                            <span class="persona-item-name">{p.name}</span>
                            {#if p.model}
                              <span class="persona-item-model">{p.model}</span>
                            {/if}
                          </button>
                        {/each}
                      </div>
                    {/if}
                  </div>
                  <span class="mono-note" style="opacity:.4">·</span>
                  <span class="mono-note">{config.model}</span>
                  <span class="mono-note" style="opacity:.4">·</span>
                  <button class="mono-note live" onclick={() => (showRuntimeModal = true)}>{hdState}</button>
                  {#if $presenceStore.simulated}
                    <span class="sim-badge" title="presence 频道断连：当前为本机中性默认值">SIM</span>
                  {/if}
                </div>
              </div>
              <div style="display:flex;gap:8px">
                <button class="quiet-btn" onclick={newConversation}>
                  <Plus size={13} />
                  新对话
                </button>
              </div>
            </div>
            <div class="thread">
              {#each flowItems as item (item.id)}
                {#if item.kind === 'dust'}
                  <div class="stardust" role="status">
                    <span class="stardust-line"></span>
                    <span class="stardust-text">他想起了 {item.dust.found} 段记忆</span>
                    {#if item.dust.keywords.length}
                      <span class="stardust-keys">{item.dust.keywords.slice(0, 4).join(' · ')}</span>
                    {/if}
                    <span class="stardust-line"></span>
                  </div>
                {:else if item.message.role === 'user'}
                  <div class="row user">
                    <div class="user-card">
                      <MessageContent
                        message={item.message}
                        onRetry={(msgId) => retryAssistantMessage(msgId)}
                        onEditSave={(msgId, newText) => editUserMessageSave(msgId, newText)}
                        onEditAndRegenerate={(msgId, newText) => editUserMessageAndRegenerate(msgId, newText)}
                        onBranch={(msgId) => branchFromMessage(msgId)}
                      />
                    </div>
                  </div>
                {:else}
                  <div class="row">
                    <div class="ai-card">
                      <MessageContent
                        message={item.message}
                        onRetry={(msgId) => retryAssistantMessage(msgId)}
                        onEditSave={(msgId, newText) => editUserMessageSave(msgId, newText)}
                        onEditAndRegenerate={(msgId, newText) => editUserMessageAndRegenerate(msgId, newText)}
                        onBranch={(msgId) => branchFromMessage(msgId)}
                      />
                    </div>
                  </div>
                {/if}
              {/each}
              {#if error}
                <div class="error-banner" role="alert">
                  <AlertCircle size={14} />
                  <span>{error}</span>
                </div>
              {/if}
            </div>
          </section>
        {/if}
      </div>

      {#if showScrollBottomBtn}
        <button class="scroll-bottom-btn" onclick={() => scrollToBottom(true)} aria-label="回到底部">
          <ChevronDown size={16} />
          <span>回到底部</span>
        </button>
      {/if}

      <div class="dock">
        <div class="col dock-col">
          <div class="composer-row">
            <div class="composer">
              <div class="editor">
                <button class="round-btn" title="新对话" onclick={newConversation} aria-label="新对话">
                  <Plus size={16} />
                </button>
                <textarea
                  bind:value={draft}
                  rows="1"
                  placeholder="与 Apeireth 交流……"
                  disabled={busy}
                  oninput={handleComposerInput}
                  onkeydown={(event) => {
                    if (event.key === 'Enter' && !event.shiftKey) {
                      event.preventDefault();
                      void send();
                    }
                  }}
                ></textarea>
              </div>
            </div>

            <div class="composer-side">
              <div class="composer-caps" aria-label="模型与上下文">
                <div class="panel" class:show={openPanel === 'ctx'} id="panel-ctx" role="dialog" aria-label="上下文窗口">
                  <h2>上下文窗口</h2>
                  <div class="bar"><i style:width={`${ctxUsage.pct}%`}></i></div>
                  <div class="bar-head">
                    <b>{ctxUsage.pct}%</b>
                    <span>{ctxUsage.tokens} / {ctxUsage.cap}</span>
                  </div>
                  <div class="kv"><span class="dot"></span>用户消息<span class="v">{ctxUsage.tokens}</span></div>
                  <h2>本轮</h2>
                  <div class="kv"><span class="dot"></span>消息数<span class="v">{activeMessages.length}</span></div>
                  <div class="kv"><span class="dot"></span>模型<span class="v">{config.model}</span></div>
                </div>

                <div class="panel" class:show={openPanel === 'model'} id="panel-model" role="dialog" aria-label="模型选择器">
                  <h2>当前模型</h2>
                  <div class="cur">
                    <span class="provider">{modelLetter}</span>
                    <span>{config.model}</span>
                  </div>
                  <div class="search" style="margin:14px 0 4px">
                    <Search size={13} class="shell-icon-sm" />
                    <input placeholder="切换模型" bind:value={modelQuery} />
                  </div>
                  {#if modelsLoading}
                    <p class="wb-empty">正在拉取模型列表…</p>
                  {:else if filteredModels.length}
                    {#each filteredModels as id (id)}
                      <button
                        class="model"
                        aria-current={id === config.model ? 'true' : undefined}
                        onclick={() => selectModel(id)}
                      >
                        {id}
                      </button>
                    {/each}
                  {:else}
                    <p class="wb-empty">暂无可用模型。可在设置中配置提供商。</p>
                  {/if}
                </div>

                <button
                  class="cap-btn pill-model"
                  aria-expanded={openPanel === 'model'}
                  onclick={() => togglePanel('model')}
                  title={`模型：${config.model}`}
                  aria-label={`切换模型：${config.model}`}
                >
                  <span class="provider">{modelLetter}</span>
                </button>
                <span class="cap-sep" aria-hidden="true"></span>
                <button
                  class="cap-btn pill-ctx"
                  aria-expanded={openPanel === 'ctx'}
                  title={`上下文窗口 ${ctxUsage.pct}%`}
                  aria-label={`上下文窗口 ${ctxUsage.pct}%`}
                  onclick={() => togglePanel('ctx')}
                >
                  <svg class="ctx-ring" viewBox="0 0 24 24" aria-hidden="true">
                    <circle class="bg" cx="12" cy="12" r="9"></circle>
                    <circle
                      class="fg"
                      cx="12"
                      cy="12"
                      r="9"
                      stroke-dasharray={ctxUsage.dasharray}
                      stroke-dashoffset={ctxUsage.dashoffset}
                    ></circle>
                  </svg>
                </button>
              </div>

              <div class="composer-send">
                {#if busy}
                  <button class="send stop" onclick={stop} aria-label="中断">
                    <Square size={14} />
                  </button>
                {:else}
                  <button
                    class="send"
                    onclick={() => send()}
                    disabled={!draft.trim() || healthState === 'offline'}
                    aria-label="发送"
                  >
                    <ArrowUp size={16} />
                  </button>
                {/if}
              </div>
            </div>
          </div>
          <p class="hint">ENTER 发送 · SHIFT+ENTER 换行</p>
        </div>
      </div>
    </div>

    <Workbench
      conversation={activeConversation}
      {busy}
      closed={!wbOpen}
      onClose={() => toggleWb(false)}
    />
  </div>

  {#if !isEssenceTheme}
    <nav class="mode-switch" aria-label="模式切换">
      {#each modes as item (item.id)}
        <button
          class="mode-btn"
          class:active={mode === item.id}
          onclick={() => setMode(item.id)}
          title={item.label}
          aria-label={item.label}
          aria-current={mode === item.id ? 'page' : undefined}
        >
          <item.icon size={16} />
        </button>
      {/each}
    </nav>
  {/if}

  <button class="focus-exit" onclick={() => setMode('companion')}>返回舰桥</button>

  {#if legacyToast}
    <div class="legacy-toast" role="status">
      <Sparkles size={12} />
      <span>{legacyToast}</span>
    </div>
  {/if}

  {#if introPlaying}
    <IntroLayer onComplete={handleIntroComplete} />
  {/if}
</div>

<VoiceCallModal
  isOpen={showVoiceCall}
  padState={{
    pleasure: (($presenceStore.current?.p ?? 0.4) + 1) / 2,
    arousal: (($presenceStore.current?.a ?? -0.2) + 1) / 2,
    dominance: (($presenceStore.current?.d ?? 0.2) + 1) / 2,
  }}
  onClose={() => (showVoiceCall = false)}
  onSendMessage={handleVoiceMessage}
/>

<ConfirmDialog
  open={pendingCanonical !== null}
  title="需要批准"
  message={pendingCanonical ? `${pendingCanonical.tool_name}：${pendingCanonical.governance_reason}` : ''}
  confirmText={approvalBusy ? '处理中…' : '批准'}
  cancelText="拒绝"
  onConfirm={() => void resolvePending('approve')}
  onCancel={() => void resolvePending('reject')}
/>

<RuntimeModal
  open={showRuntimeModal}
  report={healthReport}
  {capabilities}
  isRefreshing={isRefreshingHealth}
  onClose={() => (showRuntimeModal = false)}
  onRefresh={refreshConnection}
/>

<style>
  .app-root {
    position: relative;
    height: 100vh;
    overflow: hidden;
    background: var(--ap-space-void, #07070c);
    color: var(--ap-bone);
  }
  .scene-underlay {
    position: absolute;
    inset: 0;
    z-index: 0;
  }
  .layer-xfade {
    position: absolute;
    inset: 0;
    z-index: 1;
    pointer-events: none;
    transition: opacity 0.8s ease;
  }
  .layer-xfade.layer-off {
    opacity: 0;
  }
  .app-root.mode-focus .layer-xfade {
    transition-duration: 0.6s;
  }
  .planet-xfade {
    pointer-events: none;
    transition: opacity 0.8s ease;
  }
  .planet-xfade.layer-off {
    opacity: 0;
  }
  .app-root.mode-focus .planet-xfade {
    transition-duration: 0.6s;
  }
  .app-root.mode-focus .shell,
  .app-root.intro-playing .shell,
  .app-root.mode-focus #presence,
  .app-root.intro-playing #presence,
  .app-root.mode-focus #vignette,
  .app-root.intro-playing #vignette {
    opacity: 0;
    pointer-events: none;
  }
  .app-root.mode-focus .shell,
  .app-root.intro-playing .shell {
    transition: opacity 0.6s ease;
  }
  .focus-exit {
    position: absolute;
    left: 50%;
    bottom: 28px;
    transform: translateX(-50%);
    z-index: 6;
    display: none;
    padding: 7px 18px;
    border-radius: 999px;
    border: 1px solid rgba(255, 210, 122, 0.45);
    background: rgba(7, 7, 12, 0.72);
    color: var(--ap-gold);
    font-size: 12px;
    letter-spacing: 0.16em;
    cursor: pointer;
    pointer-events: auto;
  }
  .app-root.mode-focus .focus-exit {
    display: inline-flex;
  }
  .legacy-toast {
    position: absolute;
    left: 50%;
    bottom: 98px;
    transform: translateX(-50%);
    z-index: 8;
    display: flex;
    align-items: center;
    gap: 8px;
    max-width: min(560px, 80vw);
    padding: 7px 16px;
    border-radius: 999px;
    background: var(--ap-panel);
    border: 1px solid var(--ap-line);
    backdrop-filter: blur(12px);
    color: rgba(232, 224, 204, 0.75);
    font-size: 11px;
    letter-spacing: 0.06em;
    pointer-events: auto;
  }
  .legacy-toast :global(svg) {
    color: var(--ap-gold);
    flex: none;
  }
  .drawer-body code {
    font-family: var(--ap-font-mono);
    font-size: 10.5px;
    color: var(--ap-bone-68);
    background: rgba(0, 0, 0, 0.3);
    padding: 1px 5px;
    border-radius: 3px;
  }
  .search {
    display: flex;
    align-items: center;
    gap: 8px;
    border: 1px solid var(--ap-line);
    border-radius: 5px;
    padding: 7px 11px;
    color: var(--ap-bone-30);
  }
  .search input {
    flex: 1;
    border: 0;
    outline: 0;
    background: transparent;
    font-size: 12px;
    color: var(--ap-bone);
  }
</style>
