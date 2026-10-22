<script lang="ts">
	import { slide } from 'svelte/transition';
	import Search from '@lucide/svelte/icons/search';
	import RefreshCw from '@lucide/svelte/icons/refresh-cw';
	import ChevronRight from '@lucide/svelte/icons/chevron-right';
	import ChevronsUpDown from '@lucide/svelte/icons/chevrons-up-down';
	import Copy from '@lucide/svelte/icons/copy';
	import Check from '@lucide/svelte/icons/check';
	import ProviderMark from './provider-mark.svelte';
	import EmptyState from './empty-state.svelte';
	import { copyText } from '$lib/clipboard';
	import { ageLabel, formatCount, pools, providerName, type Account, type Model, type ModelListing } from '$lib/management';
	import { t } from '$lib/i18n.svelte';
	type Group = { id: string; provider: string; name: string; models: Model[] };
	let { models, listings = {}, accounts, busy, navigate, toggleModel, setModels, refreshModels }: { models: Model[]; listings?: Record<string, ModelListing>; accounts: Account[]; busy: string; navigate: (screen: string) => void; toggleModel: (model: Model) => void; setModels: (ids: string[], enabled: boolean) => void; refreshModels: (pool?: string) => void } = $props();
	const storageKey = 'douane-models-open';
	let search = $state('');
	let open = $state<Record<string, boolean>>(stored());
	let collapsedInSearch = $state<string[]>([]);
	let copied = $state('');
	let copyTimer: ReturnType<typeof setTimeout> | undefined;
	// Every pool with an account or a model, by name.
	let groups = $derived.by(() => {
		const byId = new Map<string, Group>(pools(accounts).map(pool => [pool.id, { id: pool.id, provider: pool.provider, name: pool.name, models: [] }]));
		for (const model of models) {
			const id = model.route || model.provider;
			const group = byId.get(id) ?? { id, provider: model.provider, name: model.route ?? providerName(model.provider), models: [] };
			group.models.push(model);
			byId.set(id, group);
		}
		return [...byId.values()].sort((a, b) => a.name.localeCompare(b.name));
	});
	let query = $derived(search.trim().toLowerCase());
	// A provider name that matches keeps its whole list.
	let shown = $derived(groups.map(group => ({ ...group, shown: !query || group.name.toLowerCase().includes(query) ? group.models : group.models.filter(m => `${m.id} ${m.name ?? ''}`.toLowerCase().includes(query)) })).filter(group => !query || group.shown.length));
	let allOpen = $derived(shown.length > 0 && shown.every(group => isOpen(group.id)));
	function stored(): Record<string, boolean> {
		try { return JSON.parse(localStorage.getItem(storageKey) ?? '{}'); } catch { return {}; }
	}
	function remember() { try { localStorage.setItem(storageKey, JSON.stringify(open)); } catch { /* The open state is a comfort, not data. */ } }
	function isOpen(id: string) { return query ? !collapsedInSearch.includes(id) : open[id] ?? groups.length === 1; }
	function toggle(id: string) {
		if (query) collapsedInSearch = collapsedInSearch.includes(id) ? collapsedInSearch.filter(item => item !== id) : [...collapsedInSearch, id];
		else { open = { ...open, [id]: !isOpen(id) }; remember(); }
	}
	function toggleAll() {
		const next = !allOpen;
		if (query) collapsedInSearch = next ? [] : shown.map(group => group.id);
		else { open = Object.fromEntries(groups.map(group => [group.id, next])); remember(); }
	}
	function setShown(list: Model[]) {
		const enable = !list.every(model => model.enabled !== false);
		const ids = list.filter(model => (model.enabled !== false) !== enable).map(model => model.id);
		if (ids.length) setModels(ids, enable);
	}
	async function copyId(id: string) {
		try { await copyText(id); copied = id; clearTimeout(copyTimer); copyTimer = setTimeout(() => copied = '', 1500); } catch { copied = ''; }
	}
</script>

<div class="page-heading">
	<div><h1>{t('nav.models')}</h1><p class="page-description">{t('models.description')}</p></div>
	<div class="heading-actions"><span class="muted small">{t('models.count', { count: models.length })}</span><button class="button" disabled={!!busy || !accounts.length} onclick={() => refreshModels()}><RefreshCw size={18} class={busy === 'models:*' ? 'spinning' : ''}/>{t('models.refreshAll')}</button></div>
</div>
<div class="section-toolbar">
	<p>{t('models.endpoint')} <span class="inline-code">/v1/models</span></p>
	<div class="toolbar-controls">
		{#if shown.length > 1}<button class="button subtle" onclick={toggleAll}><ChevronsUpDown size={17}/>{t(allOpen ? 'models.collapseAll' : 'models.expandAll')}</button>{/if}
		<label class="search-input"><Search size={15}/><input type="search" bind:value={search} oninput={() => collapsedInSearch = []} aria-label={t('models.search')} placeholder={t('models.search')}/></label>
	</div>
</div>

{#if !groups.length}<section class="ledger"><EmptyState title={t('models.emptyTitle')} description={t('models.emptyDescription')} action={t('models.addAccount')} onclick={() => navigate('accounts')}/></section>
{:else if !shown.length}<section class="ledger"><EmptyState title={t('models.noMatchTitle')} description={t('models.noMatchDescription')}/></section>
{:else}
	<div class="model-groups">
		{#each shown as group (group.id)}
			{@const expanded = isOpen(group.id)}
			{@const enabled = group.models.filter(model => model.enabled !== false).length}
			{@const allOn = group.shown.length > 0 && group.shown.every(model => model.enabled !== false)}
			{@const someOn = group.shown.some(model => model.enabled !== false)}
			{@const listing = listings[group.id]}
			<section class="model-group" class:open={expanded} aria-label={t('models.group', { name: group.name })}>
				<header class="group-head">
					<button class="group-toggle" aria-expanded={expanded} aria-controls="models-{group.id}" onclick={() => toggle(group.id)}>
						<ChevronRight size={18} class="chevron"/>
						<ProviderMark provider={group.provider} size={26}/>
						<h2>{group.name}</h2>
						<span class="group-count">{t('models.enabledCount', { enabled, count: group.models.length })}</span>
					</button>
					<div class="group-actions">
						{#if listing?.stale}<span class="state-badge warning" title={t('models.staleTitle', { name: group.name })}><span class="status-pip"></span>{t('models.stale')}</span>
						{:else if listing}<span class="refreshed" title={new Date(listing.refreshed_at).toLocaleString()}>{t('models.refreshed', { age: ageLabel(listing.refreshed_at) })}</span>{/if}
						<button class="icon-button" disabled={!!busy} onclick={() => refreshModels(group.id)} title={t('models.refreshPool', { name: group.name })} aria-label={t('models.refreshPool', { name: group.name })}><RefreshCw size={18} class={busy === `models:${group.id}` || busy === 'models:*' ? 'spinning' : ''}/></button>
						<button class="switch" class:on={allOn} class:partial={someOn && !allOn} role="switch" aria-checked={allOn ? 'true' : someOn ? 'mixed' : 'false'} title={t(allOn ? 'models.disableShown' : 'models.enableShown', { name: group.name })} aria-label={t(allOn ? 'models.disableShown' : 'models.enableShown', { name: group.name })} disabled={!!busy || !group.shown.length} onclick={() => setShown(group.shown)}><span></span></button>
					</div>
				</header>
				{#if expanded}
					<div id="models-{group.id}" class="group-body" transition:slide={{ duration: 180 }}>
						{#if !group.shown.length}<p class="pool-empty">{t('models.poolEmpty')}</p>
						{:else}<div class="table-scroll"><table><thead><tr><th>{t('table.model')}</th><th class="context">{t('table.context')}</th><th class="available">{t('table.available')}</th></tr></thead><tbody>
							{#each group.shown as model (model.id)}
								<tr class:off={model.enabled === false}>
									<td><span class="model-name">{model.name ?? model.id}</span><button class="model-id" onclick={() => copyId(model.id)} title={t('models.copyId', { id: model.id })} aria-label={t('models.copyId', { id: model.id })}>{model.id}{#if copied === model.id}<Check size={14}/>{:else}<Copy size={14}/>{/if}</button></td>
									<td class="context tabular muted">{formatCount(model.context_length ?? undefined)}</td>
									<td class="available"><button class="switch" class:on={model.enabled !== false} role="switch" aria-checked={model.enabled !== false} aria-label={t('common.enable', { name: model.name ?? model.id })} disabled={!!busy} onclick={() => toggleModel(model)}><span></span></button></td>
								</tr>
							{/each}
						</tbody></table></div>{/if}
					</div>
				{/if}
			</section>
		{/each}
	</div>
{/if}

<style>
	.toolbar-controls { display: flex; align-items: center; gap: 10px; }
	.model-groups { display: grid; gap: 14px; }
	.model-group { overflow: clip; border: 1px solid var(--border); border-radius: 13px; background: var(--surface); transition: border-color 160ms; }
	.model-group.open { border-color: #34466a; }
	/* The heading stays in view while a long list scrolls under it. */
	.group-head { position: sticky; top: 0; z-index: 2; display: flex; align-items: center; gap: 10px; padding: 8px 14px 8px 8px; border-radius: 13px; background: var(--surface); }
	.group-toggle { display: flex; flex: 1; align-items: center; gap: 12px; min-width: 0; padding: 9px 10px; border: 0; border-radius: 9px; background: transparent; color: inherit; text-align: left; cursor: pointer; }
	.group-toggle:hover { background: var(--surface-hover); }
	.group-toggle :global(.chevron) { flex-shrink: 0; color: var(--faint); transition: transform 180ms ease; }
	.open .group-toggle :global(.chevron) { transform: rotate(90deg); color: var(--blue); }
	.group-toggle h2 { overflow: hidden; margin: 0; font-family: var(--font-display); font-size: 20px; font-weight: 600; letter-spacing: -.02em; text-overflow: ellipsis; white-space: nowrap; }
	.group-count { margin-left: auto; padding-left: 12px; color: var(--muted); font-size: 15px; font-variant-numeric: tabular-nums; white-space: nowrap; }
	.group-actions { display: flex; align-items: center; gap: 6px; }
	.refreshed { color: var(--faint); font-size: 14px; white-space: nowrap; }
	.group-actions .switch { margin-left: 6px; }
	.switch.partial { border-color: var(--primary); background: #5478f640; }
	.switch.partial span { transform: translateX(6.5px); background: #fff; }
	.open .group-head { border-bottom: 1px solid var(--border); border-radius: 13px 13px 0 0; }
	.group-body tbody tr:last-child td { border-bottom: 0; }
	.group-body td { padding: 13px 18px; }
	.group-body th.available, .group-body td.available, .group-body .context { width: 1%; text-align: right; white-space: nowrap; }
	.group-body .context { padding-right: 36px; }
	.group-body td.available .switch { margin-left: auto; }
	tr.off .model-name { color: var(--faint); }
	.model-name { display: block; }
	.model-id { display: inline-flex; align-items: center; gap: 7px; max-width: 100%; margin-top: 3px; padding: 0; border: 0; background: transparent; color: var(--muted); font-family: var(--account-font); font-size: 14px; text-align: left; overflow-wrap: anywhere; cursor: copy; }
	.model-id :global(svg) { flex-shrink: 0; opacity: 0; transition: opacity 120ms; }
	.model-id:hover { color: var(--text); }
	.model-id:hover :global(svg), .model-id:focus-visible :global(svg) { opacity: 1; }
	.pool-empty { margin: 0; padding: 20px 18px; color: var(--muted); font-size: 15px; }
	@media(max-width:700px) {
		.group-head { flex-wrap: wrap; padding: 6px 10px 8px 6px; gap: 4px; }
		.group-toggle { flex-basis: 100%; }
		.group-actions { width: 100%; justify-content: flex-end; }
		.refreshed { margin-right: auto; padding-left: 10px; }
		.group-actions .state-badge { margin-right: auto; margin-left: 10px; }
		.toolbar-controls { width: 100%; }
		.toolbar-controls .search-input { flex: 1; width: auto; }
		.group-body thead { display: none; }
		.group-body td { padding: 12px 14px; }
		.group-body .context { padding-right: 6px; }
		.model-id { font-size: 13px; }
	}
</style>
