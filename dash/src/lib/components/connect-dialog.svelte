<script lang="ts">
	import { onMount } from 'svelte';
	import X from '@lucide/svelte/icons/x';
	import ExternalLink from '@lucide/svelte/icons/external-link';
	import Upload from '@lucide/svelte/icons/upload';
	import Check from '@lucide/svelte/icons/check';
	import Copy from '@lucide/svelte/icons/copy';
	import ChevronRight from '@lucide/svelte/icons/chevron-right';
	import ProviderMark from './provider-mark.svelte';
	import { copyText } from '$lib/clipboard';
	import { management, importErrors, type Provider, type OAuthSession } from '$lib/management';
	let { providers, close, completed, importFiles }: { providers: Provider[]; close: () => void; completed: () => void; importFiles: (files: FileList) => void } = $props();
	let dialog: HTMLDialogElement;
	let fileInput = $state<HTMLInputElement>();
	let session = $state.raw<OAuthSession | null>(null);
	let callback = $state('');
	let pending = $state(false);
	let failure = $state('');
	let success = $state(false);
	let terminal = $state(false);
	let status = $state('');
	let manual = $state('');
	let accountName = $state('');
	let credential = $state('');
	let endpoint = $state('');
	let codeCopied = $state(false);
	const copyFailureMessage = 'Copie refusée par le navigateur. Sélectionnez le code pour le copier.';
	let mounted = false;
	const endpoints: Record<string,string> = { opencode_go:'https://opencode.ai/zen/go',xai:'https://cli-chat-proxy.grok.com/v1',openai_compatible:'',kimi:'https://api.kimi.com/coding/v1',qwen:'https://portal.qwen.ai/v1' };
	onMount(() => { mounted = true; dialog.showModal(); return () => { mounted = false; dialog.close(); credential = ''; callback = ''; }; });
	async function start(provider: string) {
		pending = true; failure = ''; callback = ''; status = ''; terminal = false; codeCopied = false;
		try { const attempt = await management.startOAuth(provider); if (mounted) { session = attempt; status = attempt.user_code ? "En attente d'autorisation. La connexion est vérifiée automatiquement." : ''; } }
		catch (error) { if (mounted) failure = (error as Error).message; }
		finally { pending = false; }
	}
	function connected() { success = true; terminal = true; callback = ''; credential = ''; completed(); }
	async function check(manualCheck = true) {
		const attempt = session; if (!attempt || pending || terminal) return attempt?.interval ?? 5;
		pending = true; if (manualCheck) failure = '';
		try {
			const result = await management.oauth(attempt.id);
			if (!mounted || session?.id !== attempt.id) return 5;
			if ((result.account_id || result.account?.id) && ['complete','completed','connected','success'].includes(result.status)) connected();
			else if (result.status === 'expired') { terminal = true; failure = 'Cette connexion a expiré. Choisissez le fournisseur pour recommencer.'; }
			else if (result.error || result.status === 'error') { terminal = true; failure = result.error || "Le fournisseur a refusé la connexion. Recommencez pour réessayer."; }
			else status = result.status === 'exchanging' ? 'Autorisation reçue, connexion du compte en cours.' : attempt.user_code ? "En attente d'autorisation. La connexion est vérifiée automatiquement." : 'La connexion attend encore votre autorisation.';
			return Math.min(60,Math.max(5,result.interval ?? attempt.interval ?? 5));
		} catch (error) { if (mounted) failure = (error as Error).message; return attempt.interval ?? 5; }
		finally { pending = false; }
	}
	$effect(() => {
		const attempt = session; if (!attempt?.user_code || success || terminal) return;
		let stopped = false;
		let timer: ReturnType<typeof setTimeout>;
		async function poll() { const interval = document.hidden ? attempt?.interval ?? 5 : await check(false); if (!stopped) timer = setTimeout(poll,(interval ?? 5)*1000); }
		timer = setTimeout(poll,Math.max(5,attempt.interval ?? 5)*1000);
		return () => { stopped = true; clearTimeout(timer); };
	});
	async function finish(event: SubmitEvent) {
		event.preventDefault(); const attempt = session; if (!attempt || pending) return;
		pending = true; failure = '';
		try { const result = await management.oauthCallback(attempt.id,callback.trim(),attempt.state); if (!result.account?.id) throw new Error("Le serveur n'a pas confirmé l'ajout du compte."); if (mounted) connected(); }
		catch (error) { if (mounted) failure = (error as Error).message; }
		finally { pending = false; }
	}
	async function saveCredential(event: SubmitEvent) {
		event.preventDefault(); pending = true; failure = '';
		try { const result = await management.importCredential({ type:manual,name:accountName,access_token:credential,base_url:endpoint }); if (result.errors?.length || !result.imported) throw new Error(result.errors?.length ? importErrors(result.errors) : "Le compte n'a pas été ajouté."); if (mounted) connected(); }
		catch (error) { if (mounted) failure = (error as Error).message; }
		finally { pending = false; }
	}
	function reset() { session = null; manual = ''; credential = ''; callback = ''; failure = ''; status = ''; terminal = false; }
	async function copyCode() { if (!session?.user_code) return; try { await copyText(session.user_code); codeCopied = true; if (failure === copyFailureMessage) failure = ''; } catch { codeCopied = false; failure = copyFailureMessage; } }
</script>

<dialog bind:this={dialog} oncancel={close} class="connect-dialog">
	<div class="dialog-heading"><div><h2>Connecter un compte</h2><p>Ajoutez un abonnement à douane.</p></div><button class="icon-button" onclick={close} aria-label="Fermer"><X size={18}/></button></div>
	{#if success}
		<div class="connection-success"><span><Check size={24}/></span><h3>Compte connecté</h3><p>Votre abonnement est maintenant disponible dans douane.</p><button class="button primary" onclick={close}>Voir mes comptes</button></div>
	{:else if session}
		<div class="oauth-flow">
			{#if session.user_code}<p>Ouvrez la page Grok et saisissez ce code pour autoriser la connexion.</p><div class="device-code"><span>{session.user_code}</span><button class="icon-button" onclick={copyCode} aria-label="Copier le code">{#if codeCopied}<Check size={16}/>{:else}<Copy size={16}/>{/if}</button></div>{:else}<p>Ouvrez la page du fournisseur et autorisez la connexion.</p>{/if}
			<a class="button primary" href={session.url} target="_blank" rel="noopener noreferrer external" data-sveltekit-reload>Ouvrir la connexion<ExternalLink size={14}/></a>
			{#if !session.user_code}<p class="flow-hint">Si le retour vers localhost ne charge pas, copiez son URL complète. Claude peut aussi afficher un code à coller ici.</p><form onsubmit={finish}><label for="callback">URL de retour ou code de connexion</label><input id="callback" type="text" placeholder="URL de retour ou code#state" bind:value={callback} required maxlength="8192" autocomplete="off" spellcheck="false"/><button class="button primary" disabled={pending || terminal || !callback.trim()}>Terminer la connexion</button></form>{/if}
			<div class="flow-controls"><button class="button" disabled={pending || terminal} onclick={() => check()}>Vérifier la connexion</button><button class="button subtle" disabled={pending} onclick={reset}>Choisir un autre fournisseur</button></div>{#if status}<p class="muted small" role="status">{status}</p>{/if}
		</div>
	{:else if manual}
		<div class="oauth-flow"><p>Ajoutez le jeton de connexion de votre abonnement. Il disparaît de ce formulaire après l'enregistrement.</p><form onsubmit={saveCredential}><label for="account-name">Nom du compte</label><input id="account-name" bind:value={accountName} placeholder="Mon abonnement" required/><label for="credential">Jeton de connexion</label><input id="credential" type="password" bind:value={credential} required autocomplete="off" spellcheck="false"/><label for="endpoint">Adresse du fournisseur</label><input id="endpoint" type="url" bind:value={endpoint} required placeholder="https://votre-fournisseur/v1"/><button class="button primary" disabled={pending || !credential.trim()}>Ajouter le compte</button></form><button class="button subtle" style="margin-top:15px" disabled={pending} onclick={reset}>Choisir un autre fournisseur</button></div>
	{:else}
		<div class="provider-options">{#each providers as provider (provider.id)}<button class="provider-option" disabled={pending} onclick={() => { if (provider.oauth) void start(provider.id); else { manual = provider.id; endpoint = provider.base_url ?? endpoints[provider.id] ?? ''; accountName = provider.name; } }}><span class="provider-logo"><ProviderMark provider={provider.id} size={28}/></span><span>{provider.name}<small>{provider.oauth ? 'Connecter mon abonnement' : "Ajouter un jeton"}</small></span><ChevronRight size={15}/></button>{/each}{#if !providers.length}<p class="muted small">Aucun fournisseur disponible. Importez un fichier d'authentification pour ajouter un compte.</p>{/if}</div>
		<div class="dialog-import"><div><h3>Vous avez déjà un fichier ?</h3><p>Importez un ou plusieurs fichiers d'authentification JSON.</p></div><button class="button" disabled={pending} onclick={() => fileInput?.click()}><Upload size={14}/>Importer</button><input hidden type="file" accept=".json,application/json" multiple bind:this={fileInput} onchange={(event) => { if (event.currentTarget.files?.length) { importFiles(event.currentTarget.files); close(); } }}/></div>
	{/if}
	{#if pending}<p class="muted small" role="status">Connexion en cours...</p>{/if}
	{#if failure}<div class="notice error" role="alert">{failure}</div>{/if}
</dialog>

<style>
	.connect-dialog { width: min(640px,calc(100vw - 32px)); max-height: calc(100dvh - 32px); overflow-y: auto; padding: 28px; color: var(--text); background: var(--surface); border: 1px solid #35496b; border-radius: 18px; box-shadow: 0 24px 80px #030711b0; }.connect-dialog::backdrop { background: #030711b5; backdrop-filter: blur(6px); }
	.dialog-heading { position: sticky; top: 0; z-index: 2; display: flex; gap: 20px; align-items: flex-start; justify-content: space-between; padding-bottom: 18px; margin-bottom: 8px; background: var(--surface); }.dialog-heading h2 { font-family: var(--font-display); font-size: 26px; font-weight: 650; letter-spacing: -.035em; margin: 0 0 8px; }.dialog-heading p,.dialog-import p { color: var(--muted); font-size: 15px; line-height: 1.5; margin: 0; }
	.provider-options { display: grid; grid-template-columns: repeat(2,minmax(0,1fr)); gap: 11px; }.provider-option { display: flex; gap: 12px; align-items: center; min-height: 86px; padding: 15px 12px; border: 1px solid var(--border); border-radius: 12px; background: var(--background); text-align: left; }.provider-option:hover { background: var(--surface-raised); border-color: #5576ad; }.provider-option>span:not(.provider-logo) { flex: 1; font-size: 15px; font-weight: 550; }.provider-logo { display: flex; align-items: center; justify-content: center; width: 36px; flex-shrink: 0; }.provider-option small { display: block; color: var(--muted); font-size: 15px; font-weight: 400; line-height: 1.5; margin-top: 5px; }.provider-option :global(svg) { color: var(--faint); flex-shrink: 0; }
	.dialog-import { margin-top: 24px; padding-top: 22px; border-top: 1px solid var(--border); display: flex; gap: 18px; align-items: center; }.dialog-import h3 { font-size: 15px; margin: 0 0 6px; }
	.oauth-flow>p { font-size: 15px; color: var(--muted); line-height: 1.6; }.oauth-flow .flow-hint { margin-top: 24px; font-size: 15px; }.oauth-flow form { display: grid; gap: 10px; }.oauth-flow label { font-size: 15px; }.oauth-flow input { width: 100%; }.flow-controls { display: flex; flex-wrap: wrap; gap: 8px; margin-top: 18px; }
	.device-code { display: flex; align-items: center; justify-content: space-between; border: 1px solid var(--border); background: var(--background); border-radius: 10px; padding: 14px 17px; margin: 20px 0; }.device-code>span { font-family: var(--font-display); font-size: 28px; letter-spacing: .08em; }
	.connection-success { padding: 15px 0; text-align: center; }.connection-success>span { display: inline-grid; place-items: center; width: 50px; height: 50px; background: #10b98115; color: var(--green); border-radius: 50%; }.connection-success h3 { font-size: 18px; }.connection-success p { color: var(--muted); font-size: 15px; margin-bottom: 24px; }
	@media(max-width:600px) { .connect-dialog { padding: 22px 18px; border-radius: 15px; }.dialog-heading h2 { font-size: 24px; }.provider-options { grid-template-columns: 1fr; gap: 9px; }.provider-option { min-height: 75px; padding: 13px 15px; gap: 15px; }.provider-option>span:not(.provider-logo) { font-size: 15px; }.provider-option small { font-size: 15px; }.dialog-import { align-items: flex-start; flex-direction: column; } }
</style>
