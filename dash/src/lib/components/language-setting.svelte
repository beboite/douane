<script lang="ts">
	import Languages from '@lucide/svelte/icons/languages';
	import { languageChoice, setLanguage, t, type LanguageChoice } from '$lib/i18n.svelte';
</script>

<!-- The dashboard language is a browser choice: it never reaches the server settings. -->
<section class="language-setting">
	<div><h2>{t('settings.language')}</h2><p>{t('settings.languageDescription')}</p></div>
	<div class="language-field"><Languages size={18}/><select aria-label={t('settings.language')} value={languageChoice()} onchange={(event) => setLanguage(event.currentTarget.value as LanguageChoice)}><option value="auto">{t('settings.languageAuto')}</option><option value="en" lang="en">{t('settings.languageEnglish')}</option><option value="fr" lang="fr">{t('settings.languageFrench')}</option></select></div>
</section>

<style>
	.language-setting { display: grid; grid-template-columns: 245px 1fr; gap: 34px; max-width: 1060px; margin-top: 30px; padding: 30px 0; border-top: 1px solid var(--border); }
	h2 { font-size: 16px; margin: 0 0 9px; }
	p { color: var(--muted); font-size: 15px; line-height: 1.65; margin: 0; }
	.language-field { display: flex; align-items: center; justify-content: flex-end; gap: 11px; }
	.language-field :global(svg) { flex-shrink: 0; color: var(--muted); }
	select { width: 240px; font-size: 15px; }
	@media(max-width:1150px) { .language-setting { grid-template-columns: 1fr; gap: 22px; } }
	@media(max-width:650px) { select { flex: 1; width: auto; min-width: 0; } }
</style>
