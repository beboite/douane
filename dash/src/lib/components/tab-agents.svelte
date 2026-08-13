<script lang="ts">
	import { Button } from '$lib/components/ui/button';
	import * as Card from '$lib/components/ui/card';
	import * as ToggleGroup from '$lib/components/ui/toggle-group';
	import ListTree from '@lucide/svelte/icons/list-tree';
	import Moon from '@lucide/svelte/icons/moon';
	import Empty from './empty.svelte';
	import TrustBadge from './trust-badge.svelte';
	import type { CatalogView, LiveAgent, LiveView } from '$lib/api';
	import { count, delay, harness, lasted, short, tokens } from '$lib/format';

	let {
		live,
		catalog,
		seconds = $bindable(),
		openThread
	}: {
		live: LiveView | null;
		/** Only for the names: a route serves under a technical id and the
		 *  catalogue is where the readable one lives. */
		catalog: CatalogView | null;
		/** How long an agent stays on screen after its last call. */
		seconds: number;
		/** Send a conversation to the traffic screen, filtered on it. */
		openThread: (thread: string) => void;
	} = $props();

	const WINDOWS = [
		{ value: '60', label: '1 min' },
		{ value: '300', label: '5 min' },
		{ value: '900', label: '15 min' },
		{ value: '3600', label: '1 h' }
	];

	let chosen = $state(String(seconds));
	$effect(() => {
		seconds = Number(chosen);
	});

	/* The delays on this screen count in seconds, so they cannot wait for the
	   next poll to move. The offset is the gap between the broker's clock and
	   this browser's, measured on every answer: ages stay right on a machine
	   whose clock is minutes off. */
	let tick = $state(0);
	$effect(() => {
		const timer = setInterval(() => (tick += 1), 1000);
		return () => clearInterval(timer);
	});
	const offset = $derived(live ? Date.now() - Date.parse(live.now) : 0);
	const now = $derived.by(() => {
		void tick;
		return new Date(Date.now() - offset).toISOString();
	});

	const agents = $derived(live?.agents ?? []);
	const working = $derived(agents.filter((a) => a.in_flight > 0).length);
	const labels = $derived(
		new Map((catalog?.models ?? []).filter((m) => m.label).map((m) => [m.route, m.label as string]))
	);

	/** What to call the model: the readable name when the catalogue has one, the
	 *  route otherwise. Never the upstream id, which is the same model under the
	 *  reseller's own spelling. */
	function modelName(a: LiveAgent): string {
		return labels.get(a.route) ?? a.route;
	}

	/** A colour per agent, derived from its own key so it holds across refreshes
	 *  and across screens. Two agents on the same model are told apart at a
	 *  glance, which a column of identical cards cannot do. */
	function hue(key: string): number {
		let h = 0;
		for (const c of key) h = (h * 31 + c.charCodeAt(0)) % 360;
		return h;
	}

	/** The short fingerprint, which is the only name an agent has: nothing
	 *  identifying is read out of a conversation to build one. */
	function tag(a: LiveAgent): string {
		return a.thread_fingerprint ? short(a.thread_fingerprint, 4) : a.client;
	}
</script>

<div class="space-y-4">
	<Card.Root class="shadow-panel overflow-hidden">
		<div class="flex flex-wrap items-center justify-between gap-4 px-6 py-5">
			<div class="min-w-0">
				<h2 class="flex items-baseline gap-2.5 text-xl leading-tight font-semibold sm:text-2xl">
					{#if agents.length === 0}
						Personne au poste
					{:else}
						<span class="ledger text-3xl leading-none">{count(agents.length)}</span>
						{agents.length > 1 ? 'agents au travail' : 'agent au travail'}
					{/if}
				</h2>
				<p class="text-muted-foreground mt-1.5 text-sm leading-relaxed">
					{#if agents.length === 0}
						Aucun appel n'est passé par douane sur cette fenêtre. Un agent apparaît ici dès son
						premier appel et s'efface tout seul quand il se tait.
					{:else if working > 0}
						<!-- The only figure on this screen that says now rather than a
						     moment ago: a call whose answer has not landed yet. -->
						{count(working)}
						{working > 1 ? 'sont en train de répondre' : 'est en train de répondre'} à l'instant. Un
						fil disparaît de lui-même après {WINDOWS.find((w) => w.value === chosen)?.label} de
						silence.
					{:else}
						Aucun appel en vol : ils réfléchissent, lisent des fichiers, ou attendent. Un fil
						disparaît de lui-même après {WINDOWS.find((w) => w.value === chosen)?.label} de silence.
					{/if}
				</p>
			</div>

			<div class="shrink-0">
				<div class="eyebrow mb-1.5">silence toléré</div>
				<ToggleGroup.Root type="single" size="sm" bind:value={chosen}>
					{#each WINDOWS as w (w.value)}
						<ToggleGroup.Item value={w.value} class="px-3 text-xs">{w.label}</ToggleGroup.Item>
					{/each}
				</ToggleGroup.Root>
			</div>
		</div>
	</Card.Root>

	{#if agents.length === 0}
		<Card.Root class="shadow-panel overflow-hidden py-0">
			<Empty
				icon={Moon}
				title="rien ne passe la barrière"
				hint="le poste est ouvert, personne ne s’est présenté. Chaque appel relayé fait apparaître son agent ici en moins de cinq secondes."
			/>
		</Card.Root>
	{:else}
		<div class="grid items-start gap-3 md:grid-cols-2 xl:grid-cols-3">
			{#each agents as a (a.key)}
				{@const busy = a.in_flight > 0}
				{@const h = hue(a.key)}
				<Card.Root class="shadow-panel gap-0 overflow-hidden py-0">
					<!-- The agent's own colour as a stripe: the card is identified before
					     a word of it is read. -->
					<div class="h-1" style="background: oklch(0.68 0.14 {h})"></div>

					<div class="flex flex-col gap-3.5 p-4">
						<div class="flex items-start gap-3">
							<span
								class="tabular flex size-9 shrink-0 items-center justify-center rounded-lg text-[0.7rem] font-bold"
								style="color: oklch(0.55 0.16 {h}); background: color-mix(in oklab, oklch(0.68 0.14 {h}) 16%, transparent)"
								title={a.thread_fingerprint ?? a.client}
							>
								{tag(a)}
							</span>
							<div class="min-w-0 flex-1">
								<div class="truncate text-[0.95rem] leading-tight font-semibold">
									{modelName(a)}
								</div>
								<div class="text-muted-foreground mt-1 flex items-center gap-1.5 text-xs">
									<span class="truncate">{a.provider}</span>
									<TrustBadge trust={a.trust} />
								</div>
							</div>
						</div>

						<!-- The state, in one line. Generating gets the pulse; anything else
						     gets the delay since its last call, which is the honest answer:
						     douane sees the calls, not the thinking between them. -->
						<div class="flex items-center gap-2 text-xs">
							{#if busy}
								<span class="text-trust-max relative flex size-2 shrink-0 rounded-full bg-current">
									<span
										class="absolute inline-flex size-full animate-ping rounded-full bg-current opacity-60"
									></span>
								</span>
								<span class="text-trust-max font-semibold">
									en train de répondre{a.streamed ? ' · en flux' : ''}
								</span>
							{:else if a.errors > 0 && a.last_status !== null && a.last_status >= 400}
								<span class="pill pill-block">dernier appel en échec · {a.last_status}</span>
							{:else}
								<span class="bg-muted-foreground/50 size-2 shrink-0 rounded-full"></span>
								<span class="text-muted-foreground">{delay(now, a.last_at)}</span>
							{/if}
						</div>

						<dl class="grid grid-cols-3 gap-2 text-xs">
							<div>
								<dt class="eyebrow">appels</dt>
								<dd class="tabular mt-0.5 font-semibold">{count(a.calls)}</dd>
							</div>
							<div>
								<dt class="eyebrow">entrée</dt>
								<dd class="tabular text-muted-foreground mt-0.5">{tokens(a.input_tokens)}</dd>
							</div>
							<div>
								<dt class="eyebrow">sortie</dt>
								<dd class="tabular text-muted-foreground mt-0.5">{tokens(a.output_tokens)}</dd>
							</div>
						</dl>

						<div class="text-muted-foreground flex flex-wrap items-center gap-x-2 gap-y-1 text-xs">
							<span class="text-foreground font-medium">{a.client}</span>
							{#if harness(a.user_agent)}
								<span aria-hidden="true">·</span>
								<span title={a.user_agent}>{harness(a.user_agent)}</span>
							{/if}
							<span aria-hidden="true">·</span>
							<span>depuis {lasted(a.first_at, a.last_at) || 'moins d’une seconde'}</span>
						</div>

						{#if a.thread_fingerprint}
							<Button
								variant="outline"
								size="sm"
								class="h-8 w-full text-xs"
								onclick={() => openThread(a.thread_fingerprint as string)}
							>
								<ListTree class="size-3.5" />
								Voir ses appels
							</Button>
						{/if}
					</div>
				</Card.Root>
			{/each}
		</div>
	{/if}
</div>
