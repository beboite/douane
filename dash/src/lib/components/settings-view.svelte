<script lang="ts">
	import { untrack } from 'svelte';
	import Save from '@lucide/svelte/icons/save';
	import GitBranch from '@lucide/svelte/icons/git-branch';
	import ProviderMark from './provider-mark.svelte';
	import { type Settings, type Provider, type Strategy } from '$lib/management';
	let { settings, providers, busy, save }: { settings: Settings; providers: Provider[]; busy: string; save: (settings: Settings) => Promise<boolean> } = $props();
	let draft = $state<Settings>(untrack(() => ({ ...settings, provider_strategies: { ...settings.provider_strategies } })));
	const strategies: { id: Strategy; label: string; desc: string }[] = [
		{ id:'round-robin',label:'Rotation',desc:'Chaque compte actif reçoit les requêtes à tour de rôle.' },
		{ id:'fill-first',label:'Épuiser un compte',desc:'Utilise le premier compte disponible avant de passer au suivant.' },
		{ id:'reset-first',label:'Reset le plus proche',desc:'Utilise le compte dont le quota hebdomadaire se réinitialise en premier. Les autres servent seulement s’il est indisponible.' },
		{ id:'smart',label:'Répartition intelligente',desc:'À moins de 48 h du reset, 90 % des nouvelles conversations utilisent ce compte jusqu’à épuisement.' }
	];
	// A routing choice saves on click with every visible edit; a refused save
	// restores the routing in force, so a highlighted strategy is never a draft.
	async function route(change: Partial<Settings>) {
		const inForce = { strategy: draft.strategy, provider_strategies: draft.provider_strategies };
		draft = { ...draft, ...change };
		if (!(await save(draft))) draft = { ...draft, ...inForce };
	}
	function override(provider: string, strategy: string) { const next = { ...draft.provider_strategies }; if (strategy) next[provider] = strategy as Strategy; else delete next[provider]; route({ provider_strategies: next }); }
</script>

<div class="page-heading"><h1>Réglages</h1></div>
<p class="session-note"><GitBranch size={18}/><span>Une conversation garde son compte tant qu'il reste disponible. Avec Reset le plus proche, elle revient au compte dont le reset est le plus proche dès qu'il est disponible.</span></p>
<form class="settings-form" onsubmit={(event) => { event.preventDefault(); save(draft); }}>
	<section class="settings-section"><div><h2>Répartition des requêtes</h2><p>La règle par défaut s'applique aux fournisseurs sans choix particulier. Chaque choix est enregistré dès le clic.</p></div><div class="strategy-options">{#each strategies as strategy (strategy.id)}<label class="strategy-option" class:chosen={draft.strategy === strategy.id}><input type="radio" name="strategy" value={strategy.id} checked={draft.strategy === strategy.id} disabled={!!busy} onchange={() => route({ strategy: strategy.id })}/><span>{strategy.label}<small>{strategy.desc}</small></span></label>{/each}</div></section>
	<section class="settings-section"><div><h2>Par fournisseur</h2><p>Adaptez la répartition à chacun de vos abonnements.</p></div><div class="settings-fields provider-fields">{#each providers as provider (provider.id)}<label><span class="provider-label"><ProviderMark provider={provider.id} size={22}/>{provider.name}</span><select value={draft.provider_strategies?.[provider.id] ?? ''} disabled={!!busy} onchange={(event) => override(provider.id,event.currentTarget.value)}><option value="">Règle par défaut</option>{#each strategies as strategy (strategy.id)}<option value={strategy.id}>{strategy.label}</option>{/each}</select></label>{/each}{#if !providers.length}<p class="muted small">Les fournisseurs apparaîtront lorsque le service sera connecté.</p>{/if}</div></section>
	<section class="settings-section"><div><h2>Actualisation et délais</h2><p>Les quotas et la sélection des comptes sont recalculés après chaque actualisation.</p></div><div class="settings-fields"><label>Actualiser les quotas toutes les<span class="unit-field"><input type="number" min="30" max="86400" step="1" bind:value={draft.quota_refresh_seconds} required/><span>secondes</span></span></label><label>Délai maximal d'une requête<span class="unit-field"><input type="number" min="10" max="1800" step="1" bind:value={draft.request_timeout_seconds} required/><span>secondes</span></span></label><label>Fournisseur par défaut<select bind:value={draft.default_provider}><option value={null}>Sélection automatique</option>{#each providers as provider (provider.id)}<option value={provider.id}>{provider.name}</option>{/each}</select></label></div></section>
	<div class="settings-footer"><button class="button primary" type="submit" disabled={!!busy}><Save size={14}/>{busy === 'settings' ? 'Enregistrement...' : 'Enregistrer les réglages'}</button></div>
</form>

<style>
	.session-note { display: flex; align-items: flex-start; gap: 12px; font-size: 15px; color: var(--muted); line-height: 1.65; margin: 24px 0 0; padding: 16px 18px; border: 1px solid var(--border); border-radius: 12px; background: var(--surface); max-width: 1060px; }
	.session-note :global(svg) { flex-shrink: 0; color: var(--blue); margin-top: 2px; }
	.settings-form { margin-top: 32px; max-width: 1060px; }.settings-section { display: grid; grid-template-columns: 245px 1fr; gap: 34px; border-bottom: 1px solid var(--border); padding: 30px 0; }.settings-section:first-child { padding-top: 0; }.settings-section h2 { font-size: 16px; margin: 0 0 9px; }.settings-section p { color: var(--muted); font-size: 15px; line-height: 1.65; margin: 0; }.strategy-options { display: grid; grid-template-columns: repeat(2,minmax(0,1fr)); gap: 12px; }.strategy-option { display: flex; gap: 12px; align-items: flex-start; min-height: 126px; padding: 18px 16px; border: 1px solid var(--border); border-radius: 12px; background: var(--surface); cursor: pointer; }.strategy-option:hover { border-color: #3c5279; }.strategy-option.chosen { border-color: var(--blue); background: #5478f613; box-shadow: inset 0 0 0 1px #7899ff22; }.strategy-option input { flex-shrink: 0; width: auto; height: auto; margin: 3px 0 0; accent-color: var(--primary); }.strategy-option span { font-size: 15px; font-weight: 550; line-height: 1.3; }.strategy-option small { display: block; font-size: 15px; font-weight: 400; color: var(--muted); line-height: 1.55; margin-top: 8px; }.settings-fields { display: grid; gap: 20px; }.settings-fields>label { display: grid; grid-template-columns: 1fr 240px; gap: 18px; align-items: center; font-size: 15px; }.provider-fields { gap: 0; border: 1px solid var(--border); border-radius: 12px; background: var(--surface); overflow: hidden; }.provider-fields>label { padding: 13px 16px; border-bottom: 1px solid var(--border); }.provider-fields>label:last-child { border-bottom: 0; }.provider-label { display: flex; align-items: center; gap: 11px; font-size: 15px; }.provider-fields select { font-size: 15px; }.unit-field { display: flex; align-items: center; gap: 10px; color: var(--muted); font-size: 15px; }.unit-field input { width: 115px; }.settings-footer { margin-top: 25px; display: flex; justify-content: flex-end; }
	@media(max-width:1150px) { .settings-section { grid-template-columns: 1fr; gap: 22px; }.settings-section>div:first-child { max-width: 600px; } }
	@media(max-width:650px) { .strategy-options { grid-template-columns: 1fr; }.strategy-option { min-height: 0; padding: 16px; }.strategy-option small { font-size: 15px; }.settings-fields>label { grid-template-columns: 1fr; gap: 10px; }.provider-fields>label { grid-template-columns: 1fr 170px; gap: 10px; padding: 13px 12px; }.provider-label { font-size: 15px; gap: 8px; }.provider-fields select { width: 100%; padding: 0 7px; }.settings-footer .button { width: 100%; }.session-note { padding: 15px; } }
</style>
