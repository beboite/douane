<script lang="ts">
	import Copy from '@lucide/svelte/icons/copy';
	import Check from '@lucide/svelte/icons/check';
	import KeyRound from '@lucide/svelte/icons/key-round';
	import Modal from './modal.svelte';
	import { copyText } from '$lib/clipboard';
	import { t } from '$lib/i18n.svelte';
	// The key lives only in this component: closing it drops the last copy the dashboard holds.
	let { name, key, close }: { name: string; key: string; close: () => void } = $props();
	let copied = $state(false);
	let failure = $state('');
	async function copy() { try { await copyText(key); copied = true; failure = ''; } catch { copied = false; failure = t('common.copyRefused'); } }
</script>

<Modal title={t('clients.keyTitle', { name })} {close}>
	<p class="once"><KeyRound size={17}/>{t('clients.keyOnce')}</p>
	<div class="key-field"><code>{key}</code><button class="button primary" onclick={copy}>{#if copied}<Check size={17}/>{t('clients.copied')}{:else}<Copy size={17}/>{t('clients.copyKey')}{/if}</button></div>
	<p class="dialog-text">{t('clients.keyBoite')}</p>
	{#if failure}<p class="dialog-error" role="alert">{failure}</p>{/if}
	<div class="dialog-actions"><button class="button" onclick={close}>{t('common.close')}</button></div>
</Modal>

<style>
	.once { display: flex; align-items: center; gap: 9px; margin: 0 0 16px; color: var(--yellow); font-size: 15px; }
	.once :global(svg) { flex-shrink: 0; }
	.key-field { display: flex; align-items: center; gap: 12px; margin: 0 0 18px; padding: 12px 12px 12px 16px; border: 1px solid #3c5279; border-radius: 11px; background: var(--background); }
	.key-field code { flex: 1; min-width: 0; font-size: 15px; line-height: 1.5; overflow-wrap: anywhere; user-select: all; }
	@media(max-width:650px) { .key-field { flex-direction: column; align-items: stretch; } }
</style>
