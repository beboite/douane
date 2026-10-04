<script lang="ts">
	import Search from '@lucide/svelte/icons/search';
	import RefreshCw from '@lucide/svelte/icons/refresh-cw';
	import Plus from '@lucide/svelte/icons/plus';
	import ExternalLink from '@lucide/svelte/icons/external-link';
	import ProviderMark from './provider-mark.svelte';
	import QuotaMeter from './quota-meter.svelte';
	import AccountName from './account-name.svelte';
	import PrivateEmail from './private-email.svelte';
	import AccountResets from './account-resets.svelte';
	import EmptyState from './empty-state.svelte';
	import { providerName, formatDate, resetLabel, creditValue, usageLabel, privateLabel, accountStatus, accountWarning, type Account, type Quota, type Settings, type QuotaDisplay } from '$lib/management';
	let { accounts, quotas, busy, refresh, connect, rename, resetUpdated, settings, setQuotaDisplay }: { accounts: Account[]; quotas: Quota[]; busy: string; refresh: (id?: string) => void; connect: () => void; rename: (account: Account, name: string) => Promise<void>; resetUpdated: () => Promise<void>; settings: Settings | null; setQuotaDisplay: (provider: string, display: QuotaDisplay) => Promise<void> } = $props();
	let provider = $state('all');
	let search = $state('');
	let sort = $state('name');
	const order = ['claude', 'codex', 'opencode_go', 'xai'];
	let providerIds = $derived([...new Set(accounts.map(a => a.provider))].sort((a, b) => (order.indexOf(a) < 0 ? 99 : order.indexOf(a)) - (order.indexOf(b) < 0 ? 99 : order.indexOf(b)) || providerName(a).localeCompare(providerName(b))));
	let filtered = $derived(accounts.filter(a => (provider === 'all' || !providerIds.includes(provider) || a.provider === provider) && `${a.name} ${a.email ?? ''}`.toLowerCase().includes(search.toLowerCase())).sort((a,b) => sort === 'remaining' ? (remaining(a.id) ?? 101) - (remaining(b.id) ?? 101) : a.name.localeCompare(b.name)));
	let groups = $derived(providerIds.map(id => ({ id, accounts: filtered.filter(a => a.provider === id) })).filter(group => group.accounts.length));
	function display(id: string): QuotaDisplay { return settings?.quota_display?.[id] ?? 'accounts'; }
	async function chooseDisplay(id: string, select: HTMLSelectElement) { await setQuotaDisplay(id, select.value as QuotaDisplay); select.value = display(id); }
	function remaining(id: string) { const values = quotas.find(q => q.account_id === id)?.windows.map(w => w.remaining_percent).filter((v): v is number => v != null) ?? []; return values.length ? Math.min(...values) : null; }
</script>

<div class="page-heading">
	<h1>Quotas</h1>
	<div class="heading-actions"><button class="button" disabled={!!busy || !accounts.length} onclick={() => refresh()}><RefreshCw size={18} class={busy === 'quotas' ? 'spinning' : ''}/>Actualiser les quotas</button><button class="button primary" onclick={connect}><Plus size={20}/>Connecter un compte</button></div>
</div>

{#if providerIds.length > 1}
	<div class="provider-tabs" aria-label="Filtrer par fournisseur">
		<button class:active={provider === 'all'} onclick={() => provider = 'all'} aria-pressed={provider === 'all'}>Tous <span class="count-badge">{accounts.length}</span></button>
		{#each providerIds as id (id)}<button class:active={provider === id} onclick={() => provider = id} aria-pressed={provider === id}><ProviderMark provider={id} size={22}/>{providerName(id)}<span class="count-badge">{accounts.filter(a => a.provider === id).length}</span></button>{/each}
	</div>
{/if}

<div class="section-toolbar quota-toolbar"><label class="search-input"><Search size={18}/><input type="search" placeholder="Rechercher" aria-label="Rechercher un compte" bind:value={search}/></label><select aria-label="Trier les comptes" bind:value={sort}><option value="name">Par nom</option><option value="remaining">Quota restant</option></select></div>

{#if !accounts.length}<section class="ledger"><EmptyState title="Aucun compte connecté" description="Connectez un abonnement pour afficher ses quotas." action="Connecter un compte" onclick={connect}/></section>
{:else if !filtered.length}<section class="ledger"><EmptyState title="Aucun compte correspondant" description="Essayez un autre nom ou fournisseur."/></section>
{:else}
	<div class="provider-groups">
		{#each groups as group (group.id)}
			<section class="provider-group" aria-label="Quotas {providerName(group.id)}">
				<header class="provider-heading"><ProviderMark provider={group.id} size={29}/><h2>{providerName(group.id)}</h2><span>{group.accounts.length} {group.accounts.length === 1 ? 'compte' : 'comptes'}</span>{#if settings}<label class="display-choice" title="Affichage des quotas {providerName(group.id)} dans Boite et les clients de /v1/quotas"><span>Dans Boite / les clients</span><select value={display(group.id)} disabled={!!busy} onchange={(event) => chooseDisplay(group.id, event.currentTarget)}><option value="accounts">Détail par compte</option><option value="average">Moyenne des comptes</option></select></label>{/if}</header>
				<div class="ledger">
					{#each group.accounts as account (account.id)}
						{@const quota = quotas.find(q => q.account_id === account.id)}
						{@const subscription = quota?.subscription ?? account.subscription}
						<article class="quota-row" class:disabled-row={!account.enabled}>
							<div class="account-identity"><AccountName {account} {rename} {busy}/>{#if subscription?.label || account.plan}<p class="account-plan">{subscription?.label ?? account.plan}</p>{/if}{#if account.email}<PrivateEmail email={account.email}/>{/if}<AccountResets {account} {busy} updated={resetUpdated}/></div>
							<div class="quota-details">
								{#if quota?.verification_required}<div class="verification-needed"><p>Google demande de vérifier ce compte.</p>{#if quota.verification_available}<a class="button" href={`/admin/api/accounts/${encodeURIComponent(account.id)}/verification`} target="_blank" rel="noopener noreferrer" data-sveltekit-reload>Vérifier le compte Google<ExternalLink size={16}/></a>{:else}<p>Ouvrez Antigravity avec ce compte pour terminer la vérification.</p>{/if}</div>{/if}
								<div class="quota-windows">{#if quota?.windows.length}{#each quota.windows as window (window.id)}<div class="quota-window"><QuotaMeter value={window.remaining_percent} label={usageLabel(window.label)}/><p class="reset-date" title={window.reset_at ? `Réinitialisation : ${formatDate(window.reset_at)}` : undefined}><RefreshCw size={16}/>{resetLabel(window.reset_at)}</p></div>{/each}{:else if !quota?.verification_required}<p class="quota-unavailable">{quota?.error ? privateLabel(quota.error) : 'Quota non communiqué'}</p>{/if}</div>
								{#if quota?.credits?.length}<div class="credit-windows">{#each quota.credits as credit (credit.id)}<div class="credit-window"><div><span>{usageLabel(credit.label)}</span><strong>{credit.enabled === false ? 'Désactivé' : credit.unlimited ? 'Illimités' : creditValue(credit.remaining, credit.unit)}{#if credit.enabled !== false && credit.remaining != null}<span class="muted">{' restants'}</span>{/if}</strong></div>{#if credit.used != null}<p>{creditValue(credit.used, credit.unit)}{' utilisés'}{#if credit.limit != null}{' sur '}{creditValue(credit.limit, credit.unit)}{/if}</p>{/if}{#if credit.reset_at}<p class="reset-date"><RefreshCw size={16}/>{resetLabel(credit.reset_at)}</p>{/if}</div>{/each}</div>{/if}
							</div>
							<div class="account-state"><span class="state-badge" class:warning={!!quota?.error || accountWarning(account)} class:inactive={!account.enabled}><span class="status-pip"></span>{!account.enabled ? 'Désactivé' : quota?.error ? 'À vérifier' : accountStatus(account)}</span><button class="icon-button" disabled={!!busy} onclick={() => refresh(account.id)} title="Actualiser ce compte" aria-label="Actualiser {privateLabel(account.name)}"><RefreshCw size={19}/></button></div>
						</article>
					{/each}
				</div>
			</section>
		{/each}
	</div>
{/if}

<style>
	.quota-toolbar { justify-content: flex-end; margin: 20px 0 26px; }
	.quota-toolbar>select { background: var(--surface); font-size: 15px; }
	.provider-groups { display: grid; gap: 30px; }
	.provider-heading { display: flex; align-items: center; gap: 12px; margin: 0 0 14px; }
	.provider-heading h2 { font-family: var(--font-display); font-size: 23px; font-weight: 600; }
	.provider-heading>span { margin-left: auto; font-size: 15px; color: var(--muted); }
	.display-choice { display: flex; align-items: center; gap: 10px; margin-left: 10px; font-size: 15px; color: var(--muted); }
	.display-choice select { background: var(--surface); font-size: 15px; }
	.quota-row { display: grid; grid-template-columns: minmax(220px,.8fr) minmax(320px,1.8fr) auto; gap: 26px; padding: 25px 24px; border-bottom: 1px solid var(--border); align-items: center; }
	.quota-row:last-child { border-bottom: 0; }
	.account-identity { min-width: 0; }
	.account-plan { color: var(--muted); font-size: 15px; margin: 4px 0; }
	.quota-details { min-width: 0; }
	.verification-needed { display: grid; justify-items: start; gap: 12px; margin-bottom: 16px; }
	.verification-needed p { margin: 0; color: var(--muted); font-size: 16px; line-height: 1.5; }
	.quota-windows { display: grid; grid-template-columns: repeat(2,minmax(130px,1fr)); gap: 23px 28px; }
	.reset-date { display: flex; align-items: center; gap: 7px; font-size: 15px; color: var(--muted); margin: 11px 0 0; line-height: 1.4; }
	.reset-date :global(svg) { flex-shrink: 0; color: var(--faint); }
	.quota-unavailable { grid-column: 1/-1; font-size: 16px; color: var(--muted); margin: 0; }
	.account-state { display: flex; align-items: center; justify-content: flex-end; gap: 8px; }
	.disabled-row { opacity: .65; }
	.credit-windows { display: grid; gap: 18px; margin-top: 23px; padding-top: 18px; border-top: 1px solid var(--border); }
	.credit-window>div { display: flex; justify-content: space-between; gap: 12px; font-size: 15px; }
	.credit-window strong { font-weight: 600; }
	.credit-window strong>span { font-weight: 400; }
	.credit-window>p { margin: 8px 0 0; color: var(--muted); font-size: 15px; }
	@media(max-width:1250px) { .quota-row { grid-template-columns: minmax(190px,.7fr) minmax(260px,1.3fr); gap: 22px; }.account-state { grid-column: 1/-1; margin-top: -8px; } }
	@media(max-width:650px) { .quota-toolbar { flex-wrap: nowrap; margin: 18px 0 25px; gap: 10px; }.quota-toolbar>select { max-width: 140px; padding: 0 8px; }.quota-toolbar>.search-input { min-width: 0; width: auto; }.provider-groups { gap: 28px; }.provider-heading { flex-wrap: wrap; }.provider-heading h2 { font-size: 22px; }.display-choice { flex-basis: 100%; justify-content: space-between; margin-left: 0; }.display-choice select { max-width: 200px; padding: 0 8px; }.quota-row { grid-template-columns: 1fr; gap: 22px; padding: 20px 18px; }.quota-windows { grid-template-columns: 1fr; gap: 23px; }.account-state { grid-column: auto; justify-content: space-between; margin-top: 0; }.credit-window>div { flex-wrap: wrap; } }
</style>
