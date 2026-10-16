<script lang="ts">
	import Download from '@lucide/svelte/icons/download';
	import Upload from '@lucide/svelte/icons/upload';
	import TriangleAlert from '@lucide/svelte/icons/triangle-alert';
	import { management } from '$lib/management';
	import { t } from '$lib/i18n.svelte';
	let { busy, restore }: { busy: string; restore: (bundle: unknown, settings: boolean) => Promise<boolean> } = $props();
	let exporting = $state(false);
	let failure = $state('');
	let pending = $state<{ name: string; bundle: unknown; count: number } | null>(null);
	let withSettings = $state(false);
	let picker = $state<HTMLInputElement>();

	async function download() {
		exporting = true; failure = '';
		try {
			const bundle = await management.exportConfig();
			const date = typeof bundle.exported_at === 'string' ? bundle.exported_at.slice(0, 10) : new Date().toISOString().slice(0, 10);
			const url = URL.createObjectURL(new Blob([JSON.stringify(bundle, null, 2)], { type: 'application/json' }));
			const link = Object.assign(document.createElement('a'), { href: url, download: `douane-export-${date}.json` });
			link.click();
			setTimeout(() => URL.revokeObjectURL(url), 1000);
		} catch (error) { failure = (error as Error).message; }
		finally { exporting = false; }
	}

	async function choose(files: FileList | null) {
		failure = ''; pending = null; withSettings = false;
		const file = files?.[0];
		if (picker) picker.value = '';
		if (!file) return;
		if (file.size > 32 * 1024 * 1024) { failure = t('backup.tooLarge', { name: file.name }); return; }
		let bundle: { format?: unknown; accounts?: unknown };
		try { bundle = JSON.parse(await file.text()); } catch { failure = t('notice.invalidJson', { name: file.name }); return; }
		if (bundle?.format !== 'douane-export' || !Array.isArray(bundle.accounts)) { failure = t('backup.notExport', { name: file.name }); return; }
		pending = { name: file.name, bundle, count: bundle.accounts.length };
	}

	async function confirm() {
		if (!pending) return;
		if (await restore(pending.bundle, withSettings)) pending = null;
	}
</script>

<section class="backup-setting">
	<div><h2>{t('backup.title')}</h2><p>{t('backup.description')}</p></div>
	<div class="backup-body">
		<p class="backup-warning"><TriangleAlert size={18}/><span>{t('backup.warning')}</span></p>
		<div class="backup-actions">
			<button class="button" type="button" disabled={exporting || !!busy} onclick={download}><Download size={16}/>{exporting ? t('backup.exporting') : t('backup.export')}</button>
			<button class="button" type="button" disabled={!!busy} onclick={() => picker?.click()}><Upload size={16}/>{t('backup.restore')}</button>
			<input bind:this={picker} class="file-input" type="file" accept="application/json,.json" aria-label={t('backup.restore')} onchange={(event) => choose(event.currentTarget.files)}/>
		</div>
		{#if failure}<p class="backup-error" role="alert">{failure}</p>{/if}
		{#if pending}
			<div class="restore-confirmation" role="group" aria-label={t('backup.restore')}>
				<p>{t(pending.count === 1 ? 'backup.confirm.one' : 'backup.confirm.other', { count: pending.count, name: pending.name })}</p>
				<label class="settings-choice"><input type="checkbox" bind:checked={withSettings}/><span>{t('backup.withSettings')}</span></label>
				<div class="confirmation-actions">
					<button class="button primary" type="button" disabled={!!busy} onclick={confirm}>{busy === 'restore' ? t('backup.restoring') : t('backup.confirmButton')}</button>
					<button class="button" type="button" disabled={busy === 'restore'} onclick={() => pending = null}>{t('common.cancel')}</button>
				</div>
			</div>
		{/if}
	</div>
</section>

<style>
	.backup-setting { display: grid; grid-template-columns: 245px 1fr; gap: 34px; max-width: 1060px; margin-top: 30px; padding: 30px 0 0; border-top: 1px solid var(--border); }
	h2 { font-size: 16px; margin: 0 0 9px; }
	p { color: var(--muted); font-size: 15px; line-height: 1.65; margin: 0; }
	.backup-body { display: grid; gap: 16px; align-content: start; }
	.backup-warning { display: flex; align-items: flex-start; gap: 11px; padding: 14px 16px; border: 1px solid color-mix(in srgb,var(--yellow) 28%,transparent); border-radius: 12px; background: color-mix(in srgb,var(--yellow) 6%,transparent); color: var(--text); }
	.backup-warning :global(svg) { flex-shrink: 0; color: var(--yellow); margin-top: 3px; }
	.backup-actions { display: flex; flex-wrap: wrap; justify-content: flex-end; gap: 10px; }
	.file-input { display: none; }
	.backup-error { color: var(--red); }
	.restore-confirmation { display: grid; gap: 14px; padding: 16px; border: 1px solid var(--border); border-radius: 12px; background: var(--surface); }
	.restore-confirmation p { color: var(--text); }
	.settings-choice { display: flex; align-items: center; gap: 10px; font-size: 15px; cursor: pointer; }
	.settings-choice input { width: auto; height: auto; margin: 0; accent-color: var(--primary); }
	.confirmation-actions { display: flex; flex-wrap: wrap; justify-content: flex-end; gap: 10px; }
	@media(max-width:1150px) { .backup-setting { grid-template-columns: 1fr; gap: 22px; } }
	@media(max-width:650px) { .backup-actions .button, .confirmation-actions .button { flex: 1; } }
</style>
