<script lang="ts">
  import {ChevronRight, FileText} from 'lucide-svelte';
  import type {Conversation} from '../types';

  let {
    conversation = null,
    busy = false,
    closed = false,
    onClose,
  }: {
    conversation: Conversation | null;
    busy?: boolean;
    closed?: boolean;
    onClose: () => void;
  } = $props();

  let open = $state<Record<string, boolean>>({
    goal: true,
    tools: true,
    files: true,
    mem: true,
  });

  const lastAssistant = $derived(
    conversation?.messages.filter((m) => m.role === 'assistant').at(-1) ?? null,
  );
  const toolCalls = $derived(lastAssistant?.toolCalls ?? []);
  const provenance = $derived(lastAssistant?.provenance);
  const memories = $derived(provenance?.memories ?? []);
  const runningTools = $derived(toolCalls.filter((t) => t.status === 'pending' || t.status === 'running'));
  const doneTools = $derived(toolCalls.filter((t) => t.status === 'succeeded'));
  const blockCount = $derived(
    (conversation ? 1 : 0) + (toolCalls.length ? 1 : 0) + (memories.length ? 1 : 0),
  );

  function toggle(id: string): void {
    open = {...open, [id]: !open[id]};
  }

  function toolLabel(status: string): string {
    if (status === 'running' || status === 'pending') return '运行中';
    if (status === 'succeeded') return '完成';
    if (status === 'failed') return '失败';
    return status;
  }
</script>

<aside class="wb" class:closed aria-label="工作台">
  <div class="wb-head">
    <h2>工作台</h2>
    <span class="count">{blockCount} 区块</span>
    <button class="mini" onclick={onClose} aria-label="收起">
      <ChevronRight size={13} class="shell-icon-sm" />
    </button>
  </div>
  <div class="wb-body">
    {#if !conversation}
      <p class="wb-empty">开始对话后，目标、工具轨迹与本轮记忆会显示在这里。</p>
    {:else}
      <div class="blk">
        <button
          class="blk-head"
          aria-expanded={open.goal}
          onclick={() => toggle('goal')}
        >
          目标
          <ChevronRight size={13} class="shell-icon-sm caret" />
        </button>
        {#if open.goal}
          <div class="blk-body show">
            <p class="goal">{conversation.title || '新对话'}</p>
          </div>
        {/if}
      </div>

      <div class="blk">
        <button
          class="blk-head"
          aria-expanded={open.tools}
          onclick={() => toggle('tools')}
        >
          代理与执行
          {#if toolCalls.length}
            <span class="tagn">{runningTools.length ? `${runningTools.length} 运行中` : `${toolCalls.length} 项工具`}</span>
          {/if}
          <ChevronRight size={13} class="shell-icon-sm caret" />
        </button>
        {#if open.tools}
          <div class="blk-body show">
            <button class="agent" type="button">
              <span class="st" class:run={busy}></span>
              主 Agent
              <span class="tag">{busy ? '运行中' : '空闲'}</span>
            </button>
            {#if toolCalls.length}
              {#each toolCalls as tool (tool.id)}
                <button class="agent" type="button">
                  <span class="st" class:run={tool.status === 'running' || tool.status === 'pending'}></span>
                  {tool.name}
                  <span class="tag">{toolLabel(tool.status)}</span>
                </button>
              {/each}
            {:else}
              <p class="wb-empty">本轮尚无工具调用。</p>
            {/if}
          </div>
        {/if}
      </div>

      {#if doneTools.length || lastAssistant?.toolCalls?.length}
        <div class="blk">
          <button
            class="blk-head"
            aria-expanded={open.files}
            onclick={() => toggle('files')}
          >
            引用的工具
            <span class="tagn">{toolCalls.length}</span>
            <ChevronRight size={13} class="shell-icon-sm caret" />
          </button>
          {#if open.files}
            <div class="blk-body show">
              {#each toolCalls as tool (tool.id)}
                <button class="file" type="button">
                  <FileText size={13} class="shell-icon-sm" />
                  {tool.name}
                </button>
              {/each}
            </div>
          {/if}
        </div>
      {/if}

      <div class="blk">
        <button
          class="blk-head"
          aria-expanded={open.mem}
          onclick={() => toggle('mem')}
        >
          本轮记忆
          {#if memories.length}
            <span class="tagn">{memories.length}</span>
          {:else if provenance?.count}
            <span class="tagn">{provenance.count}</span>
          {/if}
          <ChevronRight size={13} class="shell-icon-sm caret" />
        </button>
        {#if open.mem}
          <div class="blk-body show">
            {#if memories.length}
              {#each memories as mem, i (i)}
                <div class="mem"><span class="sp">✦</span><span>{mem}</span></div>
              {/each}
            {:else if provenance?.count}
              <div class="mem">
                <span class="sp">✦</span>
                <span>他想起了 {provenance.count} 段记忆</span>
              </div>
            {:else}
              <p class="wb-empty">本轮尚未召回记忆。</p>
            {/if}
          </div>
        {/if}
      </div>
    {/if}
  </div>
</aside>
