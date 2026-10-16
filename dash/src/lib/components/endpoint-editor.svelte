<script lang="ts">
	import Pencil from '@lucide/svelte/icons/pencil';
	import { dialects, type Account, type Dialect, type EndpointPatch } from '$lib/management';
	let { account, busy, save }: { account: Account; busy: string; save: (account: Account, patch: EndpointPatch) => Promise<boolean> } = $props();
	let editing = $state(false);
	let route = $state('');
	let baseUrl = $state('');
	let dialect = $state<Dialect | ''>('');
	let models = $state('');
	let key = $state('');
	const dialectLabel = $derived(dialects.find(option => option.id === (account.dialect ?? ''))?.label ?? '');
	function open() {
		route = account.route ?? ''; baseUrl = account.base_url ?? ''; dialect = account.dialect ?? ''; models = (account.models ?? []).join(', '); key = ''; editing = true;
	}
	async function submit(event: SubmitEvent) {
		event.preventDefault();
		const patch: EndpointPatch = {};
		const nextRoute = route.trim().toLowerCase();
		if (nextRoute && nextRoute !== (account.route ?? '')) patch.route = nextRoute;
		if (baseUrl.trim() && baseUrl.trim() !== (account.base_url ?? '')) patch.base_url = baseUrl.trim();
		if ((dialect || null) !== (account.dialect ?? null)) patch.dialect = dialect || null;
		const list = models.split(',').map(model => model.trim()).filter(Boolean);
		if (list.join(',') !== (account.models ?? []).join(',')) patch.models = list;
		if (key.trim()) patch.api_key = key.trim();
		if (!Object.keys(patch).length) { editing = false; return; }
		if (await save(account, patch)) { editing = false; key = ''; }
	}
</script>

{#if editing}
	<form class="endpoint-editor" onsubmit={submit}>
		<label>Préfixe des modèles<input bind:value={route} required pattern="[a-z0-9][a-z0-9_\-]{'{0,31}'}" autocomplete="off" spellcheck="false"/></label>
		<label>Adresse de l'API<input type="url" bind:value={baseUrl} required/></label>
		<label>Format de l'API<select bind:value={dialect}>{#each dialects as option (option.id)}<option value={option.id}>{option.label}</option>{/each}</select></label>
		<label>Modèles servis<input bind:value={models} placeholder="Tous les modèles de l'API" autocomplete="off" spellcheck="false"/></label>
		<label>Nouvelle clé d'API<input type="password" bind:value={key} placeholder="Inchangée" autocomplete="off" spellcheck="false"/></label>
		<div class="editor-actions"><button class="button primary" disabled={!!busy}>Enregistrer</button><button type="button" class="button" onclick={() => { editing = false; key = ''; }}>Annuler</button></div>
	</form>
{:else if account.route || account.base_url}
	<div class="endpoint-line">
		{#if account.route}<code>{account.route}/</code>{/if}
		<span class="endpoint-url" title={account.base_url ?? ''}>{account.base_url}</span>
		{#if dialectLabel}<span class="endpoint-format">{dialectLabel}</span>{/if}
		<button class="icon-button" aria-label="Modifier l'endpoint" title="Modifier l'endpoint" disabled={!!busy} onclick={open}><Pencil size={15}/></button>
	</div>
{/if}

<style>
	.endpoint-line { display: flex; flex-wrap: wrap; gap: 6px 9px; align-items: center; margin-top: 7px; font-size: 14px; color: var(--muted); }
	.endpoint-line code { color: var(--text); font-size: 14px; }
	.endpoint-url { max-width: 260px; overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
	.endpoint-format { padding: 2px 7px; border: 1px solid var(--border); border-radius: 6px; font-size: 13px; }
	.endpoint-editor { display: grid; gap: 9px; margin-top: 10px; max-width: 340px; }
	.endpoint-editor label { display: grid; gap: 5px; font-size: 14px; color: var(--muted); }
	.endpoint-editor input, .endpoint-editor select { width: 100%; font-size: 14px; }
	.editor-actions { display: flex; gap: 8px; margin-top: 4px; }
</style>
