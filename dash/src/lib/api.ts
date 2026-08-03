import { base } from '$app/paths';

/** Every cost in this app is a map keyed by currency. Douane never converts, so
 *  two units stay two numbers and a total is never a scalar. */
export type Money = Record<string, number>;

export type Bucket = {
	key: string;
	calls: number;
	errors: number;
	input_tokens: number;
	output_tokens: number;
	cache_read_tokens: number;
	cost: Money;
};

/** One point on the activity curve. The broker fills the quiet slots itself, so
 *  a gap in the traffic is a gap in the chart and never a straight line drawn
 *  between two calls an hour apart. */
export type Slot = {
	at: string;
	/** Slot width in seconds. It travels with the point because it depends on the
	 *  window that was asked for, and a guess mislabels the axis. */
	seconds: number;
	calls: number;
	errors: number;
	input_tokens: number;
	output_tokens: number;
	cost: Money;
};

export type Summary = {
	totals: Bucket;
	by_provider: Bucket[];
	by_client: Bucket[];
	by_route: Bucket[];
	by_agent: Bucket[];
	series: Slot[];
};

export type Call = {
	id: number;
	at: string;
	client: string;
	route: string;
	provider: string;
	model_requested: string;
	model_sent: string;
	trust: Trust;
	agent_fingerprint: string | null;
	thread_fingerprint: string | null;
	streamed: boolean;
	status: number | null;
	duration_ms: number | null;
	bytes_up: number;
	bytes_down: number | null;
	input_tokens: number | null;
	output_tokens: number | null;
	cache_read_tokens: number | null;
	cache_write_tokens: number | null;
	cost: number | null;
	currency: string | null;
	error: string | null;
};

export type Thread = {
	fingerprint: string;
	first_at: string;
	last_at: string;
	calls: number;
	clients: string;
	providers: string;
	input_tokens: number;
	output_tokens: number;
	cost: Money;
};

export type Alert = {
	id: number;
	at: string;
	call_id: number | null;
	client: string;
	provider: string;
	trust: Trust;
	kind: string;
	label: string;
	action: 'allow' | 'warn' | 'block' | string;
};

export type Trust = 'max' | 'mid' | 'low' | string;

export type ProviderView = {
	name: string;
	base_url: string;
	trust: Trust;
	keyed: boolean;
	priced: boolean;
};

export type RouteView = {
	name: string;
	provider: string;
	model: string | null;
	fallback: string[];
	base_provider: string;
	base_model: string | null;
	overridden: boolean;
	price: string | null;
};

export type ClientView = {
	name: string;
	routes: string[];
	max_trust: Trust | null;
	note: string | null;
};

export type ConfigView = {
	providers: ProviderView[];
	routes: RouteView[];
	clients: ClientView[];
	currency: string;
	scrub: Record<string, unknown>;
	watching: number;
	egress: { listening: boolean; rules: number };
};

/** What a caller may know about a secret without holding it. There is
 *  deliberately no shape here with a value in it. */
export type EntryInfo = {
	namespace: string;
	key: string;
	placeholder: string;
	fingerprint: string;
	bytes: number;
	updated_at: string;
	note: string | null;
};

export type VaultView = {
	entries: EntryInfo[];
	/** References the running configuration authenticates with. Deleting one of
	 *  these is refused by the broker, so the page marks them instead of
	 *  offering a button that cannot work. */
	in_use: string[];
	watching: number;
};

/** The broker answers errors in Anthropic's shape, so one unwrapper covers both
 *  its own refusals and anything it relayed. */
export class ApiError extends Error {
	constructor(
		readonly status: number,
		message: string
	) {
		super(message);
	}
}

export const UNAUTHORIZED = 401;

async function request<T>(token: string, path: string, init?: RequestInit): Promise<T> {
	const res = await fetch(`${base}/api${path}`, {
		...init,
		headers: {
			// The token goes in a header and never in the query string: a URL is
			// written to every access log between the browser and the broker.
			'x-douane-admin': token,
			...(init?.body ? { 'content-type': 'application/json' } : {}),
			...init?.headers
		}
	});
	if (!res.ok) {
		let message = `${res.status}`;
		try {
			const body = await res.json();
			message = body?.error?.message ?? message;
		} catch {
			/* not JSON: the status is all there is to say */
		}
		throw new ApiError(res.status, message);
	}
	return res.status === 204 ? (undefined as T) : ((await res.json()) as T);
}

export const api = {
	summary: (token: string, hours: number) =>
		request<{ currency: string; summary: Summary }>(token, `/summary?hours=${hours}`),

	calls: (token: string, limit: number, filter: { client?: string; thread?: string } = {}) => {
		const q = new URLSearchParams({ limit: String(limit) });
		if (filter.client) q.set('client', filter.client);
		if (filter.thread) q.set('thread', filter.thread);
		return request<Call[]>(token, `/calls?${q}`);
	},

	threads: (token: string, hours: number, limit = 50) =>
		request<Thread[]>(token, `/threads?hours=${hours}&limit=${limit}`),

	alerts: (token: string, limit = 200) => request<Alert[]>(token, `/alerts?limit=${limit}`),

	config: (token: string) => request<ConfigView>(token, '/config'),

	vault: (token: string) => request<VaultView>(token, '/vault'),

	setRoute: (token: string, name: string, body: { provider?: string; model?: string | null }) =>
		request<{ route: string; provider: string; model: string | null }>(
			token,
			`/routes/${encodeURIComponent(name)}`,
			{ method: 'PUT', body: JSON.stringify(body) }
		),

	clearRoute: (token: string, name: string) =>
		request<unknown>(token, `/routes/${encodeURIComponent(name)}`, { method: 'DELETE' }),

	/** Deposit or replace a credential. Omit `value` to rewrite the note alone,
	 *  which is the only way to annotate an entry without the browser having to
	 *  hold a secret it has no use for. */
	putSecret: (
		token: string,
		namespace: string,
		key: string,
		body: { value?: string; note?: string | null }
	) =>
		request<{ entry: EntryInfo; watching: number }>(
			token,
			`/vault/${encodeURIComponent(namespace)}/${encodeURIComponent(key)}`,
			{ method: 'PUT', body: JSON.stringify(body) }
		),

	deleteSecret: (token: string, namespace: string, key: string) =>
		request<{ deleted: string; watching: number }>(
			token,
			`/vault/${encodeURIComponent(namespace)}/${encodeURIComponent(key)}`,
			{ method: 'DELETE' }
		)
};
