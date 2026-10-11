<script lang="ts">
	import Upload from '@lucide/svelte/icons/upload';
	import Plus from '@lucide/svelte/icons/plus';
	import Trash2 from '@lucide/svelte/icons/trash-2';
	import Search from '@lucide/svelte/icons/search';
	import ProviderMark from './provider-mark.svelte';
	import AccountName from './account-name.svelte';
	import PrivateEmail from './private-email.svelte';
	import EmptyState from './empty-state.svelte';
	import AccountReserve from './account-reserve.svelte';
	import { providerName, formatDate, privateLabel, accountStatus, accountWarning, type Account, type Quota } from '$lib/management';
	let { accounts, quotas, busy, connect, importFiles, toggle, remove, rename, setReserve }: { accounts: Account[]; quotas: Quota[]; busy: string; connect: () => void; importFiles: (files: FileList) => void; toggle: (account: Account) => void; remove: (account: Account) => void; rename: (account: Account, name: string) => Promise<void>; setReserve: (account: Account, reserve: number | null) => Promise<boolean> } = $props();
	let search = $state('');
	let confirmation = $state<string | null>(null);
	let fileInput: HTMLInputElement;
	let filtered = $derived(accounts.filter(a => `${a.name} ${a.provider} ${a.email ?? ''}`.toLowerCase().includes(search.toLowerCase())));
	let dragging = $state(false);
</script>

<div class="page-heading"><h1>Comptes</h1><div class="heading-actions"><button class="button" onclick={() => fileInput.click()} disabled={!!busy}><Upload size={18}/>Importer</button><button class="button primary" onclick={connect}><Plus size={20}/>Connecter un compte</button></div></div>
<input hidden type="file" accept=".json,application/json" multiple bind:this={fileInput} onchange={(event) => { if (event.currentTarget.files?.length) importFiles(event.currentTarget.files); event.currentTarget.value = ''; }}/>
<div class="section-toolbar"><p>{accounts.length} {accounts.length === 1 ? 'compte' : 'comptes'}</p><label class="search-input"><Search size={18}/><input type="search" placeholder="Rechercher un compte" aria-label="Rechercher un compte" bind:value={search}/></label></div>
<div class="drop-zone" class:dragging role="region" aria-label="Déposer des fichiers d'authentification" ondragover={(event) => { event.preventDefault(); dragging = true; }} ondragleave={() => dragging = false} ondrop={(event) => { event.preventDefault(); dragging = false; if (event.dataTransfer?.files.length) importFiles(event.dataTransfer.files); }}><Upload size={19}/><span>Déposez vos fichiers JSON pour importer des comptes</span></div>
<section class="ledger">
	{#if !accounts.length}<EmptyState title="Aucun compte connecté" description="Connectez un abonnement ou importez son fichier JSON." action="Connecter un compte" onclick={connect}/>
	{:else if !filtered.length}<EmptyState title="Aucun compte correspondant" description="Essayez un autre nom ou fournisseur."/>
	{:else}<div class="account-table-wrap"><table><thead><tr><th>Compte</th><th>Abonnement</th><th>État</th><th>Réserve</th><th>Quotas vérifiés</th><th>Actions</th></tr></thead><tbody>{#each filtered as account (account.id)}{@const quota = quotas.find(q => q.account_id === account.id)}<tr><td><AccountName {account} {rename} {busy}/>{#if account.email}<PrivateEmail email={account.email}/>{/if}{#if account.last_error}<p class="inline-error">{privateLabel(account.last_error)}</p>{/if}</td><td><span class="inline-provider"><ProviderMark provider={account.provider} size={23}/>{providerName(account.provider)}</span>{#if quota?.subscription?.label || account.subscription?.label || account.plan}<div class="account-plan">{quota?.subscription?.label ?? account.subscription?.label ?? account.plan}</div>{/if}</td><td><span class="state-badge" class:inactive={!account.enabled} class:warning={accountWarning(account)}><span class="status-pip"></span>{accountStatus(account)}</span></td><td class="reserve-cell"><AccountReserve {account} {busy} save={setReserve}/></td><td class="muted verified-at">{quota?.updated_at ? formatDate(quota.updated_at) : 'Non vérifiés'}</td><td><div class="row-actions"><button class="switch" role="switch" aria-checked={account.enabled} aria-label="Activer {privateLabel(account.name)}" class:on={account.enabled} disabled={!!busy} onclick={() => toggle(account)}><span></span></button><button class="icon-button" aria-label="Supprimer {privateLabel(account.name)}" title="Supprimer ce compte" disabled={!!busy} onclick={() => confirmation = account.id}><Trash2 size={20}/></button></div>{#if confirmation === account.id}<div class="delete-confirmation"><p>Supprimer ce compte de douane ?</p><button class="button danger" disabled={!!busy} onclick={() => { remove(account); confirmation = null; }}>Supprimer</button><button class="button" onclick={() => confirmation = null}>Annuler</button></div>{/if}</td></tr>{/each}</tbody></table></div>{/if}
</section>

<style>
	.drop-zone { display: flex; gap: 10px; align-items: center; justify-content: center; border: 1px dashed #3c5279; border-radius: 11px; background: #111b2e80; padding: 20px; color: var(--muted); font-size: 15px; line-height: 1.5; margin-bottom: 18px; }
	.drop-zone.dragging { background: #5478f612; border-color: var(--blue); color: var(--text); }
	.drop-zone :global(svg) { flex-shrink: 0; }
	.account-table-wrap { overflow-x: auto; }
	td:first-child { min-width: 200px; }
	.inline-provider { display: inline-flex; gap: 9px; align-items: center; font-size: 16px; white-space: nowrap; }
	.account-plan { font-size: 15px; color: var(--muted); margin-top: 7px; }
	.reserve-cell { min-width: 190px; }
	.reserve-cell :global(.reserve), .reserve-cell :global(.reserve-editor) { margin-top: 0; }
	.row-actions { display: flex; gap: 14px; align-items: center; }
	.inline-error { max-width: 350px; font-size: 15px; color: var(--red); }
	.delete-confirmation { font-size: 15px; min-width: 210px; margin-top: 12px; }
	.delete-confirmation p { margin: 0 0 10px; }.delete-confirmation .button { margin-right: 5px; }
	@media(max-width:900px) { table { display: block; } thead { display: none; } tbody { display: block; } tr { display: grid; grid-template-columns: 1fr auto; padding: 22px 20px; border-bottom: 1px solid var(--border); gap: 18px; } tr:last-child { border: 0; } td { padding: 0; border: 0; } td:first-child, .reserve-cell { grid-column: 1/-1; min-width: 0; } td:last-child { display: flex; justify-content: flex-end; }.row-actions { gap: 20px; }.verified-at::before { content: 'Quotas vérifiés : '; } }
	@media(max-width:650px) { .drop-zone { padding: 17px; } tr { padding: 20px 18px; }.delete-confirmation { min-width: 0; } }
</style>
