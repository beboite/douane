<script lang="ts">
	import { tick } from 'svelte';
	import ShieldCheck from '@lucide/svelte/icons/shield-check';
	import Check from '@lucide/svelte/icons/check';
	import X from '@lucide/svelte/icons/x';
	import { reserveWindow, reserveWindows, type Account, type Quota, type ReserveWindow } from '$lib/management';
	let { account, quota, busy, save }: { account: Account; quota?: Quota; busy: string; save: (account: Account, reserve: number | null, windows: ReserveWindow[]) => Promise<boolean> } = $props();
	let editing = $state(false);
	let draft = $state('');
	let chosen = $state<ReserveWindow[]>([]);
	let saving = $state(false);
	let failure = $state('');
	let input = $state<HTMLInputElement>();
	let reserve = $derived(account.reserve_percent ?? null);
	let watched = $derived(account.reserve_windows ?? []);
	// Only the windows this account reports, plus any already watched.
	let offered = $derived(reserveWindows.filter(({ id }) => watched.includes(id) || quota?.windows.some(window => window.remaining_percent != null && reserveWindow(window.id) === id)));
	let watchedLabel = $derived(reserveWindows.filter(({ id }) => watched.includes(id)).map(({ label }) => label).join(' + '));
	const percent = (value: number) => new Intl.NumberFormat('fr-FR', { maximumFractionDigits: 1 }).format(value);
	async function edit() {
		draft = reserve == null ? '' : String(reserve); chosen = [...watched]; failure = ''; editing = true;
		await tick(); input?.focus(); input?.select();
	}
	function toggle(id: ReserveWindow) { chosen = chosen.includes(id) ? chosen.filter(kind => kind !== id) : [...chosen, id]; }
	async function submit(next: number | null, windows: ReserveWindow[]) {
		if (saving || busy) return;
		if (next === reserve && windows.length === watched.length && windows.every(id => watched.includes(id))) { editing = false; return; }
		saving = true; failure = '';
		try { if (await save(account, next, windows)) editing = false; else failure = "La réserve n'a pas été enregistrée."; }
		finally { saving = false; }
	}
	function confirm() {
		const text = draft.trim().replace(',', '.');
		if (!text) return submit(null, []);
		const value = Number(text);
		if (!/^\d+(\.\d+)?$/.test(text) || value >= 100) { failure = 'Saisissez un pourcentage entre 0 et 99.'; return; }
		return value === 0 ? submit(null, []) : submit(value, chosen);
	}
</script>

{#if editing}
	<form class="reserve-editor" onsubmit={(event) => { event.preventDefault(); void confirm(); }}>
		<label><span>Garder au moins</span><input bind:this={input} bind:value={draft} type="text" inputmode="decimal" placeholder="5" aria-label="Réserve minimale en pourcentage" disabled={saving} onkeydown={(event) => { if (event.key === 'Escape' && !saving) editing = false; }}/><span>%</span></label>
		<div class="reserve-actions"><button class="icon-button" type="submit" aria-label="Enregistrer la réserve" title="Enregistrer" disabled={saving || !!busy}><Check size={20}/></button><button class="icon-button" type="button" aria-label="Annuler" title="Annuler" disabled={saving} onclick={() => editing = false}><X size={20}/></button></div>
		{#if offered.length > 1}
			<div class="reserve-windows" role="group" aria-label="Limites surveillées">
				<button type="button" class="chip" class:on={!chosen.length} aria-pressed={!chosen.length} title="La limite la plus longue du compte (mois ou semaine)" disabled={saving} onclick={() => chosen = []}>Plus longue</button>
				{#each offered as option (option.id)}<button type="button" class="chip" class:on={chosen.includes(option.id)} aria-pressed={chosen.includes(option.id)} disabled={saving} onclick={() => toggle(option.id)}>{option.label}</button>{/each}
			</div>
		{/if}
		{#if reserve != null}<button class="link-button" type="button" disabled={saving || !!busy} onclick={() => submit(null, [])}>Retirer la réserve</button>{/if}
		{#if failure}<p class="reserve-error" role="alert">{failure}</p>{/if}
	</form>
{:else}
	<button class="reserve" class:set={reserve != null} disabled={!!busy} onclick={edit} title={watched.length ? `Douane cesse d'utiliser ce compte quand une de ces limites atteint ce seuil : ${watchedLabel}` : "Douane cesse d'utiliser ce compte quand son quota le plus long (mois ou semaine) atteint ce seuil"}><ShieldCheck size={16}/>{reserve == null ? 'Ajouter une réserve' : `Réserve ${percent(reserve)} %${watched.length ? ` · ${watchedLabel}` : ''}`}</button>
	{#if reserve != null && account.enabled && account.reserve_reached}<p class="reserve-note reached">Réserve atteinte : douane n'utilise plus ce compte jusqu'à la réinitialisation.</p>
	{:else if reserve != null && account.reserve_measured === false}<p class="reserve-note">{watched.length ? 'Inactive : ce compte ne communique pas ces limites.' : "Inactive : ce fournisseur ne communique pas de quota pour l'ensemble du compte."}</p>{/if}
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
	.reserve-windows { display: flex; flex-wrap: wrap; flex-basis: 100%; gap: 6px; }
	.chip { padding: 5px 11px; border: 1px solid var(--border); border-radius: 999px; background: transparent; color: var(--muted); font-size: 14px; transition: background .15s, border-color .15s, color .15s; }
	.chip:hover:not(:disabled) { border-color: var(--blue); color: var(--text); }
	.chip.on { border-color: var(--blue); background: #5478f622; color: var(--text); }
	.link-button { border: 0; background: transparent; padding: 3px 0; color: var(--muted); font-size: 15px; text-decoration: underline; }
	.link-button:hover { color: var(--red); }
	.reserve-error { flex-basis: 100%; color: var(--red); font-size: 15px; line-height: 1.5; margin: 2px 0; }
	.reserve-note { max-width: 320px; margin: 6px 0 0; color: var(--muted); font-size: 14px; line-height: 1.45; }
	.reserve-note.reached { color: var(--yellow); }
</style>
