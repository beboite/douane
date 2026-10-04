<script lang="ts">
	import { onMount } from 'svelte';
	import Gauge from '@lucide/svelte/icons/gauge';
	import LayoutDashboard from '@lucide/svelte/icons/layout-dashboard';
	import Users from '@lucide/svelte/icons/users';
	import Boxes from '@lucide/svelte/icons/boxes';
	import Activity from '@lucide/svelte/icons/activity';
	import Settings2 from '@lucide/svelte/icons/settings-2';
	import Rocket from '@lucide/svelte/icons/rocket';
	import RefreshCw from '@lucide/svelte/icons/refresh-cw';
	import Menu from '@lucide/svelte/icons/menu';
	import X from '@lucide/svelte/icons/x';
	import ChevronRight from '@lucide/svelte/icons/chevron-right';
	import QuotasView from '$lib/components/quotas-view.svelte';
	import AccountsView from '$lib/components/accounts-view.svelte';
	import DataViews from '$lib/components/data-views.svelte';
	import SettingsView from '$lib/components/settings-view.svelte';
	import ConnectDialog from '$lib/components/connect-dialog.svelte';
	import DouaneMark from '$lib/components/douane-mark.svelte';
	import { management, importErrors, type Account, type Quota, type Provider, type Model, type Call, type Status, type Settings, type QuotaDisplay } from '$lib/management';
	const groups = [
		{ label:'Abonnements', items:[{ id:'quotas',label:'Quotas',icon:Gauge },{ id:'overview',label:"Vue d'ensemble",icon:LayoutDashboard },{ id:'accounts',label:'Comptes',icon:Users },{ id:'models',label:'Modèles',icon:Boxes }] },
		{ label:'Service',items:[{ id:'activity',label:'Requêtes',icon:Activity },{ id:'settings',label:'Réglages',icon:Settings2 },{ id:'quickstart',label:'Démarrage rapide',icon:Rocket }] }
	];
	const mobileItems = groups.flatMap(group => group.items).filter(item => ['quotas','accounts','activity','settings'].includes(item.id));
	let screen = $state('quotas');
	let mobileOpen = $state(false);
	let connectOpen = $state(false);
	let accounts = $state.raw<Account[]>([]);
	let quotas = $state.raw<Quota[]>([]);
	let providers = $state.raw<Provider[]>([]);
	let models = $state.raw<Model[]>([]);
	let calls = $state.raw<Call[]>([]);
	let summary = $state.raw<Status | null>(null);
	let settings = $state.raw<Settings | null>(null);
	let loading = $state(true);
	let busy = $state('');
	let failure = $state('');
	let notice = $state('');
	let pageTitle = $derived(groups.flatMap(g => g.items).find(item => item.id === screen)?.label ?? 'Quotas');
	let loadingRequest: Promise<void> | null = null;
	async function load(silent = false) {
		if (loadingRequest) return loadingRequest;
		if (!silent) loading = true;
		loadingRequest = (async () => {
			const responses = await Promise.allSettled([management.accounts(),management.quotas(),management.providers(),management.models(),management.calls(),management.status(),management.settings()]);
			const errors: string[] = [];
			if (responses[0].status === 'fulfilled') accounts = responses[0].value.accounts; else errors.push(responses[0].reason.message);
			if (responses[1].status === 'fulfilled') quotas = responses[1].value.quotas; else errors.push(responses[1].reason.message);
			if (responses[2].status === 'fulfilled') providers = responses[2].value.providers; else errors.push(responses[2].reason.message);
			if (responses[3].status === 'fulfilled') models = responses[3].value.models; else errors.push(responses[3].reason.message);
			if (responses[4].status === 'fulfilled') calls = responses[4].value.calls; else errors.push(responses[4].reason.message);
			if (responses[5].status === 'fulfilled') summary = responses[5].value; else { summary = null; errors.push(responses[5].reason.message); }
			if (responses[6].status === 'fulfilled') settings = responses[6].value; else errors.push(responses[6].reason.message);
			failure = [...new Set(errors)].join(' ');
			loading = false;
		})();
		try { await loadingRequest; } finally { loadingRequest = null; }
	}
	function navigate(next: string) { screen = next; mobileOpen = false; failure = ''; notice = ''; location.hash = next; window.scrollTo(0,0); }
	function hashChange() { const hash = location.hash.slice(1); if (groups.flatMap(g => g.items).some(item => item.id === hash)) screen = hash; }
	async function mutate(name: string, action: () => Promise<unknown>, message = '') {
		if (busy) return; busy = name; failure = ''; notice = '';
		try { await action(); if (loadingRequest) await loadingRequest; await load(true); notice = message; }
		catch (error) { failure = (error as Error).message; }
		finally { busy = ''; }
	}
	function refresh(id?: string) { return mutate('quotas', () => management.refreshQuotas(id)); }
	function setQuotaDisplay(provider: string, display: QuotaDisplay) {
		const next = { ...settings?.quota_display };
		if (display === 'average') next[provider] = display; else delete next[provider];
		return mutate('settings', () => management.settingsSave({ quota_display: next }), 'Affichage dans Boite enregistré.');
	}
	async function rename(account: Account, name: string) {
		if (busy) throw new Error('Une action est déjà en cours.');
		busy = 'rename';
		try { const result = await management.renameAccount(account.id, name); accounts = accounts.map(item => item.id === account.id ? result.account : item); }
		finally { busy = ''; }
	}
	async function importFiles(files: FileList) {
		const selected = Array.from(files);
		await mutate('import',async () => {
			const contents = await Promise.all(selected.map(async file => { if (file.size > 2*1024*1024) throw new Error(`${file.name} dépasse la limite de 2 Mo.`); try { return { name:file.name,content:JSON.parse(await file.text()) }; } catch { throw new Error(`${file.name} : le fichier doit contenir du JSON valide.`); } }));
			const result = await management.importAccounts(contents);
			if (result.errors?.length) { await load(true); throw new Error(`${result.imported} compte(s) importé(s). ${importErrors(result.errors)}`); }
			if (!result.imported) throw new Error("Le fichier n'a ajouté aucun compte.");
		},'Comptes importés.');
	}
	onMount(() => { hashChange(); void load(); const timer = setInterval(() => { if (!document.hidden && !busy) void load(true); },15_000); return () => clearInterval(timer); });
</script>

<svelte:head><title>{pageTitle} · douane</title><meta name="description" content="Gérez vos abonnements, leurs quotas et les requêtes de vos outils."/></svelte:head>
<svelte:window onhashchange={hashChange} onkeydown={(event) => { if (event.key === 'Escape') mobileOpen = false; }}/>
<svelte:document onvisibilitychange={() => { if (!document.hidden) void load(true); }}/>

<div class="app-shell">
	{#if mobileOpen}<button class="nav-backdrop" aria-label="Fermer le menu" onclick={() => mobileOpen = false}></button>{/if}
	<aside id="main-navigation" class="sidebar" class:mobile-open={mobileOpen}>
		<div class="brand"><span class="brand-symbol"><DouaneMark/></span><span>douane</span><button class="icon-button mobile-close" aria-label="Fermer le menu" onclick={() => mobileOpen = false}><X size={18}/></button></div>
		<nav aria-label="Navigation principale">{#each groups as group (group.label)}<div class="nav-group"><p>{group.label}</p>{#each group.items as item (item.id)}{@const Icon = item.icon}<button class="nav-link" class:active={screen === item.id} aria-current={screen === item.id ? 'page' : undefined} onclick={() => navigate(item.id)}><Icon size={17}/><span>{item.label}</span>{#if item.id === 'accounts' && accounts.length}<span class="nav-count">{accounts.length}</span>{/if}</button>{/each}</div>{/each}</nav>
		<div class="sidebar-bottom"><button class="sidebar-help" onclick={() => navigate('quickstart')}>Configurer un outil<ChevronRight size={19}/></button></div>
	</aside>
	<div class="workspace">
		<main id="main-content">
			{#if failure}<div class="notice error" role="alert"><span>{failure}</span><button class="button small" onclick={() => load()} disabled={loading}>Réessayer</button></div>{/if}
			{#if notice}<div class="notice success" role="status">{notice}<button class="icon-button" onclick={() => notice = ''} aria-label="Fermer la notification"><X size={14}/></button></div>{/if}
			{#if loading && !summary}<div class="loading-state" role="status"><RefreshCw size={18} class="spinning"/><span>Chargement de vos abonnements...</span></div>{/if}
			{#if screen === 'quotas'}<QuotasView {accounts} {quotas} {busy} {refresh} {rename} {settings} {setQuotaDisplay} resetUpdated={() => load(true)} connect={() => connectOpen = true}/>
			{:else if screen === 'accounts'}<AccountsView {accounts} {quotas} {busy} {rename} connect={() => connectOpen = true} {importFiles} toggle={(account) => mutate('account',() => management.setAccountEnabled(account.id,!account.enabled),account.enabled ? 'Compte désactivé.' : 'Compte activé.')} remove={(account) => mutate('account',() => management.deleteAccount(account.id),'Compte supprimé.')}/>
			{:else if screen === 'settings'}{#if settings}{#key JSON.stringify(settings)}<SettingsView {settings} {providers} {busy} save={(next) => mutate('settings',() => management.settingsSave(next),'Réglages enregistrés.')}/>{/key}{:else}<div class="page-heading"><div><h1>Réglages</h1><p class="page-description">Les réglages seront disponibles lorsque le serveur répondra.</p></div></div>{/if}
			{:else}{#key screen}<DataViews {screen} {summary} {calls} {models} {accounts} {busy} {navigate} toggleModel={(model) => mutate('model',() => management.setModelEnabled(model.id,model.enabled === false),'Modèle mis à jour.')}/>{/key}{/if}
		</main>
	</div>
</div>
<nav class="mobile-nav" aria-label="Navigation mobile">{#each mobileItems as item (item.id)}{@const Icon = item.icon}<button class:active={screen === item.id} aria-current={screen === item.id ? 'page' : undefined} onclick={() => navigate(item.id)}><Icon size={19}/><span>{item.label}</span></button>{/each}<button onclick={() => mobileOpen = true} aria-label="Ouvrir le menu" aria-expanded={mobileOpen} aria-controls="main-navigation"><Menu size={19}/><span>Menu</span></button></nav>
{#if connectOpen}<ConnectDialog {providers} close={() => connectOpen = false} completed={() => { notice = 'Compte connecté.'; void load(true); }} {importFiles}/>{/if}
