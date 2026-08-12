<script lang="ts">
	import { Button } from '$lib/components/ui/button';
	import * as Card from '$lib/components/ui/card';
	import * as Table from '$lib/components/ui/table';
	import * as ToggleGroup from '$lib/components/ui/toggle-group';
	import MessagesSquare from '@lucide/svelte/icons/messages-square';
	import Radio from '@lucide/svelte/icons/radio';
	import X from '@lucide/svelte/icons/x';
	import ActivityChart from './activity-chart.svelte';
	import Empty from './empty.svelte';
	import Panel from './panel.svelte';
	import Ranked from './ranked.svelte';
	import TrustBadge from './trust-badge.svelte';
	import type { Call, Summary, Thread } from '$lib/api';
	import { count, ms, short, tokens, when } from '$lib/format';

	let {
		summary,
		hours,
		calls,
		threads,
		filter = $bindable()
	}: {
		summary: Summary | null;
		hours: number;
		calls: Call[];
		threads: Thread[];
		/** The thread fingerprint the call list is narrowed to, if any. Bound so
		 *  clicking a conversation above filters the log below. */
		filter: string | null;
	} = $props();

	let measure = $state<'calls' | 'tokens'>('calls');

	const totals = $derived(summary?.totals);
	const series = $derived(summary?.series ?? []);
	const errors = $derived(totals?.errors ?? 0);

	/* Named `windowLabel` rather than `window`: a component that shadows the
	   browser global is one careless line away from a very confusing bug. */
	const windowLabel = $derived(
		hours <= 1
			? 'sur la dernière heure'
			: hours <= 24
				? `sur les ${hours} dernières heures`
				: `sur les ${Math.round(hours / 24)} derniers jours`
	);

	const failed = (c: Call) => c.error !== null || (c.status !== null && c.status >= 400);

	/* Every table on this screen is wider than a phone, so the columns keep their
	   width and the panel scrolls sideways around them. The rest is breathing
	   room: a log read line by line needs the lines apart. */
	const cells =
		'[&_td]:py-3 [&_th:first-child]:pl-5 [&_td:first-child]:pl-5 [&_th:last-child]:pr-5 [&_td:last-child]:pr-5';
	const heads =
		'bg-card sticky top-0 z-10 [&_th]:text-muted-foreground [&_th]:text-[0.7rem] [&_th]:font-semibold [&_th]:uppercase [&_th]:tracking-[0.06em]';
</script>

<div class="space-y-4">
	<Card.Root class="shadow-panel gap-0 overflow-hidden py-0">
		<!-- The window's totals as ledger figures rather than a sentence: three
		     numbers side by side compare, a sentence has to be read. -->
		<div class="flex flex-wrap items-end justify-between gap-4 px-5 pt-5 pb-3">
			<div class="flex min-w-0 items-baseline gap-6">
				<div>
					<div class="eyebrow">appels {windowLabel}</div>
					<div class="ledger mt-1 text-2xl leading-none">{count(totals?.calls ?? 0)}</div>
				</div>
				<div>
					<div class="eyebrow">tokens</div>
					<div class="ledger text-muted-foreground mt-1 text-2xl leading-none">
						{tokens((totals?.input_tokens ?? 0) + (totals?.output_tokens ?? 0))}
					</div>
				</div>
				{#if errors > 0}
					<div>
						<div class="eyebrow text-destructive">en échec</div>
						<div class="ledger text-destructive mt-1 text-2xl leading-none">{count(errors)}</div>
					</div>
				{/if}
			</div>
			<ToggleGroup.Root type="single" size="sm" bind:value={measure} class="shrink-0">
				<ToggleGroup.Item value="calls" class="px-3 text-xs">appels</ToggleGroup.Item>
				<ToggleGroup.Item value="tokens" class="px-3 text-xs">tokens</ToggleGroup.Item>
			</ToggleGroup.Root>
		</div>

		<div class="relative px-2 pb-4">
			{#if series.length}
				<ActivityChart {series} bind:measure />
			{/if}
			<!-- Sitting on top of the empty axes rather than under them: an axis with
			     no bars still shows the window, and the sentence says why. -->
			{#if (totals?.calls ?? 0) === 0}
				<p
					class="text-muted-foreground text-sm {series.length
						? 'absolute inset-0 flex items-center justify-center'
						: 'py-14 text-center'}"
				>
					aucun appel sur cette fenêtre
				</p>
			{/if}
		</div>
	</Card.Root>

	<!-- `items-start` so a panel with one row does not stretch to match a panel
	     with ten and leave a rectangle of nothing under it. -->
	<div class="grid items-start gap-4 lg:grid-cols-2">
		<Panel title="Par route" note="qui a servi chaque nom de modèle, et si le cache tenait" flush>
			<Ranked rows={summary?.by_route ?? []} empty="aucun agent n’a demandé de modèle" />
		</Panel>
		<Panel title="Par client" note="ce que chaque clé virtuelle a fait passer" flush>
			<Ranked rows={summary?.by_client ?? []} empty="aucune clé virtuelle ne s’est présentée" />
		</Panel>
	</div>

	<Panel
		title="Conversations"
		note="regroupées par empreinte, sans jamais lire le contenu. Cliquer une ligne filtre le journal"
		flush
		cap={threads.length ? 'max-h-[22rem]' : ''}
	>
		{#if threads.length === 0}
			<Empty
				icon={MessagesSquare}
				title="aucune conversation sur cette fenêtre"
				hint="un fil apparaît dès qu’un agent enchaîne deux appels avec le même contexte"
			/>
		{:else}
			<Table.Root class={cells}>
				<Table.Header class={heads}>
					<Table.Row>
						<Table.Head>fil</Table.Head>
						<Table.Head>début</Table.Head>
						<Table.Head>dernier</Table.Head>
						<Table.Head>chez</Table.Head>
						<Table.Head class="text-right">appels</Table.Head>
						<Table.Head class="text-right">tokens</Table.Head>
					</Table.Row>
				</Table.Header>
				<Table.Body>
					{#each threads as t (t.fingerprint)}
						<Table.Row
							class="rowlink {filter === t.fingerprint ? 'bg-accent/60' : ''}"
							onclick={() => (filter = filter === t.fingerprint ? null : t.fingerprint)}
						>
							<Table.Cell class="tabular text-sm font-medium" title={t.fingerprint}>
								{short(t.fingerprint)}
							</Table.Cell>
							<Table.Cell class="text-muted-foreground text-sm">{when(t.first_at)}</Table.Cell>
							<Table.Cell class="text-muted-foreground text-sm">{when(t.last_at)}</Table.Cell>
							<Table.Cell class="text-sm">{t.providers}</Table.Cell>
							<Table.Cell class="tabular text-right text-sm font-medium">
								{count(t.calls)}
							</Table.Cell>
							<Table.Cell class="tabular text-muted-foreground text-right text-sm">
								{tokens(t.input_tokens + t.output_tokens)}
							</Table.Cell>
						</Table.Row>
					{/each}
				</Table.Body>
			</Table.Root>
		{/if}
	</Panel>

	<Panel
		title="Appels"
		note={filter ? `filtrés sur le fil ${short(filter)}` : 'le dernier passé en premier'}
		flush
		cap={calls.length ? 'max-h-[42rem]' : ''}
	>
		{#snippet actions()}
			{#if filter}
				<!-- The way back out of a filter is the one button this panel has, so
				     it says what pressing it does and is impossible to miss. -->
				<Button size="sm" class="shrink-0" onclick={() => (filter = null)}>
					<X class="size-4" />
					Voir tous les appels
				</Button>
			{/if}
		{/snippet}

		{#if calls.length === 0}
			<Empty
				icon={Radio}
				title={filter ? 'aucun appel sur ce fil' : 'aucun appel'}
				hint={filter
					? 'le fil existe mais ses appels sont hors de la fenêtre choisie'
					: 'rien n’est passé par le broker sur cette fenêtre'}
			/>
		{:else}
			<Table.Root class={cells}>
				<Table.Header class={heads}>
					<Table.Row>
						<Table.Head>quand</Table.Head>
						<Table.Head>client</Table.Head>
						<Table.Head>modèle demandé · envoyé</Table.Head>
						<Table.Head>chez</Table.Head>
						<Table.Head class="text-right">entrée</Table.Head>
						<Table.Head class="text-right">sortie</Table.Head>
						<Table.Head class="text-right">cache écrit / relu</Table.Head>
						<Table.Head class="text-right">durée</Table.Head>
						<Table.Head class="text-right">statut</Table.Head>
					</Table.Row>
				</Table.Header>
				<Table.Body>
					{#each calls as c (c.id)}
						<Table.Row class={failed(c) ? 'bg-destructive/5' : ''}>
							<Table.Cell class="tabular text-muted-foreground text-sm" title={c.at}>
								{when(c.at)}
							</Table.Cell>
							<Table.Cell class="text-sm font-medium">{c.client}</Table.Cell>
							<!-- The two names of one call stacked in a single cell: what the
							     agent asked for, and what actually crossed. Reading down the
							     column shows every rename at once. -->
							<Table.Cell class="text-sm">
								<div>{c.model_requested}</div>
								{#if c.model_sent !== c.model_requested || c.streamed}
									<div class="text-muted-foreground mt-0.5 text-xs">
										{c.model_sent}
										{#if c.streamed}<span class="ml-1">· flux</span>{/if}
									</div>
								{/if}
							</Table.Cell>
							<Table.Cell>
								<span class="flex items-center gap-2 text-sm">
									{c.provider}
									<TrustBadge trust={c.trust} />
								</span>
							</Table.Cell>
							<Table.Cell class="tabular text-muted-foreground text-right text-sm">
								{tokens(c.input_tokens)}
							</Table.Cell>
							<Table.Cell class="tabular text-muted-foreground text-right text-sm">
								{tokens(c.output_tokens)}
							</Table.Cell>
							<!-- The two cache counts of a single call, which is the only place
							     a rewritten prefix is visible: a turn that writes more than it
							     reads back paid for a context the upstream already had. -->
							<Table.Cell class="tabular text-muted-foreground text-right text-sm">
								{#if c.cache_write_tokens === null && c.cache_read_tokens === null}
									aucun
								{:else}
									<!-- The written figure carries the weight when it beats the read
									     one, in ink rather than in colour: this column is one call
									     out of hundreds, and the coloured warning belongs upstairs
									     on the route that keeps doing it. -->
									<span
										class={(c.cache_write_tokens ?? 0) > (c.cache_read_tokens ?? 0)
											? 'text-foreground font-semibold'
											: ''}
									>
										{tokens(c.cache_write_tokens ?? 0)}
									</span>
									/ {tokens(c.cache_read_tokens ?? 0)}
								{/if}
							</Table.Cell>
							<Table.Cell class="tabular text-muted-foreground text-right text-sm">
								{ms(c.duration_ms)}
							</Table.Cell>
							<!-- A status is only worth a badge when it is not the usual one:
							     two hundred green pills down a column would hide the one row
							     that failed. -->
							<Table.Cell class="text-right">
								{#if c.error}
									<span class="pill pill-block" title={c.error}>échec</span>
								{:else if c.status && c.status >= 400}
									<span class="pill pill-block tabular">{c.status}</span>
								{:else if c.status}
									<span class="tabular text-muted-foreground text-sm">{c.status}</span>
								{:else}
									<span class="pill pill-quiet">en vol</span>
								{/if}
							</Table.Cell>
						</Table.Row>
					{/each}
				</Table.Body>
			</Table.Root>
		{/if}
	</Panel>
</div>
