import { base } from '$app/paths';

/** douane prices nothing: a reseller's price list is fiction. The counters
 *  below are trust signals and attribution, never a bill. */
export type Bucket = {
	key: string;
	calls: number;
	errors: number;
	input_tokens: number;
	output_tokens: number;
	/** Read and written cache tokens stay two numbers. Their ratio says whether
	 *  the upstream held the prefix between turns or primed it again, and summed
	 *  into one "cached" figure the two cases look the same. */
	cache_read_tokens: number;
	cache_write_tokens: number;
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
};

/** One agent seen working inside the live window. There is no session table
 *  behind this: a row is a group of calls that stops being listed on its own
 *  once the calls stop, so nothing has to report that it left. */
export type LiveAgent = {
	/** The thread fingerprint, or `client:<name>` for a call that carried no
	 *  conversation. Stable for the life of the thread, so a list keyed on it
	 *  does not reshuffle between refreshes. */
	key: string;
	client: string;
	route: string;
	provider: string;
	model_requested: string;
	model_sent: string;
	trust: Trust;
	agent_fingerprint: string | null;
	thread_fingerprint: string | null;
	/** What the harness calls itself. A hint, never a credential. */
	user_agent: string | null;
	first_at: string;
	last_at: string;
	calls: number;
	/** Calls with no answer yet: the agent is generating while the screen is
	 *  being read. The one field here that says now rather than just before. */
	in_flight: number;
	errors: number;
	streamed: boolean;
	last_status: number | null;
	input_tokens: number;
	output_tokens: number;
	cache_read_tokens: number;
	cache_write_tokens: number;
};

export type LiveView = {
	/** The broker's own clock. Ages are measured against it, so a browser whose
	 *  clock drifts still reads the right delay. */
	now: string;
	window_seconds: number;
	agents: LiveAgent[];
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
	action: 'allow' | 'warn' | 'substitute' | 'block' | string;
};

/** One credential access request, filed by an agent over MCP. Names and
 *  timestamps only: there is no field a value could travel in. */
export type Grant = {
	id: number;
	task_id: string;
	at: string;
	client: string;
	reference: string;
	reason: string;
	status: 'pending' | 'approved' | 'denied' | 'cancelled' | string;
	note: string | null;
	decided_at: string | null;
	expires_at: string | null;
	used_at: string | null;
};

export type Trust = 'max' | 'mid' | 'low' | string;

export type ProviderView = {
	name: string;
	base_url: string;
	trust: Trust;
	keyed: boolean;
};

export type RouteView = {
	name: string;
	provider: string;
	model: string | null;
	fallback: string[];
	base_provider: string;
	base_model: string | null;
	overridden: boolean;
	/** True when this route exists because a model was switched on, rather than
	 *  because it was written in douane.toml. Those are edited on the models
	 *  screen, so this page shows them and does not offer to repoint them. */
	from_catalog: boolean;
};

/** One model an upstream sells. A row here is a fact about the provider;
 *  `enabled` is the owner's decision, and only that turns it into a route. */
export type ModelEntry = {
	provider: string;
	upstream_id: string;
	route: string;
	label: string | null;
	context_length: number | null;
	enabled: boolean;
	source: 'probed' | 'manual';
	/** False once a refresh no longer finds it upstream. It keeps serving if it
	 *  was on: an upstream that stops listing a model it still answers on is
	 *  common, and cutting the route on that basis would break a working setup. */
	present: boolean;
	first_seen: string;
	last_seen: string;
	note: string | null;
	/** The dialects the upstream says this id answers in. Empty means it never
	 *  said, and silence is never read as a refusal. */
	endpoints: string[];
	/** Why this row installs no route, or null when it does. Computed by the
	 *  broker so the switch can refuse before it is clicked. */
	blocked: Collision | null;
};

/** Why an enabled row serves nothing. `already_in_file` is the one that is not
 *  a problem: the file and the catalogue agree and the model answers. */
export type CollisionKind = 'already_in_file' | 'conflict' | 'no_provider' | 'wrong_dialect';

export type Collision = {
	kind: CollisionKind;
	/** The broker's own words, in English. The screen writes its own. */
	detail: string;
	/** Who the file points that name at, when the name is the problem. */
	other: string | null;
};

export type CatalogProvider = {
	name: string;
	trust: Trust;
	/** Whether this upstream publishes a catalogue at all. When it does not,
	 *  models are added by hand. */
	catalog: boolean;
	prefix: string;
	/** Which dialect this surface speaks. Two providers on one host are one
	 *  subscription sold twice, and each id answers on one of them. */
	wire: 'anthropic' | 'openai';
	host: string;
};

export type CatalogView = {
	models: ModelEntry[];
	/** Rows switched on whose route name is already served by douane.toml, which
	 *  the file always wins. Shown rather than silently ignored. */
	collisions: Record<string, Collision>;
	providers: CatalogProvider[];
};

/** What one provider answered when asked for its catalogue. */
export type RefreshResult = {
	provider: string;
	ok: boolean;
	found: number;
	/** How many of those this surface can actually reach. */
	speaks: number;
	added: number;
	/** Rows deleted because this surface cannot reach them. Never one that was
	 *  switched on: a decision is refused in the open, never erased. */
	dropped: number;
	missing: number;
	status: number | null;
	ms: number;
	detail: string | null;
};

export type ClientView = {
	name: string;
	routes: string[];
	max_trust: Trust | null;
	note: string | null;
};

/** One provider's answer to "does your key still open the door". A real
 *  one-token call with the real credential, so `ok` means the key works today,
 *  not that one is configured. */
export type Probe = {
	provider: string;
	route: string | null;
	model: string | null;
	ok: boolean;
	status: number | null;
	ms: number;
	detail: string | null;
};

export type ConfigView = {
	providers: ProviderView[];
	routes: RouteView[];
	clients: ClientView[];
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
		request<{ summary: Summary }>(token, `/summary?hours=${hours}`),

	calls: (token: string, limit: number, filter: { client?: string; thread?: string } = {}) => {
		const q = new URLSearchParams({ limit: String(limit) });
		if (filter.client) q.set('client', filter.client);
		if (filter.thread) q.set('thread', filter.thread);
		return request<Call[]>(token, `/calls?${q}`);
	},

	threads: (token: string, hours: number, limit = 50) =>
		request<Thread[]>(token, `/threads?hours=${hours}&limit=${limit}`),

	alerts: (token: string, limit = 200) => request<Alert[]>(token, `/alerts?limit=${limit}`),

	/** Who is working right now. `seconds` is how long an agent stays on screen
	 *  after its last call, not how far back the log is read. */
	live: (token: string, seconds: number) => request<LiveView>(token, `/live?seconds=${seconds}`),

	config: (token: string) => request<ConfigView>(token, '/config'),

	/** POST on purpose: each probe spends a (tiny) real call per provider. */
	probe: (token: string) => request<{ probes: Probe[] }>(token, '/providers/probe', { method: 'POST' }),

	grants: (token: string, status?: string, limit = 200) => {
		const q = new URLSearchParams({ limit: String(limit) });
		if (status) q.set('status', status);
		return request<Grant[]>(token, `/grants?${q}`);
	},

	/** The owner's word on a request. One decision each: a second call answers
	 *  409, which the page reads as "already handled elsewhere". */
	decideGrant: (
		token: string,
		id: number,
		body: { approve: boolean; note?: string; ttl_hours?: number }
	) =>
		request<Grant>(token, `/grants/${id}/decide`, {
			method: 'POST',
			body: JSON.stringify(body)
		}),

	vault: (token: string) => request<VaultView>(token, '/vault'),

	setRoute: (token: string, name: string, body: { provider?: string; model?: string | null }) =>
		request<{ route: string; provider: string; model: string | null }>(
			token,
			`/routes/${encodeURIComponent(name)}`,
			{ method: 'PUT', body: JSON.stringify(body) }
		),

	clearRoute: (token: string, name: string) =>
		request<unknown>(token, `/routes/${encodeURIComponent(name)}`, { method: 'DELETE' }),

	models: (token: string) => request<CatalogView>(token, '/models'),

	/** Ask the upstreams what they sell. POST because it spends one real request
	 *  per provider. Nothing it finds is served until it is switched on. */
	refreshModels: (token: string, provider?: string) =>
		request<{ refreshed: RefreshResult[] }>(
			token,
			`/models/refresh${provider ? `?provider=${encodeURIComponent(provider)}` : ''}`,
			{ method: 'POST' }
		),

	setModelEnabled: (token: string, route: string, enabled: boolean) =>
		request<{ route: string; enabled: boolean }>(
			token,
			`/models/${encodeURIComponent(route)}/enabled`,
			{ method: 'PUT', body: JSON.stringify({ enabled }) }
		),

	setProviderModelsEnabled: (token: string, provider: string, enabled: boolean) =>
		request<{ provider: string; changed: number; skipped: number }>(
			token,
			`/providers/${encodeURIComponent(provider)}/models/enabled`,
			{ method: 'PUT', body: JSON.stringify({ enabled }) }
		),

	/** Add a model by hand, for an upstream that publishes no catalogue. */
	putModel: (
		token: string,
		body: {
			provider: string;
			upstream_id: string;
			route?: string;
			label?: string | null;
			context_length?: number | null;
			enabled?: boolean;
			note?: string | null;
		}
	) => request<{ route: string }>(token, '/models', { method: 'PUT', body: JSON.stringify(body) }),

	/** Correct a discovered row: the name on screen, the window a harness sizes
	 *  itself from, the name agents type. */
	editModel: (
		token: string,
		route: string,
		body: { label?: string; context_length?: number; note?: string; route?: string }
	) =>
		request<{ route: string }>(token, `/models/${encodeURIComponent(route)}`, {
			method: 'PATCH',
			body: JSON.stringify(body)
		}),

	deleteModel: (token: string, route: string) =>
		request<{ deleted: string }>(token, `/models/${encodeURIComponent(route)}`, {
			method: 'DELETE'
		}),

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
