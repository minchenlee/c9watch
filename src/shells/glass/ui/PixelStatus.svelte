<script lang="ts">
    // Classic pixel status (direction §10): pixel font, uppercase, letter
    // spacing, a square dot. 13px minimum; the brighter status text colors.
    import type { Tone } from './format';

    export let tone: Tone;
    export let label: string;
</script>

<span class="pixel-status {tone}" data-tone={tone}>{label}</span>

<style>
    .pixel-status {
        display: inline-flex;
        align-items: center;
        gap: var(--space-2);
        min-width: 0;
        font-family: var(--font-pixel);
        font-weight: 500;
        font-size: 13px;
        line-height: 16px;
        letter-spacing: 0.1em;
        text-transform: uppercase;
        color: var(--status-idle-text);
        white-space: nowrap;
    }
    .pixel-status::before {
        content: '';
        flex: none;
        width: 8px;
        height: 8px;
        background: currentColor;
    }
    .working { color: var(--status-working-text); }
    .needs { color: var(--status-permission-text); }
    .waiting { color: var(--status-input-text); }
    .stale { color: var(--status-error-text); }
    .done, .idle { color: var(--text-3); }
    .stale::before { background: none; box-shadow: inset 0 0 0 1.5px currentColor; }
    @media (prefers-reduced-motion: no-preference) {
        .working::before { animation: glass-breathe 2.4s ease-in-out infinite; }
    }
    @keyframes glass-breathe { 50% { opacity: 0.45; } }
</style>
