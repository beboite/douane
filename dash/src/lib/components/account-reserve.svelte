<script lang="ts">
	import { tick } from 'svelte';
	import ShieldCheck from '@lucide/svelte/icons/shield-check';
	import Check from '@lucide/svelte/icons/check';
	import X from '@lucide/svelte/icons/x';
	import type { Account } from '$lib/management';
	let { account, busy, save }: { account: Account; busy: string; save: (account: Account, reserve: number | null) => Promise<boolean> } = $props();
	let editing = $state(false);
	let draft = $state('');
	let saving = $state(false);
	let failure = $state('');
	let input = $state<HTMLInputElement>();
	let reserve = $derived(account.reserve_percent ?? null);
	const percent = (value: number) => new Intl.NumberFormat('fr-FR', { maximumFractionDigits: 1 }).format(value);
	async function edit() {
		draft = reserve == null ? '' : String(reserve); failure = ''; editing = true;
		await tick(); input?.focus(); input?.select();
	}
	async function submit(next: number | null) {
		if (saving || busy) return;
		if (next === reserve) { editing = false; return; }
		saving = true; failure = '';
		try { if (await save(account, next)) editing = false; else failure = "La réserve n'a pas été enregistrée."; }
		finally { saving = false; }
	}
	function confirm() {
		const text = draft.trim().replace(',', '.');
		if (!text) return submit(null);
		const value = Number(text);
		if (!/^\d+(\.\d+)?$/.test(text) || value >= 100) { failure = 'Saisissez un pourcentage entre 0 et 99.'; return; }
		return submit(value === 0 ? null : value);
	}
</script>

{#if editing}
	<form class="reserve-editor" onsubmit={(event) => { event.preventDefault(); void confirm(); }}>
		<label><span>Garder au moins</span><input bind:this={input} bind:value={draft} type="text" inputmode="decimal" placeholder="5" aria-label="Réserve minimale en pourcentage" disabled={saving} onkeydown={(event) => { if (event.key === 'Escape' && !saving) editing = false; }}/><span>%</span></label>
		<div class="reserve-actions"><button class="icon-button" type="submit" aria-label="Enregistrer la réserve" title="Enregistrer" disabled={saving || !!busy}><Check size={20}/></button><button class="icon-button" type="button" aria-label="Annuler" title="Annuler" disabled={saving} onclick={() => editing = false}><X size={20}/></button></div>
		{#if reserve != null}<button class="link-button" type="button" disabled={saving || !!busy} onclick={() => submit(null)}>Retirer la réserve</button>{/if}
		{#if failure}<p class="reserve-error" role="alert">{failure}</p>{/if}
	</form>
{:else}
	<button class="reserve" class:set={reserve != null} disabled={!!busy} onclick={edit} title="Douane cesse d'utiliser ce compte quand un quota du compte atteint ce seuil"><ShieldCheck size={16}/>{reserve == null ? 'Ajouter une réserve' : `Réserve ${percent(reserve)} %`}</button>
	{#if reserve != null && account.enabled && account.reserve_reached}<p class="reserve-note reached">Réserve atteinte : douane n'utilise plus ce compte jusqu'à la réinitialisation.</p>
	{:else if reserve != null && account.reserve_measured === false}<p class="reserve-note">Inactive : ce fournisseur ne communique pas de quota pour l'ensemble du compte.</p>{/if}
{/if}

<style>
	.reserve { display: flex; width: fit-content; align-items: center; gap: 7px; margin-top: 8px; padding: 3px 0; border: 0; background: transparent; color: var(--muted); font-size: 15px; }
	.reserve :global(svg) { flex-shrink: 0; color: var(--faint); }
	.reserve.set { color: var(--text); }
	.reserve.set :global(svg) { color: var(--blue); }
	.reserve:hover { color: var(--blue); }
	.reserve-editor { display: flex; flex-wrap: wrap; align-items: center; gap: 6px 10px; margin-top: 8px; }
	.reserve-editor label { display: flex; align-items: center; gap: 8px; font-size: 15px; color: var(--muted); }
	.reserve-editor input { width: 76px; height: 40px; font-size: 16px; }
	.reserve-actions { display: flex; }
	.link-button { border: 0; background: transparent; padding: 3px 0; color: var(--muted); font-size: 15px; text-decoration: underline; }
	.link-button:hover { color: var(--red); }
	.reserve-error { flex-basis: 100%; color: var(--red); font-size: 15px; line-height: 1.5; margin: 2px 0; }
	.reserve-note { max-width: 320px; margin: 6px 0 0; color: var(--muted); font-size: 14px; line-height: 1.45; }
	.reserve-note.reached { color: var(--yellow); }
</style>
