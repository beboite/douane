<script lang="ts">
	import { untrack } from 'svelte';
	import RefreshCw from '@lucide/svelte/icons/refresh-cw';
	import X from '@lucide/svelte/icons/x';
	import { management, privateLabel, formatDate, newResetRequestId, type Account, type ResetAvailability } from '$lib/management';
	let { account, busy, updated }: { account: Account; busy: string; updated: () => Promise<void> } = $props();
	let availability = $state.raw<ResetAvailability | null>(null);
	let checking = $state(false);
	let saving = $state(false);
	let failure = $state('');
	let result = $state('');
	let pending = $state(false);
	let requestId = '';
	let dialog = $state<HTMLDialogElement>();
	let supported = $derived(account.enabled && ['claude', 'codex'].includes(account.provider));
	let storageKey = $derived(`douane-reset-${account.id}`);
	let canReset = $derived(availability?.status === 'ready' && (availability.available_count ?? 0) > 0);
	async function check() {
		if (!supported || checking) return;
		checking = true; failure = '';
		try { availability = await management.accountResets(account.id); if (availability.pending_request_id) { sessionStorage.setItem(storageKey, availability.pending_request_id); pending = true; } }
		catch (error) { failure = privateLabel((error as Error).message); }
		finally { checking = false; }
	}
	$effect(() => { if (supported) untrack(() => { pending = !!sessionStorage.getItem(storageKey); void check(); }); else availability = null; });
	function open() {
		requestId = availability?.pending_request_id ?? sessionStorage.getItem(storageKey) ?? newResetRequestId();
		failure = ''; result = ''; dialog?.showModal();
	}
	async function redeem() {
		if (saving || busy) return;
		saving = true; failure = '';
		try {
			sessionStorage.setItem(storageKey, requestId); pending = true;
			const response = await management.redeemReset(account.id, requestId);
			const messages = { reset: 'Quota réinitialisé.', already_redeemed: 'Ce reset a déjà été appliqué.', nothing_to_reset: 'Le quota est déjà disponible. Aucun reset utilisé.', no_credit: 'Aucun reset disponible.' };
			result = messages[response.outcome];
			if (!result) throw new Error('Le serveur n’a pas confirmé le résultat du reset.');
			sessionStorage.removeItem(storageKey); pending = false;
			await check(); await updated();
		} catch (error) { failure = privateLabel((error as Error).message); }
		finally { saving = false; }
	}
</script>

{#if supported}
	<div class="reset-control">
		{#if canReset || pending}<button class="button reset-button" disabled={!!busy || checking} onclick={open}><RefreshCw size={17}/>{pending ? 'Reprendre le reset' : `Réinitialiser (${availability?.available_count})`}</button>
		{:else if checking}<span class="reset-status">Vérification des resets</span>
		{:else if availability?.status === 'ready'}<span class="reset-status">0 reset disponible</span>
		{:else if availability?.status !== 'unsupported'}<button class="reset-retry" disabled={!!busy} onclick={check}>Vérifier les resets</button>{/if}
		{#if failure && !dialog?.open}<p class="reset-error" role="alert">{failure}</p>{/if}
		{#if availability?.error && !failure && availability.status !== 'unsupported'}<p class="reset-error">{privateLabel(availability.error)}</p>{/if}
	</div>
	<dialog bind:this={dialog} oncancel={(event) => { if (saving) event.preventDefault(); }}>
		<div class="reset-dialog">
			<header><h2>Réinitialiser le quota</h2><button class="icon-button" aria-label="Fermer la confirmation" disabled={saving} onclick={() => dialog?.close()}><X size={22}/></button></header>
			<p class="reset-account">{privateLabel(account.name)}</p>
			{#if result}<p class="reset-result" role="status">{result}</p><div class="dialog-actions"><button class="button primary" onclick={() => dialog?.close()}>Fermer</button></div>
			{:else}
				<p>Cette action consomme 1 reset pour rétablir le quota du compte. Elle est irréversible.</p>
				{#if availability?.next_expires_at}<p>Le reset qui expire le {formatDate(availability.next_expires_at)} sera utilisé en premier.</p>{/if}
				{#if pending}<p>Une tentative est en attente de confirmation. Réessayer vérifie la même opération.</p>{/if}
				{#if failure}<p class="reset-error" role="alert">{failure}</p>{/if}
				<div class="dialog-actions"><button class="button" disabled={saving} onclick={() => dialog?.close()}>Annuler</button><button class="button primary" disabled={saving || !!busy} onclick={redeem}>{saving ? 'Réinitialisation en cours' : pending ? 'Réessayer le même reset' : 'Utiliser 1 reset'}</button></div>
			{/if}
		</div>
	</dialog>
{/if}

<style>
	.reset-control { display: flex; flex-wrap: wrap; align-items: center; gap: 8px; margin-top: 15px; }
	.reset-button { font-size: 15px; padding: 8px 11px; min-height: 42px; }
	.reset-status { font-size: 15px; color: var(--muted); }
	.reset-retry { padding: 4px 0; border: 0; background: transparent; color: var(--blue); font-size: 15px; }
	.reset-error { flex-basis: 100%; font-size: 15px; color: var(--red); line-height: 1.5; margin: 6px 0; overflow-wrap: anywhere; }
	dialog { max-width: min(520px,calc(100vw - 32px)); width: 100%; padding: 0; border: 1px solid var(--border); border-radius: 16px; background: var(--surface); color: var(--text); box-shadow: 0 24px 100px #0008; }
	dialog::backdrop { background: #030711b5; backdrop-filter: blur(5px); }
	.reset-dialog { padding: 25px; }
	.reset-dialog header { display: flex; align-items: center; justify-content: space-between; gap: 15px; }
	.reset-dialog h2 { font-family: var(--font-display); font-size: 24px; }
	.reset-dialog p { font-size: 16px; color: var(--muted); line-height: 1.6; }
	.reset-dialog .reset-account { color: var(--text); font-weight: 600; }
	.reset-dialog .reset-error { color: var(--red); }
	.reset-dialog .reset-result { color: var(--green); }
	.dialog-actions { display: flex; flex-wrap: wrap; justify-content: flex-end; gap: 10px; margin-top: 24px; }
	@media(max-width:650px) { .reset-dialog { padding: 20px; }.dialog-actions .button { flex: 1; }.reset-dialog h2 { font-size: 22px; } }
</style>
