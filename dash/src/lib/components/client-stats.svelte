<script lang="ts">
	import Info from '@lucide/svelte/icons/info';
	import ChevronDown from '@lucide/svelte/icons/chevron-down';
	import ProviderMark from './provider-mark.svelte';
	import { management, formatCount, formatDecimal, formatDate, type Client, type ClientStats, type ClientTotals } from '$lib/management';
	import { t, locale, language } from '$lib/i18n.svelte';
	let { clients, colors, noKeyColor, formerColor, version }: { clients: Client[]; colors: Map<string, string>; noKeyColor: string; formerColor: string; version: unknown } = $props();
	const periods = [{ id: 'day', hours: 24 }, { id: 'week', hours: 24 * 7 }, { id: 'month', hours: 24 * 30 }] as const;
	type Period = (typeof periods)[number];
	let period = $state<Period>(periods[0]);
	let stats = $state.raw<ClientStats | null>(null);
	let failure = $state('');
	let open = $state<string | null>(null);
	let hovered = $state<number | null>(null);
	let names = $derived(new Map(clients.map(client => [client.id, client.name])));

	$effect(() => {
		const hours = period.hours;
		void version;
		let current = true;
		management.clientStats(hours).then(value => { if (current) { stats = value; failure = ''; } }, error => { if (current) failure = (error as Error).message; });
		return () => { current = false; };
	});

	/** Calls without a key predate the keys; ids no client holds belong to deleted clients. */
	function who(id: string) {
		if (!id) return { id: 'no-key', label: t('clients.noKey'), color: noKeyColor };
		if (names.has(id)) return { id, label: names.get(id)!, color: colors.get(id) ?? noKeyColor };
		return { id: 'former', label: t('clients.former'), color: formerColor };
	}
	const tokens = (row: { input_tokens: number; output_tokens: number }) => row.input_tokens + row.output_tokens;
	type Model = { key: string; provider: string; model: string; calls: number; errors: number; input: number; output: number; cache: number; latency: number };
	type Row = { id: string; label: string; color: string; calls: number; errors: number; input: number; output: number; cache: number; latency: number; last: string; models: Model[] };
	let rows = $derived.by(() => {
		const byId = new Map<string, Row>();
		for (const client of clients) byId.set(client.id, { ...who(client.id), calls: 0, errors: 0, input: 0, output: 0, cache: 0, latency: 0, last: '', models: [] });
		for (const total of stats?.totals ?? []) {
			const owner = who(total.client_id);
			let row = byId.get(owner.id);
			if (!row) { row = { ...owner, calls: 0, errors: 0, input: 0, output: 0, cache: 0, latency: 0, last: '', models: [] }; byId.set(owner.id, row); }
			add(row, total);
			if (total.last_at > row.last) row.last = total.last_at;
			const key = `${total.provider}/${total.model}`;
			let model = row.models.find(item => item.key === key);
			if (!model) { model = { key, provider: total.provider, model: total.model, calls: 0, errors: 0, input: 0, output: 0, cache: 0, latency: 0 }; row.models.push(model); }
			add(model, total);
		}
		for (const row of byId.values()) row.models.sort((a, b) => b.input + b.output - (a.input + a.output));
		return [...byId.values()].sort((a, b) => b.input + b.output - (a.input + a.output) || b.calls - a.calls || a.label.localeCompare(b.label));
	});
	function add(target: { calls: number; errors: number; input: number; output: number; cache: number; latency: number }, total: ClientTotals) {
		target.calls += total.calls; target.errors += total.errors; target.input += total.input_tokens; target.output += total.output_tokens; target.cache += total.cache_read_tokens; target.latency += total.latency_ms;
	}
	let sum = $derived(rows.reduce((all, row) => ({ calls: all.calls + row.calls, errors: all.errors + row.errors, tokens: all.tokens + row.input + row.output }), { calls: 0, errors: 0, tokens: 0 }));
	let active = $derived(rows.filter(row => row.calls > 0).length);

	// Hours for a day, local days beyond: a bucket per column, oldest first.
	let buckets = $derived.by(() => {
		const hourly = period.hours <= 24;
		const now = new Date();
		const slots: { start: Date; label: string; values: Map<string, number>; total: number }[] = [];
		if (hourly) {
			const top = new Date(now); top.setMinutes(0, 0, 0);
			for (let i = period.hours - 1; i >= 0; i--) slots.push({ start: new Date(top.getTime() - i * 3_600_000), label: '', values: new Map(), total: 0 });
		} else {
			const days = period.hours / 24;
			for (let i = days - 1; i >= 0; i--) { const day = new Date(now.getFullYear(), now.getMonth(), now.getDate() - i); slots.push({ start: day, label: '', values: new Map(), total: 0 }); }
		}
		const time = new Intl.DateTimeFormat(locale(), hourly ? { hour: language() === 'en' ? 'numeric' : '2-digit', minute: '2-digit' } : { weekday: period.hours <= 168 ? 'short' : undefined, day: 'numeric', month: 'short' });
		for (const slot of slots) slot.label = time.format(slot.start);
		for (const hour of stats?.hours ?? []) {
			const start = new Date(`${hour.hour}:00:00Z`);
			const index = hourly
				? slots.findIndex(slot => slot.start.getTime() === start.getTime())
				: slots.findIndex(slot => slot.start.getFullYear() === start.getFullYear() && slot.start.getMonth() === start.getMonth() && slot.start.getDate() === start.getDate());
			if (index < 0) continue;
			const id = who(hour.client_id).id;
			slots[index].values.set(id, (slots[index].values.get(id) ?? 0) + hour.tokens);
			slots[index].total += hour.tokens;
		}
		return slots;
	});
	let peak = $derived(Math.max(1, ...buckets.map(bucket => bucket.total)));
	let seen = $derived(rows.filter(row => row.input + row.output > 0));
	function percent(part: number, whole: number) { return whole ? (part / whole) * 100 : 0; }
	function share(value: number) { return t('common.percent', { value: formatDecimal(value) }); }
	function describe(index: number) {
		const bucket = buckets[index];
		return `${bucket.label}: ${seen.filter(row => bucket.values.get(row.id)).map(row => `${row.label} ${formatCount(bucket.values.get(row.id))}`).join(', ') || t('stats.none')}`;
	}
</script>

<section class="stats">
	<div class="section-toolbar"><h2 class="section-title">{t('stats.title')}<span class="info" title={t('stats.info')} aria-label={t('stats.info')}><Info size={16}/></span></h2>
		<div class="periods" role="group" aria-label={t('stats.period')}>
			{#each periods as item (item.id)}<button class:active={period.id === item.id} aria-pressed={period.id === item.id} onclick={() => { period = item; hovered = null; }}>{t(`stats.${item.id}`)}</button>{/each}
		</div>
	</div>
	{#if failure}<p class="dialog-error" role="alert">{failure}</p>{/if}
	<div class="ledger stats-card">
		<div class="tiles">
			<div><span>{t('stats.tokens')}</span><strong class="tabular">{formatCount(sum.tokens)}</strong></div>
			<div><span>{t('stats.calls')}</span><strong class="tabular">{formatCount(sum.calls)}</strong></div>
			<div><span>{t('stats.activeClients')}</span><strong class="tabular">{active}</strong></div>
			<div><span>{t('stats.errors')}</span><strong class="tabular" class:bad={sum.errors > 0}>{sum.calls ? share(percent(sum.errors, sum.calls)) : '-'}</strong></div>
		</div>
		<div class="chart" role="img" aria-label={t('stats.chart')} onmouseleave={() => hovered = null}>
			<span class="peak tabular">{formatCount(peak === 1 ? 0 : peak)}</span>
			<div class="columns" style:--count={buckets.length}>
				{#each buckets as bucket, index (bucket.start.getTime())}
					<button class="column" class:dim={hovered != null && hovered !== index} aria-label={describe(index)} onmouseenter={() => hovered = index} onfocus={() => hovered = index} onblur={() => hovered = null}>
						<span class="bar" style:height="{percent(bucket.total, peak)}%">
							{#each seen as row (row.id)}{@const value = bucket.values.get(row.id) ?? 0}{#if value}<span class="piece" style:flex-grow={value} style:background={row.color}></span>{/if}{/each}
						</span>
					</button>
				{/each}
			</div>
			<div class="axis"><span>{buckets[0]?.label}</span><span>{buckets[Math.floor(buckets.length / 2)]?.label}</span><span>{buckets.at(-1)?.label}</span></div>
			{#if hovered != null}
				{@const bucket = buckets[hovered]}
				<div class="tip" style:left="{((hovered + 0.5) / buckets.length) * 100}%" class:right={hovered > buckets.length / 2}>
					<p>{bucket.label}</p>
					{#each seen.filter(row => bucket.values.get(row.id)) as row (row.id)}<p class="tip-row"><i style:background={row.color}></i>{row.label}<strong class="tabular">{formatCount(bucket.values.get(row.id))}</strong></p>
					{:else}<p class="muted">{t('stats.none')}</p>{/each}
				</div>
			{/if}
		</div>

		<div class="rows">
			{#each rows as row (row.id)}
				{@const total = row.input + row.output}
				<div class="client-row" class:idle={!row.calls}>
					<button class="row-head" aria-expanded={open === row.id} disabled={!row.calls} onclick={() => open = open === row.id ? null : row.id}>
						<span class="who"><i style:background={row.color}></i><strong>{row.label}</strong></span>
						<span class="share"><span class="share-track"><span style:width="{percent(total, sum.tokens)}%" style:background={row.color}></span></span><span class="tabular">{row.calls ? share(percent(total, sum.tokens)) : ''}</span></span>
						{#if row.calls}
							<span class="figure tabular"><b>{formatCount(total)}</b> {t('stats.tokensUnit')}</span>
							<span class="figure tabular"><b>{formatCount(row.calls)}</b> {t('stats.callsUnit')}</span>
							<span class="figure tabular muted" title={t('stats.cacheInfo')}>{t('stats.cache', { value: formatDecimal(percent(row.cache, row.input)) })}</span>
							<span class="figure tabular errors" class:bad={row.errors > 0}>{row.errors ? t('stats.errorCount', { count: formatCount(row.errors) }) : t('stats.noError')}</span>
							<span class="figure muted last">{formatDate(row.last)}</span>
							<ChevronDown size={18} class="chevron"/>
						{:else}<span class="idle-label">{t('stats.idle')}</span>{/if}
					</button>
					{#if open === row.id}
						<div class="models">
							<div class="model head"><span>{t('stats.model')}</span><span>{t('stats.calls')}</span><span>{t('stats.input')}</span><span>{t('stats.output')}</span><span>{t('stats.cacheRead')}</span><span>{t('stats.latency')}</span><span>{t('stats.errors')}</span></div>
							{#each row.models as model (model.key)}
								<div class="model"><span class="model-name"><ProviderMark provider={model.provider} size={16}/>{model.model}</span><span class="tabular">{formatCount(model.calls)}</span><span class="tabular">{formatCount(model.input)}</span><span class="tabular">{formatCount(model.output)}</span><span class="tabular">{formatCount(model.cache)}</span><span class="tabular">{model.calls ? `${formatDecimal(model.latency / model.calls / 1000)} s` : '-'}</span><span class="tabular" class:bad={model.errors > 0}>{formatCount(model.errors)}</span></div>
							{/each}
						</div>
					{/if}
				</div>
			{:else}<p class="empty">{t('stats.empty')}</p>{/each}
		</div>
	</div>
</section>

<style>
	.stats { margin-bottom: 30px; }
	.section-toolbar { flex-wrap: wrap; }
	.section-title, .info { display: inline-flex; align-items: center; gap: 8px; }
	.section-title { font-family: var(--font-display); font-size: 23px; font-weight: 600; }
	.info { color: var(--faint); cursor: help; }
	.info:hover { color: var(--blue); }
	.periods { display: inline-flex; padding: 3px; border: 1px solid var(--border); border-radius: 10px; background: var(--background); }
	.periods button { min-height: 32px; padding: 4px 13px; border: 0; border-radius: 7px; background: transparent; color: var(--muted); font-size: 14px; font-weight: 550; cursor: pointer; }
	.periods button.active { background: var(--surface); color: var(--text); box-shadow: 0 0 0 1px var(--border); }
	.stats-card { display: grid; gap: 20px; padding: 20px; }
	.tiles { display: grid; grid-template-columns: repeat(4, 1fr); gap: 12px; }
	.tiles div { display: grid; gap: 4px; }
	.tiles span { color: var(--muted); font-size: 14px; }
	.tiles strong { font-family: var(--font-display); font-size: 26px; font-weight: 650; letter-spacing: -.02em; }
	.bad { color: var(--red); }
	.chart { position: relative; display: grid; gap: 6px; padding-top: 18px; }
	.peak { position: absolute; top: 0; left: 0; color: var(--faint); font-size: 12px; }
	.columns { display: grid; grid-template-columns: repeat(var(--count), 1fr); align-items: end; gap: 2px; height: 120px; border-bottom: 1px solid var(--border); background: repeating-linear-gradient(to top, transparent 0 39px, #25334c66 39px 40px); }
	.column { display: flex; align-items: flex-end; height: 100%; padding: 0; border: 0; background: transparent; cursor: default; }
	.column.dim .bar { opacity: .45; }
	.bar { display: flex; flex-direction: column-reverse; width: 100%; min-height: 0; overflow: hidden; border-radius: 4px 4px 0 0; transition: opacity .12s; }
	.piece { flex-basis: 0; min-height: 1px; }
	.piece + .piece { box-shadow: inset 0 -2px 0 var(--surface); }
	.axis { display: flex; justify-content: space-between; color: var(--faint); font-size: 12px; }
	.tip { position: absolute; top: 4px; z-index: 2; min-width: 150px; padding: 9px 11px; border: 1px solid var(--border); border-radius: 9px; background: #0b1325f2; box-shadow: 0 8px 24px #02061099; font-size: 13px; transform: translateX(8px); pointer-events: none; }
	.tip.right { transform: translateX(calc(-100% - 8px)); }
	.tip p { margin: 0; }
	.tip p:first-child { margin-bottom: 5px; color: var(--muted); }
	.tip-row { display: flex; align-items: center; gap: 7px; }
	.tip-row strong { margin-left: auto; padding-left: 12px; font-weight: 600; }
	.tip i, .who i { flex-shrink: 0; width: 10px; height: 10px; border-radius: 3px; }
	.rows { display: grid; border-top: 1px solid var(--border); }
	.client-row + .client-row { border-top: 1px solid var(--border); }
	.row-head { display: grid; grid-template-columns: minmax(120px, 1.2fr) minmax(120px, 1.4fr) repeat(4, minmax(70px, auto)) minmax(110px, auto) 18px; align-items: center; gap: 14px; width: 100%; min-height: 52px; padding: 8px 4px; border: 0; background: transparent; color: var(--text); font: inherit; font-size: 14px; text-align: left; cursor: pointer; }
	.row-head:disabled { cursor: default; }
	.row-head:not(:disabled):hover { background: #17243b80; }
	.row-head :global(.chevron) { color: var(--faint); transition: transform .15s; }
	.row-head[aria-expanded='true'] :global(.chevron) { transform: rotate(180deg); }
	.who { display: flex; align-items: center; gap: 9px; min-width: 0; }
	.who strong { overflow: hidden; font-size: 15px; font-weight: 600; text-overflow: ellipsis; white-space: nowrap; }
	.idle .who strong { color: var(--muted); font-weight: 500; }
	.share { display: grid; grid-template-columns: 1fr 52px; align-items: center; gap: 10px; }
	.share > .tabular { text-align: right; }
	.share-track { height: 8px; overflow: hidden; border-radius: 4px; background: #2a3a55; }
	.share-track span { display: block; height: 100%; border-radius: 4px; }
	.figure { white-space: nowrap; }
	.figure b { font-weight: 600; }
	.idle-label { grid-column: 3 / -1; color: var(--muted); }
	.muted { color: var(--muted); }
	.models { display: grid; margin: 0 4px 12px; border: 1px solid var(--border); border-radius: 10px; overflow: hidden; }
	.model { display: grid; grid-template-columns: minmax(170px, 2fr) repeat(6, minmax(60px, 1fr)); gap: 10px; padding: 8px 12px; font-size: 14px; }
	.model > span:not(.model-name) { text-align: right; }
	.model + .model { border-top: 1px solid var(--border); }
	.model.head { background: #17243b80; color: var(--muted); font-size: 13px; }
	.model.head > span:first-child { text-align: left; }
	.model-name { display: flex; align-items: center; gap: 8px; min-width: 0; overflow-wrap: anywhere; }
	.empty { margin: 0; padding: 16px 4px 0; color: var(--muted); }
	@media(max-width:900px) {
		.tiles { grid-template-columns: repeat(2, 1fr); }
		.row-head { grid-template-columns: 1fr auto auto 18px; row-gap: 6px; }
		.share { grid-column: 1 / -1; grid-row: 2; }
		.row-head .figure.muted, .errors { display: none; }
		.idle-label { grid-column: 2 / -1; }
		.models { overflow-x: auto; }
		.model { min-width: 620px; }
	}
</style>
