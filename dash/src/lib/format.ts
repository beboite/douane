const compact = new Intl.NumberFormat('fr-FR', { notation: 'compact', maximumFractionDigits: 1 });
const plain = new Intl.NumberFormat('fr-FR');

export function tokens(n: number | null | undefined): string {
	if (n === null || n === undefined) return '';
	return n >= 10_000 ? compact.format(n) : plain.format(n);
}

export function count(n: number | null | undefined): string {
	return n === null || n === undefined ? '' : plain.format(n);
}

export function bytes(n: number | null | undefined): string {
	if (n === null || n === undefined) return '';
	if (n < 1024) return `${n} o`;
	if (n < 1024 * 1024) return `${(n / 1024).toFixed(1)} Ko`;
	return `${(n / 1024 / 1024).toFixed(1)} Mo`;
}

export function ms(n: number | null | undefined): string {
	if (n === null || n === undefined) return '';
	return n < 1000 ? `${n} ms` : `${(n / 1000).toFixed(1)} s`;
}

const time = new Intl.DateTimeFormat('fr-FR', {
	hour: '2-digit',
	minute: '2-digit',
	second: '2-digit'
});
const dateAndTime = new Intl.DateTimeFormat('fr-FR', {
	day: '2-digit',
	month: '2-digit',
	hour: '2-digit',
	minute: '2-digit'
});

/** Today shows a clock, anything older shows a date: a log where every row says
 *  the same date wastes the width on repeating itself. */
export function when(iso: string): string {
	const d = new Date(iso);
	if (Number.isNaN(d.getTime())) return iso;
	const sameDay = d.toDateString() === new Date().toDateString();
	return sameDay ? time.format(d) : dateAndTime.format(d);
}

export function ago(iso: string): string {
	const d = new Date(iso).getTime();
	if (Number.isNaN(d)) return '';
	const s = Math.max(0, (Date.now() - d) / 1000);
	if (s < 60) return "à l'instant";
	if (s < 3600) return `il y a ${Math.floor(s / 60)} min`;
	if (s < 86400) return `il y a ${Math.floor(s / 3600)} h`;
	return `il y a ${Math.floor(s / 86400)} j`;
}

/** How long ago, counted in seconds and measured against the broker's clock
 *  rather than the browser's. On a live screen `ago` is too coarse: everything
 *  under a minute reads "à l'instant", which is most of what is on it. */
export function delay(nowIso: string, iso: string): string {
	const now = new Date(nowIso).getTime();
	const then = new Date(iso).getTime();
	if (Number.isNaN(now) || Number.isNaN(then)) return '';
	const s = Math.max(0, Math.round((now - then) / 1000));
	if (s < 5) return "à l'instant";
	if (s < 60) return `il y a ${s} s`;
	if (s < 3600) return `il y a ${Math.floor(s / 60)} min`;
	return `il y a ${Math.floor(s / 3600)} h`;
}

/** How long a stretch of work has been going, from its first call to its last. */
export function lasted(fromIso: string, toIso: string): string {
	const from = new Date(fromIso).getTime();
	const to = new Date(toIso).getTime();
	if (Number.isNaN(from) || Number.isNaN(to)) return '';
	const s = Math.max(0, Math.round((to - from) / 1000));
	if (s < 60) return `${s} s`;
	if (s < 3600) return `${Math.floor(s / 60)} min`;
	return `${Math.floor(s / 3600)} h ${Math.floor((s % 3600) / 60)} min`;
}

/* What each harness calls itself in its `user-agent`. The left side is matched
   lowercased, and the first hit wins, so the more specific names come first. */
const HARNESSES: [string, string][] = [
	['claude-cli', 'Claude Code'],
	['claude-code', 'Claude Code'],
	['opencode', 'OpenCode'],
	['codex', 'Codex'],
	['cline', 'Cline'],
	['roo', 'Roo'],
	['aider', 'Aider'],
	['cursor', 'Cursor'],
	['zed', 'Zed'],
	['anthropic-sdk', 'SDK Anthropic'],
	['anthropic', 'SDK Anthropic'],
	['openai', 'SDK OpenAI'],
	['langchain', 'LangChain'],
	['litellm', 'LiteLLM'],
	['python-httpx', 'script Python'],
	['python-requests', 'script Python'],
	['httpx', 'script Python'],
	['curl', 'curl'],
	['node', 'Node']
];

/** The harness behind a call, named from what it says about itself. A hint and
 *  nothing more: the header is caller-controlled, so this labels a row and is
 *  never allowed to decide anything. */
export function harness(userAgent: string | null | undefined): string {
	if (!userAgent) return '';
	const ua = userAgent.toLowerCase();
	for (const [needle, name] of HARNESSES) if (ua.includes(needle)) return name;
	// Something unknown: its own first word is a better label than nothing.
	return userAgent.split(/[\s/]/)[0].slice(0, 24);
}

/** A fingerprint is 64 hex characters and nobody reads more than the first few.
 *  Shortening it in the UI keeps the full value in the DOM for copying. */
export function short(fingerprint: string | null | undefined, n = 8): string {
	return fingerprint ? fingerprint.slice(0, n) : '';
}

const dayAndTime = new Intl.DateTimeFormat('fr-FR', {
	weekday: 'short',
	hour: '2-digit',
	minute: '2-digit'
});
const dayOnly = new Intl.DateTimeFormat('fr-FR', { day: '2-digit', month: 'short' });
const hourOnly = new Intl.DateTimeFormat('fr-FR', { hour: '2-digit', minute: '2-digit' });

/** How a point on the curve names itself, decided by how wide the slot is
 *  rather than by which window was picked. A day-wide slot labelled with a
 *  clock reads as a precision the number does not have. */
export function slotLabel(iso: string, seconds: number): string {
	const d = new Date(iso);
	if (Number.isNaN(d.getTime())) return iso;
	if (seconds >= 86_400) return dayOnly.format(d);
	if (seconds >= 3_600) return dayAndTime.format(d);
	return hourOnly.format(d);
}

/** The axis, thinned. Sixty labels do not fit and a chart that draws them all
 *  is unreadable at every width. */
export function axisLabel(iso: string, seconds: number): string {
	const d = new Date(iso);
	if (Number.isNaN(d.getTime())) return '';
	return seconds >= 86_400 ? dayOnly.format(d) : hourOnly.format(d);
}
