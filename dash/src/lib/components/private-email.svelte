<script lang="ts">
	import Eye from '@lucide/svelte/icons/eye';
	import EyeOff from '@lucide/svelte/icons/eye-off';
	import { t } from '$lib/i18n.svelte';
	let { email }: { email: string } = $props();
	let revealed = $state(false);
</script>

<button class="private-email" aria-label={t(revealed ? 'email.hide' : 'email.show')} aria-pressed={revealed} title={t(revealed ? 'email.hide' : 'email.show')} onclick={() => revealed = !revealed}>
	<span class:blurred={!revealed} aria-hidden={!revealed}>{email}</span>
	{#if revealed}<EyeOff size={16}/>{:else}<Eye size={16}/>{/if}
</button>

<style>
	.private-email { display: flex; align-items: center; gap: 10px; max-width: 100%; padding: 5px 0; border: 0; background: transparent; color: var(--muted); text-align: left; font-size: 15px; line-height: 1.4; }
	.private-email span { overflow-wrap: anywhere; }
	.blurred { filter: blur(5px); user-select: none; }
	.private-email :global(svg) { flex-shrink: 0; color: var(--faint); }
	.private-email:hover { color: var(--text); }
</style>
