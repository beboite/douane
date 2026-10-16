import { en, type StringKey } from './strings/en';
import { fr } from './strings/fr';

export type Language = 'en' | 'fr';
export type LanguageChoice = 'auto' | Language;
export type { StringKey };

const storageKey = 'douane-language';
const tables: Record<Language, Record<StringKey, string>> = { en, fr };
const locales: Record<Language, string> = { en: 'en-US', fr: 'fr-FR' };

function stored(): LanguageChoice {
	try {
		const value = localStorage.getItem(storageKey);
		return value === 'en' || value === 'fr' ? value : 'auto';
	} catch { return 'auto'; }
}

/** French when the first preferred language that is French or English is French. */
function detect(): Language {
	if (typeof navigator === 'undefined') return 'en';
	const preferred = navigator.languages?.length ? navigator.languages : [navigator.language];
	for (const tag of preferred) {
		const lower = tag?.toLowerCase() ?? '';
		if (lower.startsWith('fr')) return 'fr';
		if (lower.startsWith('en')) return 'en';
	}
	return 'en';
}

const state = $state({ choice: typeof window === 'undefined' ? 'auto' as LanguageChoice : stored(), detected: detect() });

export function languageChoice(): LanguageChoice { return state.choice; }
export function language(): Language { return state.choice === 'auto' ? state.detected : state.choice; }
export function locale(): string { return locales[language()]; }

function applyDocumentLanguage() { if (typeof document !== 'undefined') document.documentElement.lang = language(); }

export function setLanguage(choice: LanguageChoice) {
	state.choice = choice;
	try { if (choice === 'auto') localStorage.removeItem(storageKey); else localStorage.setItem(storageKey, choice); }
	catch { /* A browser without storage keeps the choice for this page only. */ }
	applyDocumentLanguage();
}

if (typeof window !== 'undefined') {
	applyDocumentLanguage();
	window.addEventListener('languagechange', () => { state.detected = detect(); applyDocumentLanguage(); });
}

/** The string for `key` in the active language, with `{name}` placeholders replaced. */
export function t(key: StringKey, params?: Record<string, string | number>): string {
	const text = tables[language()][key] ?? en[key];
	return params ? text.replace(/\{(\w+)\}/g, (match, name: string) => name in params ? String(params[name]) : match) : text;
}
