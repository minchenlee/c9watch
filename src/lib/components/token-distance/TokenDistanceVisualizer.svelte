<script lang="ts">
	import { onMount, onDestroy, tick } from 'svelte';
	import { invoke } from '$lib/data/commands';
	import { MILESTONES, tokensToHeight, formatHeight, getCurrentMilestone } from './milestones';

	let { totalTokens, dateRange, onclose }: { totalTokens: number; dateRange: string; onclose: () => void } = $props();

	let canvas = $state<HTMLCanvasElement | null>(null);
	let animationDone = $state(false);
	let showIntro = $state(true);
	let introVisible = $state(false);
	let currentTokens = $state(0);
	let animFrameId: number | null = null;


	// ── Constants ────────────────────────────────────────────────
	const GRAIN_SIZE = 3;          // px size of each grain dot at 1:1 zoom
	const GRAINS_PER_ROW = 12;     // grains per row in the tower
	const TOWER_WIDTH_PX = GRAINS_PER_ROW * GRAIN_SIZE; // tower width at 1:1

	// Scale: tokens per grain. Target ~3000 grains for good animation density.
	// totalTokens is a prop that doesn't change after mount, so these are stable.
	// eslint-disable-next-line -- intentional snapshot of prop value
	const _tokens = $derived(totalTokens);
	const scale = $derived(Math.max(1, Math.ceil(_tokens / 3000)));
	const totalGrains = $derived(Math.ceil(_tokens / scale));
	const totalGrainRows = $derived(Math.ceil(totalGrains / GRAINS_PER_ROW));
	// Full tower height at 1:1 zoom
	const towerHeightPx = $derived(totalGrainRows * GRAIN_SIZE);

	// Colors
	const AMBER = '#ff6600';
	const AMBER_DIM = 'rgba(255, 102, 0, 0.3)';
	const TEXT_PRIMARY = '#ffffff';
	const TEXT_MUTED = '#666666';
	const BORDER = '#333333';
	const BG = '#000000';

	// Font families matching CSS design tokens
	const FONT_PIXEL = "'Geist Pixel Square', 'Geist Mono', 'SF Mono', Monaco, monospace";
	const FONT_MONO = "'Geist Mono', 'SF Mono', Monaco, monospace";

	// ── Formatting ───────────────────────────────────────────────
	function formatTokenCount(n: number): string {
		if (n >= 1_000_000_000) return (n / 1_000_000_000).toFixed(1) + 'B';
		if (n >= 1_000_000) return (n / 1_000_000).toFixed(1) + 'M';
		if (n >= 1_000) return (n / 1_000).toFixed(1) + 'K';
		return n.toString();
	}

	// ── Canvas rendering ─────────────────────────────────────────
	// followTop: 0 = anchored at bottom, 1 = camera follows top of stack
	function render(ctx: CanvasRenderingContext2D, w: number, h: number, grainsFilled: number, zoom: number, followTop: number, dpr = window.devicePixelRatio || 1) {
		ctx.setTransform(dpr, 0, 0, dpr, 0, 0);

		// Black background (so PNG export isn't transparent)
		ctx.fillStyle = BG;
		ctx.fillRect(0, 0, w, h);

		const filledRows = Math.ceil(grainsFilled / GRAINS_PER_ROW);

		const scaledGrainSize = GRAIN_SIZE * zoom;
		const scaledTowerWidth = TOWER_WIDTH_PX * zoom;

		// Tower base X: centered horizontally
		const towerBaseX = (w - scaledTowerWidth) / 2;

		// Camera Y: blend between "follow top" and "anchored at bottom"
		// followTop=1: top of stack stays at 30% from canvas top
		// followTop=0: tower base at canvas bottom (settled final view)
		const anchoredBaseY = h - 40;
		let followBaseY = anchoredBaseY;
		if (filledRows > 0) {
			const topTargetY = h * 0.3;
			followBaseY = topTargetY + filledRows * scaledGrainSize;
		}
		const towerBaseY = anchoredBaseY + (followBaseY - anchoredBaseY) * followTop;

		// Draw ground line
		ctx.strokeStyle = BORDER;
		ctx.lineWidth = 1;
		ctx.beginPath();
		ctx.moveTo(towerBaseX - 10, towerBaseY);
		ctx.lineTo(towerBaseX + scaledTowerWidth + 10, towerBaseY);
		ctx.stroke();

		// Draw grains from bottom to top
		for (let i = 0; i < grainsFilled; i++) {
			const row = Math.floor(i / GRAINS_PER_ROW);
			const col = i % GRAINS_PER_ROW;

			const x = towerBaseX + col * scaledGrainSize;
			const y = towerBaseY - (row + 1) * scaledGrainSize;

			// Don't draw if above canvas
			if (y < -scaledGrainSize) continue;
			// Don't draw if below canvas
			if (y > h) continue;

			// Grain color: slightly vary brightness for texture
			const brightness = 0.8 + (((i * 7) % 13) / 13) * 0.2;
			ctx.fillStyle = `rgba(255, ${Math.round(102 * brightness)}, 0, ${0.7 + brightness * 0.3})`;

			// Draw grain as a small rect with 1px gap for visibility when zoomed in
			const gap = zoom > 2 ? 1 : 0;
			ctx.fillRect(x + gap, y + gap, scaledGrainSize - gap * 2, scaledGrainSize - gap * 2);
		}

		// Draw milestone markers on the right side of the tower
		const passedMilestones = MILESTONES.filter(m => currentTokens >= m.tokens);
		for (const m of passedMilestones) {
			const mGrainRow = Math.ceil(m.tokens / scale / GRAINS_PER_ROW);
			const markerY = towerBaseY - mGrainRow * scaledGrainSize;

			// Only draw if visible
			if (markerY < -20 || markerY > h + 20) continue;

			// Marker line
			const lineStartX = towerBaseX + scaledTowerWidth + 6;
			ctx.strokeStyle = TEXT_MUTED;
			ctx.lineWidth = 1;
			ctx.beginPath();
			ctx.moveTo(lineStartX, markerY);
			ctx.lineTo(lineStartX + 12, markerY);
			ctx.stroke();

			// Label
			const fontSize = Math.max(9, Math.min(12, 10 / Math.sqrt(1 / zoom)));
			ctx.font = `${fontSize}px ${FONT_MONO}`;
			ctx.fillStyle = TEXT_PRIMARY;
			ctx.textBaseline = 'middle';
			ctx.fillText(m.label, lineStartX + 16, markerY);

			// Height
			const heightStr = formatHeight(tokensToHeight(m.tokens));
			const labelWidth = ctx.measureText(m.label).width;
			ctx.fillStyle = TEXT_MUTED;
			ctx.font = `${Math.max(8, fontSize - 2)}px ${FONT_MONO}`;
			ctx.fillText(heightStr, lineStartX + 16 + labelWidth + 8, markerY);
		}

		// Draw stats on the left side of the tower
		const statsX = towerBaseX - 20;
		const topOfStack = towerBaseY - filledRows * scaledGrainSize;
		// Position stats near the top of the stack, clamped to visible area
		const statsY = Math.max(30, Math.min(topOfStack, h - 80));

		// Token count
		ctx.textAlign = 'right';
		ctx.textBaseline = 'middle';
		ctx.font = `bold 28px ${FONT_PIXEL}`;
		ctx.fillStyle = AMBER;
		ctx.fillText(formatTokenCount(currentTokens), statsX, statsY);

		// "tokens" label
		ctx.font = `10px ${FONT_MONO}`;
		ctx.fillStyle = TEXT_MUTED;
		ctx.fillText('TOKENS', statsX, statsY + 18);

		// Height
		ctx.font = `bold 18px ${FONT_PIXEL}`;
		ctx.fillStyle = TEXT_PRIMARY;
		ctx.fillText(formatHeight(tokensToHeight(currentTokens)), statsX, statsY + 42);

		// "rice stack" label
		ctx.font = `10px ${FONT_MONO}`;
		ctx.fillStyle = TEXT_MUTED;
		ctx.fillText('RICE STACK', statsX, statsY + 56);

		// Current milestone
		const currentMilestone = getCurrentMilestone(currentTokens);
		if (currentMilestone) {
			ctx.font = `bold 12px ${FONT_PIXEL}`;
			ctx.fillStyle = AMBER;
			ctx.fillText(`PAST ${currentMilestone.label.toUpperCase()}!`, statsX, statsY + 78);
		}

		ctx.textAlign = 'left'; // reset
	}

	// ── Animation ────────────────────────────────────────────────
	function startAnimation() {
		if (totalTokens === 0 || !canvas) {
			animationDone = true;
			return;
		}

		const c = canvas;
		const ctx = c.getContext('2d')!;

		const dpr = window.devicePixelRatio || 1;
		const rect = c.getBoundingClientRect();
		const w = rect.width;
		const h = rect.height;
		c.width = w * dpr;
		c.height = h * dpr;

		const duration = 6000;
		const startTime = performance.now();

		// Zoom range: start zoomed in (big grains), end zoomed out to fit tower
		// At start: we want ~8 rows visible → zoom = canvasHeight / (8 * GRAIN_SIZE)
		const zoomStart = Math.min(h / (8 * GRAIN_SIZE), 20);
		// At end: fit full tower with padding → zoom = (canvasHeight - 80) / towerHeightPx
		const zoomEnd = Math.min((h - 80) / Math.max(towerHeightPx, 1), zoomStart);

		let lastMilestoneLabel: string | null = null;
		let lastMilestoneIdx = -1;
		let milestoneFlashUntil = 0;

		function tick(now: number) {
			const elapsed = now - startTime;
			const t = Math.min(elapsed / duration, 1);

			// 3-phase easing: slow → fast → slow
			let eased: number;
			if (t < 0.08) {
				const local = t / 0.08;
				eased = 0.01 * (local * local);
			} else if (t < 0.55) {
				const local = (t - 0.08) / 0.47;
				eased = 0.01 + 0.69 * local;
			} else {
				const local = (t - 0.55) / 0.45;
				eased = 0.7 + 0.3 * (1 - Math.pow(1 - local, 3));
			}

			const grainsFilled = Math.round(eased * totalGrains);
			currentTokens = Math.round(eased * totalTokens);

			// Zoom: interpolate from zoomStart to zoomEnd
			// Use pow(eased, 0.35) for aggressive early zoom-out
			const zoomProgress = Math.pow(eased, 0.35);
			const zoom = zoomStart + (zoomEnd - zoomStart) * zoomProgress;

			// followTop: 1 during stacking, ease to 0 in the last 15% for smooth settle
			const settleT = t > 0.85 ? (t - 0.85) / 0.15 : 0;
			const follow = 1 - settleT * settleT; // ease-in settle

			// Render
			render(ctx, w, h, grainsFilled, zoom, follow);

			// Check milestones
			const currentIdx = MILESTONES.findLastIndex(m => currentTokens >= m.tokens);
			if (currentIdx > lastMilestoneIdx && currentIdx >= 0) {
				lastMilestoneIdx = currentIdx;
				lastMilestoneLabel = MILESTONES[currentIdx].label;
				milestoneFlashUntil = now + 800;
			}

			// Draw milestone flash overlay text
			if (lastMilestoneLabel && now < milestoneFlashUntil) {
				const flashAlpha = (milestoneFlashUntil - now) / 800;
				const flashScale = 1 + (1 - flashAlpha) * 0.3;
				ctx.save();
				ctx.font = `bold 16px ${FONT_PIXEL}`;
				ctx.fillStyle = `rgba(255, 102, 0, ${flashAlpha})`;
				ctx.textAlign = 'center';
				ctx.textBaseline = 'top';
				ctx.translate(w / 2, 60);
				ctx.scale(flashScale, flashScale);
				ctx.fillText(lastMilestoneLabel.toUpperCase(), 0, 0);
				ctx.restore();
			}

			if (t < 1) {
				animFrameId = requestAnimationFrame(tick);
			} else {
				// Final frame — settled at bottom
				currentTokens = totalTokens;
				render(ctx, w, h, totalGrains, zoomEnd, 0);
				animationDone = true;
			}
		}

		animFrameId = requestAnimationFrame(tick);
	}

	async function skipToFinal() {
		if (animFrameId) cancelAnimationFrame(animFrameId);
		if (introTimeoutId) { clearTimeout(introTimeoutId); introTimeoutId = null; }
		if (introFadeId) { clearTimeout(introFadeId); introFadeId = null; }
		currentTokens = totalTokens;
		animationDone = true;

		// If still showing intro, switch to canvas and wait for mount
		if (showIntro) {
			showIntro = false;
			await tick();
		}

		if (!canvas) return;
		const c = canvas;
		const ctx = c.getContext('2d')!;

		const dpr = window.devicePixelRatio || 1;
		const rect = c.getBoundingClientRect();
		const w = rect.width;
		const h = rect.height;
		c.width = w * dpr;
		c.height = h * dpr;

		const zoomEnd = Math.min((h - 80) / Math.max(towerHeightPx, 1), 20);
		render(ctx, w, h, totalGrains, zoomEnd, 0);
	}

	function handleKeydown(e: KeyboardEvent) {
		if (e.key === 'Escape') {
			if (animationDone) onclose();
			else skipToFinal();
		}
		if (e.key === ' ') {
			e.preventDefault();
			if (!animationDone) skipToFinal();
		}
	}

	let sharing = $state(false);

	function renderShareImage(): string {
		// Render at 2x resolution (2160x2700 pixels, 1080x1350 logical, 4:5 ratio)
		const shareDpr = 2;
		const shareW = 1080;
		const shareH = 1350;
		const offscreen = document.createElement('canvas');
		offscreen.width = shareW * shareDpr;
		offscreen.height = shareH * shareDpr;
		const ctx = offscreen.getContext('2d')!;

		// Render the tower with dpr=2 so grains and text are crisp
		const shareTowerHeight = totalGrainRows * GRAIN_SIZE;
		const shareZoom = Math.min((shareH - 200) / Math.max(shareTowerHeight, 1), 20);
		render(ctx, shareW, shareH, totalGrains, shareZoom, 0, shareDpr);

		// Add title at top
		ctx.font = `bold 28px ${FONT_PIXEL}`;
		ctx.fillStyle = AMBER;
		ctx.textAlign = 'center';
		ctx.textBaseline = 'top';
		ctx.fillText('YOUR TOKEN JOURNEY', shareW / 2, 30);

		// Date range under title
		let nextY = 66;
		if (dateRange) {
			ctx.font = `14px ${FONT_MONO}`;
			ctx.fillStyle = TEXT_MUTED;
			ctx.fillText(dateRange, shareW / 2, nextY);
			nextY += 22;
		}

		// Explain text
		ctx.font = `11px ${FONT_MONO}`;
		ctx.fillStyle = TEXT_MUTED;
		ctx.globalAlpha = 0.7;
		ctx.fillText('1 TOKEN = 1 GRAIN OF RICE', shareW / 2, nextY);
		ctx.globalAlpha = 1;
		nextY += 20;

		// Watermark under date range (export only)
		ctx.font = `12px ${FONT_MONO}`;
		ctx.fillStyle = TEXT_MUTED;
		ctx.fillText('generated with c9watch', shareW / 2, nextY);

		return offscreen.toDataURL('image/png');
	}

	async function shareImage(e: MouseEvent) {
		e.stopPropagation(); // don't close overlay
		if (!canvas || sharing) return;
		// Capture button rect synchronously — currentTarget is null after await
		const btn = e.currentTarget as HTMLElement;
		const rect = btn.getBoundingClientRect();
		sharing = true;
		try {
			const dataUrl = renderShareImage();
			const filePath = await invoke<string>('save_temp_image', { data: dataUrl });
			const { shareFile } = await import('$lib/data/platform');
			await shareFile(filePath, {
				mimeType: 'image/png',
				title: 'My Token Journey',
				position: {
					x: Math.round(rect.left + rect.width / 2),
					y: Math.round(rect.top),
					preferredEdge: 'top',
				},
			});
		} catch (err) {
			console.error('Share failed:', err);
		} finally {
			sharing = false;
		}
	}

	let introTimeoutId: ReturnType<typeof setTimeout> | null = null;
	let introFadeId: ReturnType<typeof setTimeout> | null = null;

	onMount(() => {
		document.addEventListener('keydown', handleKeydown);
		// Show intro text, then start stacking
		introFadeId = setTimeout(() => { introVisible = true; }, 300);
		introTimeoutId = setTimeout(async () => {
			if (animationDone) return; // already skipped
			introVisible = false;
			await new Promise(r => setTimeout(r, 600));
			if (animationDone) return; // skipped during fade-out
			showIntro = false;
			await tick();
			startAnimation();
		}, 2800);
	});

	onDestroy(() => {
		if (animFrameId) cancelAnimationFrame(animFrameId);
		document.removeEventListener('keydown', handleKeydown);
	});
</script>

<!-- svelte-ignore a11y_click_events_have_key_events -->
<!-- svelte-ignore a11y_no_static_element_interactions -->
<div class="overlay" onclick={animationDone ? onclose : skipToFinal}>
	<div class="visualizer">
		<!-- Header -->
		<div class="viz-header">
			<span class="viz-title">YOUR TOKEN JOURNEY</span>
			{#if dateRange}
				<span class="viz-date-range">{dateRange}</span>
			{/if}
			<span class="viz-explain">1 TOKEN = 1 GRAIN OF RICE</span>
		</div>

		{#if totalTokens === 0}
			<div class="empty-state">No tokens yet — start a Claude session to begin your journey!</div>
		{:else if showIntro}
			<!-- Intro text -->
			<div class="intro-container">
				<p class="intro-text" class:visible={introVisible}>
					If every token were a grain of rice...
				</p>
				<p class="intro-text sub" class:visible={introVisible}>
					here is what your stack looks like.
				</p>
			</div>
		{:else}
			<!-- Canvas area -->
			<canvas class="journey-canvas" bind:this={canvas}></canvas>
		{/if}

		{#if !animationDone}
			<div class="skip-hint">PRESS SPACE TO SKIP</div>
		{:else if totalTokens > 0}
			<div class="bottom-actions">
				<button class="share-btn" onclick={shareImage} disabled={sharing}>
					{sharing ? 'SHARING...' : 'SHARE'}
				</button>
				<span class="close-hint">CLICK ANYWHERE TO CLOSE</span>
			</div>
		{/if}
	</div>
</div>

<style>
	.overlay {
		position: fixed;
		inset: 0;
		background: var(--bg-overlay);
		z-index: 9999;
		display: flex;
		align-items: center;
		justify-content: center;
		animation: fadeIn 0.3s ease-out;
	}

	@keyframes fadeIn {
		from { opacity: 0; }
		to { opacity: 1; }
	}

	.visualizer {
		display: flex;
		flex-direction: column;
		align-items: center;
		gap: var(--space-md);
		padding: var(--space-lg);
		width: 100%;
		height: 100%;
		max-width: 800px;
		max-height: 600px;
	}

	.viz-header {
		display: flex;
		flex-direction: column;
		align-items: center;
		width: 100%;
		flex-shrink: 0;
		gap: 4px;
	}

	.viz-title {
		font-family: var(--font-pixel);
		font-size: 22px;
		text-transform: uppercase;
		letter-spacing: 0.1em;
		color: var(--accent-amber);
		text-shadow: 0 0 20px var(--status-permission-glow);
	}

	.viz-date-range {
		font-family: var(--font-mono);
		font-size: 11px;
		color: var(--text-muted);
		text-transform: uppercase;
		letter-spacing: 0.05em;
	}

	.viz-explain {
		font-family: var(--font-mono);
		font-size: 10px;
		color: var(--text-muted);
		text-transform: uppercase;
		letter-spacing: 0.08em;
		opacity: 0.7;
	}

	/* ── Canvas ──────────────────────────── */
	.journey-canvas {
		flex: 1;
		width: 100%;
		min-height: 0;
	}

	/* ── Shared ──────────────────────────── */
	/* ── Intro text ─────────────────────── */
	.intro-container {
		flex: 1;
		display: flex;
		flex-direction: column;
		align-items: center;
		justify-content: center;
		gap: var(--space-md);
	}

	.intro-text {
		font-family: var(--font-pixel);
		font-size: 20px;
		color: var(--text-primary);
		text-align: center;
		opacity: 0;
		transform: translateY(10px);
		transition: opacity 0.8s ease-out, transform 0.8s ease-out;
		margin: 0;
	}

	.intro-text.visible {
		opacity: 1;
		transform: translateY(0);
	}

	.intro-text.sub {
		font-size: 16px;
		color: var(--accent-amber);
		transition-delay: 0.4s;
	}

	.empty-state {
		font-family: var(--font-mono);
		font-size: 13px;
		color: var(--text-muted);
		text-transform: uppercase;
		text-align: center;
		padding: var(--space-xl) 0;
	}

	.skip-hint {
		font-family: var(--font-mono);
		font-size: 11px;
		color: var(--text-muted);
		text-transform: uppercase;
		letter-spacing: 0.1em;
		animation: blink 2s infinite;
		flex-shrink: 0;
	}

	.bottom-actions {
		display: flex;
		flex-direction: column;
		align-items: center;
		gap: var(--space-sm);
		flex-shrink: 0;
	}

	.share-btn {
		font-family: var(--font-pixel);
		font-size: 12px;
		text-transform: uppercase;
		letter-spacing: 0.05em;
		color: var(--accent-amber);
		background: none;
		border: 1px solid var(--accent-amber);
		padding: 4px 16px;
		cursor: pointer;
		transition: background 0.15s, color 0.15s;
	}

	.share-btn:hover:not(:disabled) {
		background: var(--accent-amber);
		color: var(--bg-base);
	}

	.share-btn:disabled {
		opacity: 0.5;
		cursor: default;
	}

	.close-hint {
		font-family: var(--font-mono);
		font-size: 11px;
		color: var(--text-muted);
		text-transform: uppercase;
		letter-spacing: 0.1em;
	}

	@keyframes blink {
		0%, 100% { opacity: 0.5; }
		50% { opacity: 1; }
	}
</style>
