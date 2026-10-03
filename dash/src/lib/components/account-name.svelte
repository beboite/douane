<script lang="ts">
	import { tick } from 'svelte';
	import Pencil from '@lucide/svelte/icons/pencil';
	import Check from '@lucide/svelte/icons/check';
	import X from '@lucide/svelte/icons/x';
	import { privateLabel, type Account } from '$lib/management';
	let { account, rename, busy }: { account: Account; rename: (account: Account, name: string) => Promise<void>; busy: string } = $props();
	let editing = $state(false);
	let draft = $state('');
	let saving = $state(false);
	let failure = $state('');
	let input = $state<HTMLInputElement>();
	async function edit() {
		draft = account.name; failure = ''; editing = true;
		await tick(); input?.focus(); input?.select();
	}
	async function save() {
		if (saving || busy) return;
		const name = draft.trim();
		if (!name || name.length > 120 || /[\x00-\x1f\x7f]/.test(name)) { failure = 'Saisissez un nom de 1 à 120 caractères.'; return; }
		if (name === account.name) { editing = false; return; }
		saving = true; failure = '';
		try { await rename(account, name); editing = false; }
		catch (error) { failure = privateLabel((error as Error).message); }
		finally { saving = false; }
	}
</script>

{#if editing}
	<form class="name-editor" onsubmit={(event) => { event.preventDefault(); void save(); }}>
		<input bind:this={input} bind:value={draft} aria-label="Nouveau nom du compte" maxlength="120" required disabled={saving} onkeydown={(event) => { if (event.key === 'Escape' && !saving) editing = false; }}/>
		<div class="name-actions"><button class="icon-button" type="submit" aria-label="Enregistrer le nom" title="Enregistrer" disabled={saving || !!busy}><Check size={20}/></button><button class="icon-button" type="button" aria-label="Annuler le renommage" title="Annuler" disabled={saving} onclick={() => editing = false}><X size={20}/></button></div>
		{#if failure}<p class="rename-error" role="alert">{failure}</p>{/if}
	</form>
{:else}
	<button class="account-name" title="Renommer ce compte" disabled={!!busy} onclick={edit}><span>{privateLabel(account.name)}</span><Pencil size={15}/></button>
{/if}

<style>
	.account-name { display: inline-flex; align-items: center; gap: 9px; max-width: 100%; padding: 3px 0; border: 0; background: transparent; text-align: left; font-size: 17px; font-weight: 600; line-height: 1.45; }
	.account-name span { overflow-wrap: anywhere; }
	.account-name :global(svg) { flex-shrink: 0; color: var(--faint); opacity: .55; }
	.account-name:hover { color: var(--blue); }
	.account-name:hover :global(svg),.account-name:focus-visible :global(svg) { opacity: 1; }
	.name-editor { display: flex; flex-wrap: wrap; align-items: center; gap: 6px; max-width: 100%; }
	.name-editor input { flex: 1; min-width: 120px; width: 100%; height: 44px; font-size: 16px; }
	.name-actions { display: flex; }
	.rename-error { flex-basis: 100%; color: var(--red); font-size: 15px; line-height: 1.5; margin: 4px 0; }
</style>
