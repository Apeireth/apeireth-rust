<script lang="ts">
  import { onMount, onDestroy } from 'svelte';
  import { voiceCallManager, type VoiceState } from '../lib/voice';
  import { Mic, MicOff, PhoneOff, Sparkles, Volume2 } from 'lucide-svelte';

  export let isOpen = false;
  export let padState = { pleasure: 0.7, arousal: 0.4, dominance: 0.6 };
  export let onClose = () => {};
  export let onSendMessage: (msg: string) => Promise<string> = async () => '';

  let state: VoiceState = voiceCallManager.state;
  let canvas: HTMLCanvasElement | null = null;
  let animId: number = 0;
  let unsubscribe: (() => void) | null = null;

  onMount(() => {
    unsubscribe = voiceCallManager.subscribe((s) => {
      state = s;
    });

    voiceCallManager.setOnUserSpeechEnd(async (text) => {
      if (!text.trim()) return;
      const reply = await onSendMessage(text);
      if (reply) {
        voiceCallManager.speak(reply);
      }
    });

    startVisualizer();
  });

  onDestroy(() => {
    if (unsubscribe) unsubscribe();
    cancelAnimationFrame(animId);
    voiceCallManager.endCall();
  });

  function startVisualizer() {
    if (!canvas) return;
    const ctx = canvas.getContext('2d');
    if (!ctx) return;

    let angle = 0;

    const render = () => {
      if (!canvas || !ctx) return;
      const width = canvas.width;
      const height = canvas.height;
      const cx = width / 2;
      const cy = height / 2;

      ctx.clearRect(0, 0, width, height);

      // Base radius modified by volume and arousal
      const vol = state.audioVolume;
      const baseRadius = 60 + vol * 120 + padState.arousal * 20;

      // Dynamic color gradient based on PAD
      const r = Math.round(180 + padState.pleasure * 75);
      const g = Math.round(140 + padState.pleasure * 60);
      const b = Math.round(220 + padState.dominance * 35);

      // Outer glow
      const grad = ctx.createRadialGradient(cx, cy, baseRadius * 0.4, cx, cy, baseRadius * 1.6);
      grad.addColorStop(0, `rgba(${r}, ${g}, ${b}, ${state.isAssistantSpeaking ? 0.9 : 0.6})`);
      grad.addColorStop(0.6, `rgba(${r - 40}, ${g + 20}, ${b}, 0.3)`);
      grad.addColorStop(1, 'rgba(10, 15, 30, 0)');

      ctx.beginPath();
      ctx.arc(cx, cy, baseRadius * 1.6, 0, Math.PI * 2);
      ctx.fillStyle = grad;
      ctx.fill();

      // Pulsing organic rings
      angle += 0.03 + vol * 0.1;
      ctx.strokeStyle = `rgba(${r + 30}, ${g + 50}, ${b + 30}, 0.7)`;
      ctx.lineWidth = 3;
      ctx.beginPath();
      for (let i = 0; i <= 360; i += 6) {
        const rad = (i * Math.PI) / 180;
        const wave = Math.sin(rad * 6 + angle) * (10 + vol * 30);
        const currentR = baseRadius + wave;
        const x = cx + Math.cos(rad) * currentR;
        const y = cy + Math.sin(rad) * currentR;
        if (i === 0) ctx.moveTo(x, y);
        else ctx.lineTo(x, y);
      }
      ctx.closePath();
      ctx.stroke();

      animId = requestAnimationFrame(render);
    };
    render();
  }

  function handleToggleMute() {
    voiceCallManager.toggleMute();
  }

  function handleEndCall() {
    voiceCallManager.endCall();
    onClose();
  }
</script>

{#if isOpen}
  <div class="fixed inset-0 z-50 flex items-center justify-center bg-black/80 backdrop-blur-md p-4 animate-fade-in">
    <div class="relative w-full max-w-lg overflow-hidden rounded-3xl border border-indigo-500/30 bg-slate-950/90 shadow-2xl p-6 text-center text-white">
      <!-- Top header -->
      <div class="flex items-center justify-between border-b border-white/10 pb-4 mb-4">
        <div class="flex items-center space-x-2">
          <div class="flex h-8 w-8 items-center justify-center rounded-full bg-indigo-500/20 text-indigo-400">
            <Sparkles class="h-4 w-4 animate-pulse" />
          </div>
          <div class="text-left">
            <h3 class="text-base font-semibold">Apeireth 全双工语音通话</h3>
            <p class="text-xs text-slate-400">{state.statusText}</p>
          </div>
        </div>
        <span class="inline-flex items-center rounded-full bg-amber-500/10 px-2.5 py-0.5 text-xs font-medium text-amber-400 border border-amber-500/20">
          ● 未组装 (not_assembled)
        </span>
      </div>

      <!-- Not Assembled Degradation Notice -->
      <div class="my-3 rounded-xl bg-amber-500/10 border border-amber-500/20 p-3 text-left">
        <p class="text-xs text-amber-200 font-medium">全双工实时语音流服务尚未组装</p>
        <p class="text-xs text-slate-400 mt-1">当前发行版运行时专注于微内核、行为链安全 Guard 与统一记忆架构。全双工语音服务已在 Capability Manifest 中如实标记为 <code>not_assembled</code>。</p>
      </div>

      <!-- Center Audio Visualizer Canvas -->
      <div class="relative flex h-64 w-full items-center justify-center">
        <canvas bind:this={canvas} width="320" height="256" class="w-full h-full"></canvas>
        <div class="absolute flex flex-col items-center pointer-events-none">
          {#if state.isAssistantSpeaking}
            <Volume2 class="h-8 w-8 text-indigo-300 animate-bounce" />
            <span class="text-xs text-indigo-200 mt-1 font-mono">SPEAKING</span>
          {:else if state.isUserSpeaking}
            <Mic class="h-8 w-8 text-amber-300 animate-pulse" />
            <span class="text-xs text-amber-200 mt-1 font-mono">LISTENING</span>
          {:else}
            <div class="h-4 w-4 rounded-full bg-white/20 animate-ping"></div>
          {/if}
        </div>
      </div>

      <!-- Live Transcript -->
      <div class="my-4 min-h-[48px] rounded-xl bg-white/5 p-3 text-sm text-slate-300 border border-white/5">
        {#if state.transcript}
          <p class="italic">"{state.transcript}"</p>
        {:else}
          <p class="text-slate-500">语音流服务暂未组装，请使用文本对话与工作台执行。</p>
        {/if}
      </div>

      <!-- Control Buttons -->
      <div class="flex items-center justify-center space-x-6 pt-2">
        <button
          on:click={handleToggleMute}
          class="flex h-14 w-14 items-center justify-center rounded-full border transition duration-200 {state.isMuted ? 'bg-red-500/20 border-red-500 text-red-400' : 'bg-slate-800 border-white/20 hover:bg-slate-700 text-white'}"
          title={state.isMuted ? '取消静音' : '静音麦克风'}
        >
          {#if state.isMuted}
            <MicOff class="h-6 w-6" />
          {:else}
            <Mic class="h-6 w-6" />
          {/if}
        </button>

        <button
          on:click={handleEndCall}
          class="flex h-16 w-16 items-center justify-center rounded-full bg-red-600 border border-red-500/40 text-white shadow-lg hover:bg-red-500 hover:scale-105 transition duration-200"
          title="挂断通话"
        >
          <PhoneOff class="h-7 w-7" />
        </button>
      </div>
    </div>
  </div>
{/if}

<style>
  @keyframes fadeIn {
    from { opacity: 0; transform: scale(0.95); }
    to { opacity: 1; transform: scale(1); }
  }
  .animate-fade-in {
    animation: fadeIn 0.2s cubic-bezier(0.16, 1, 0.3, 1) forwards;
  }
</style>
