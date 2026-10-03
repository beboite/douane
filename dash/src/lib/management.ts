export type Provider = { id: string; name: string; oauth: boolean; quota: boolean; docs_url: string; credential?: boolean; base_url?: string };
export type Account = {
	id: string; name: string; provider: string; email?: string | null; plan?: string | null;
	subscription?: Subscription | null;
	enabled: boolean; status: string; last_error?: string | null; updated_at?: string | null;
};
export type QuotaWindow = { id: string; label: string; remaining_percent: number | null; reset_at?: string | null };
export type Subscription = { id: string; label: string; multiplier?: number | null; source: string };
export type CreditUsage = { id: string; label: string; used?: number | null; limit?: number | null; remaining?: number | null; unit: string; reset_at?: string | null; unlimited?: boolean | null; enabled?: boolean | null };
export type Quota = { account_id: string; windows: QuotaWindow[]; subscription?: Subscription | null; credits?: CreditUsage[]; updated_at?: string | null; error?: string | null };
export type ResetAvailability = { account_id: string; provider: string; status: 'ready' | 'unsupported' | 'unavailable' | 'ineligible'; available_count: number | null; next_expires_at: string | null; pending_request_id?: string | null; error: string | null };
export type ResetResult = { request_id: string; outcome: 'reset' | 'nothing_to_reset' | 'no_credit' | 'already_redeemed' };
export type Model = { id: string; provider: string; name?: string; context_length?: number | null; enabled?: boolean };
export type Call = { id: string | number; started_at: string; model: string; provider: string; account_id?: string | null; status: number; latency_ms?: number; input_tokens?: number; output_tokens?: number; cache_read_tokens?: number; cache_write_tokens?: number; error?: string | null };
export type Status = { version: string; uptime_seconds: number; in_flight: number; requests: number; errors: number; accounts: number; enabled_accounts: number; providers: number; input_tokens: number; output_tokens: number; cache_read_tokens?: number; cache_write_tokens?: number };
export type Strategy = 'round-robin' | 'fill-first' | 'reset-first' | 'smart';
export type Settings = { strategy: Strategy; quota_refresh_seconds: number; request_timeout_seconds: number; default_provider: string | null; provider_strategies?: Record<string, Strategy> };
export type OAuthSession = { id: string; provider: string; url: string; state: string; redirect_uri: string; expires_at: string; status: string; account_id?: string | null; error?: string | null; user_code?: string | null; interval?: number | null };
export type OAuthStatus = Partial<OAuthSession> & { status: string; account?: Account };
export type ImportError = string | { name?: string | null; error: string };
export type ImportResult = { accounts?: Account[]; imported: number; errors: ImportError[] };

export class ApiError extends Error {
	constructor(readonly status: number, message: string) { super(message); }
}

async function request<T>(path: string, init?: RequestInit): Promise<T> {
	const response = await fetch(`/admin/api${path}`, {
		...init,
		headers: { ...(init?.body ? { 'content-type': 'application/json' } : {}), ...init?.headers }
	});
	if (!response.ok) {
		let message = `Le serveur a répondu ${response.status}.`;
		try {
			const body = await response.json();
			message = body.errors?.length ? importErrors(body.errors) : typeof body.error === 'string' ? body.error : body.error?.message ?? body.message ?? message;
		} catch { /* Preserve a meaningful HTTP error when the server does not return JSON. */ }
		throw new ApiError(response.status, message);
	}
	return response.status === 204 ? undefined as T : response.json();
}

export const management = {
	status: () => request<Status>('/summary'),
	providers: () => request<{ providers: Provider[] }>('/providers'),
	accounts: () => request<{ accounts: Account[] }>('/accounts'),
	quotas: () => request<{ quotas: Quota[] }>('/quotas'),
	models: () => request<{ models: Model[] }>('/models'),
	calls: () => request<{ calls: Call[] }>('/calls?limit=100'),
	settings: () => request<Settings>('/settings'),
	refreshQuotas: (account_id?: string) => request('/quotas/refresh', { method: 'POST', body: JSON.stringify({ account_id }) }),
	importAccounts: (files: { name: string; content: unknown }[]) => request<ImportResult>('/accounts/import', { method: 'POST', body: JSON.stringify({ files }) }),
	importCredential: (credential: { type: string; name: string; access_token: string; base_url: string }) => request<ImportResult>('/accounts/import', { method: 'POST', body: JSON.stringify(credential) }),
	setAccountEnabled: (id: string, enabled: boolean) => request(`/accounts/${encodeURIComponent(id)}`, { method: 'PATCH', body: JSON.stringify({ enabled }) }),
	renameAccount: (id: string, name: string) => request<{ account: Account }>(`/accounts/${encodeURIComponent(id)}`, { method: 'PATCH', body: JSON.stringify({ name }) }),
	accountResets: (id: string) => request<ResetAvailability>(`/accounts/${encodeURIComponent(id)}/resets`),
	redeemReset: (id: string, request_id: string) => request<ResetResult>(`/accounts/${encodeURIComponent(id)}/resets/redeem`, { method: 'POST', body: JSON.stringify({ confirmation: true, request_id }) }),
	deleteAccount: (id: string) => request(`/accounts/${encodeURIComponent(id)}`, { method: 'DELETE' }),
	startOAuth: (provider: string) => request<OAuthSession>('/oauth/start', { method: 'POST', body: JSON.stringify({ provider }) }),
	oauth: (id: string) => request<OAuthStatus>(`/oauth/${encodeURIComponent(id)}`),
	oauthCallback: (id: string, code: string, state: string) => request<{ account: Account }>('/oauth/callback', { method: 'POST', body: JSON.stringify({ id, code, state }) }),
	setModelEnabled: (id: string, enabled: boolean) => request(`/models/${encodeURIComponent(id)}`, { method: 'PATCH', body: JSON.stringify({ enabled }) }),
	settingsSave: (settings: Settings) => request<Settings>('/settings', { method: 'PATCH', body: JSON.stringify(settings) })
};

export const providerNames: Record<string, string> = {
	codex: 'Codex', claude: 'Claude', gemini: 'Gemini', antigravity: 'Antigravity', qwen: 'Qwen', kimi: 'Kimi', copilot: 'GitHub Copilot', iflow: 'iFlow', opencode_go: 'OpenCode Go', xai: 'Grok', grok: 'Grok', openai_compatible: 'Compatible OpenAI'
};
export function privateLabel(value: string, showEmails = false) { return showEmails ? value : value.replace(/[A-Z0-9._%+-]+@[A-Z0-9.-]+\.[A-Z]{2,}/gi, 'compte'); }
export function importErrors(errors: ImportError[]) { return errors.map(error => typeof error === 'string' ? error : `${error.name ? `${privateLabel(error.name)} : ` : ''}${error.error}`).join(' '); }
export function accountStatus(account: Account) { return !account.enabled ? 'Désactivé' : ({ ready:'Actif',active:'Actif',disabled:'Désactivé',exhausted:'Quota épuisé',cooldown:'En pause',expired:'Connexion expirée',error:'À vérifier' }[account.status] ?? 'À vérifier'); }
export function accountWarning(account: Account) { return account.enabled && (!!account.last_error || !['ready','active'].includes(account.status)); }
export function providerName(id: string) { return providerNames[id] ?? id; }
export function formatCount(value: number | undefined) { return value == null ? '-' : new Intl.NumberFormat('fr-FR', { notation: value >= 10000 ? 'compact' : 'standard', maximumFractionDigits: 1 }).format(value); }
export function formatDate(value?: string | null) { return value ? new Intl.DateTimeFormat('fr-FR', { day: '2-digit', month: 'short', hour: '2-digit', minute: '2-digit' }).format(new Date(value)) : '-'; }
export function resetLabel(value?: string | null) {
	if (!value) return 'Date non communiquée';
	const date = new Date(value);
	if (Number.isNaN(date.getTime())) return 'Date non communiquée';
	const distant = Math.abs(date.getTime() - Date.now()) > 7 * 24 * 60 * 60 * 1000;
	return new Intl.DateTimeFormat('fr-FR', { weekday: 'long', ...(distant ? { day: 'numeric', month: 'short' } as const : {}), hour: '2-digit', minute: '2-digit' }).format(date);
}
export function creditValue(value: number | null | undefined, unit: string) {
	if (value == null) return 'Non communiqué';
	if (unit === 'USD') return new Intl.NumberFormat('fr-FR', { style: 'currency', currency: 'USD', maximumFractionDigits: 2 }).format(value);
	if (/^[A-Z]{3}_minor$/.test(unit)) return new Intl.NumberFormat('fr-FR', { style: 'currency', currency: unit.slice(0, 3), maximumFractionDigits: 2 }).format(value / 100);
	return `${new Intl.NumberFormat('fr-FR', { maximumFractionDigits: 2 }).format(value)} ${unit === 'credits' ? 'crédits' : unit}`;
}
export function usageLabel(label: string) {
	return ({ weekly: 'Semaine', 'codex weekly': 'Semaine', 'grok weekly': 'Semaine', monthly: 'Mois', '5 hours': '5 heures', 'five hour': '5 heures', 'extra usage': 'Usage supplémentaire', 'additional usage': 'Usage supplémentaire', 'codex credits': 'Crédits Codex', 'credit balance': 'Solde des crédits' } as Record<string, string>)[label.toLowerCase()] ?? label;
}
export function newResetRequestId() {
	if (typeof crypto.randomUUID === 'function') return crypto.randomUUID();
	const bytes = crypto.getRandomValues(new Uint8Array(16));
	bytes[6] = (bytes[6] & 0x0f) | 0x40; bytes[8] = (bytes[8] & 0x3f) | 0x80;
	const hex = Array.from(bytes, byte => byte.toString(16).padStart(2, '0')).join('');
	return `${hex.slice(0, 8)}-${hex.slice(8, 12)}-${hex.slice(12, 16)}-${hex.slice(16, 20)}-${hex.slice(20)}`;
}
