<script lang="ts">
	import { onMount, type Snippet } from 'svelte';
	import X from '@lucide/svelte/icons/x';
	import { t } from '$lib/i18n.svelte';
	let { title, close, locked = false, wide = false, children }: { title: string; close: () => void; locked?: boolean; wide?: boolean; children: Snippet } = $props();
	let dialog: HTMLDialogElement;
	onMount(() => { dialog.showModal(); return () => dialog.close(); });
</script>

<dialog bind:this={dialog} class="modal" class:wide oncancel={(event) => { event.preventDefault(); if (!locked) close(); }}>
	<header><h2>{title}</h2><button class="icon-button" aria-label={t('common.close')} disabled={locked} onclick={close}><X size={20}/></button></header>
	{@render children()}
</dialog>

<style>
	.modal { width: min(520px,calc(100vw - 32px)); max-height: calc(100dvh - 32px); overflow-y: auto; padding: 25px; border: 1px solid #35496b; border-radius: 16px; background: var(--surface); color: var(--text); box-shadow: 0 24px 80px #030711b0; }
	.modal.wide { width: min(640px,calc(100vw - 32px)); }
	.modal::backdrop { background: #030711b5; backdrop-filter: blur(6px); }
	header { display: flex; align-items: center; justify-content: space-between; gap: 15px; margin-bottom: 14px; }
	h2 { font-family: var(--font-display); font-size: 24px; font-weight: 650; letter-spacing: -.03em; overflow-wrap: anywhere; }
	.modal :global(.dialog-actions) { display: flex; flex-wrap: wrap; justify-content: flex-end; gap: 10px; margin-top: 24px; }
	.modal :global(.dialog-text) { margin: 0 0 10px; color: var(--muted); font-size: 16px; line-height: 1.6; }
	.modal :global(.dialog-error) { margin: 12px 0 0; color: var(--red); font-size: 15px; line-height: 1.5; overflow-wrap: anywhere; }
	@media(max-width:650px) { .modal { padding: 20px 18px; } h2 { font-size: 22px; } .modal :global(.dialog-actions .button) { flex: 1; } }
</style>
