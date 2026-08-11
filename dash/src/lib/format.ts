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
