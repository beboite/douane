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

	const window = $derived(
		hours <= 1
			? 'la dernière heure'
			: hours <= 24
				? `les ${hours} dernières heures`
				: `les ${Math.round(hours / 24)} derniers jours`
	);

	const failed = (c: Call) => c.error !== null || (c.status !== null && c.status >= 400);
</script>

<div class="space-y-4">
	<Card.Root class="shadow-panel gap-0 overflow-hidden py-0">
		<div class="px-2 pt-4 pb-2">
			<div class="mb-2 flex items-center justify-between gap-3 px-2">
				<div class="min-w-0">
					<h2 class="text-sm font-semibold">Activité</h2>
					<p class="text-muted-foreground text-xs">
						{#if errors > 0}
							<span class="text-trust-low font-medium">{count(errors)} en échec</span> sur
							{count(totals?.calls ?? 0)} appels sur {window}
						{:else}
							{count(totals?.calls ?? 0)} appels, {tokens(
								(totals?.input_tokens ?? 0) + (totals?.output_tokens ?? 0)
							)} tokens sur {window}
						{/if}
					</p>
				</div>
				<ToggleGroup.Root type="single" size="sm" bind:value={measure} class="shrink-0">
					<ToggleGroup.Item value="calls" class="h-7 px-2.5 text-xs">appels</ToggleGroup.Item>
					<ToggleGroup.Item value="tokens" class="h-7 px-2.5 text-xs">tokens</ToggleGroup.Item>
				</ToggleGroup.Root>
			</div>

			{#if series.length}
				<ActivityChart {series} bind:measure />
			{/if}

			{#if (totals?.calls ?? 0) === 0}
				<p class="text-muted-foreground -mt-24 mb-16 text-center text-xs">
					aucun appel sur cette fenêtre
				</p>
			{/if}
		</div>
	</Card.Root>

	<!-- `items-start` so a panel with one row does not stretch to match a panel
	     with ten and leave a rectangle of nothing under it. -->
	<div class="grid items-start gap-4 lg:grid-cols-2">
		<Panel
			title="Par route"
			note="qui a servi chaque nom de modèle, et si le cache tenait"
			flush
		>
			<Ranked rows={summary?.by_route ?? []} empty="aucun agent n’a demandé de modèle" />
		</Panel>
		<Panel title="Par client" note="ce que chaque clé virtuelle a fait passer" flush>
			<Ranked rows={summary?.by_client ?? []} empty="aucune clé virtuelle ne s’est présentée" />
		</Panel>
	</div>

	<Panel
		title="Conversations"
		note="regroupées par empreinte du fil, ce qui suit une session d’agent sans jamais en lire le contenu. Cliquer une ligne filtre le journal en dessous"
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
			<Table.Root>
				<Table.Header class="bg-card sticky top-0 z-10">
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
							<Table.Cell class="font-mono text-xs" title={t.fingerprint}>
								{short(t.fingerprint)}
							</Table.Cell>
							<Table.Cell class="text-muted-foreground text-xs">{when(t.first_at)}</Table.Cell>
							<Table.Cell class="text-muted-foreground text-xs">{when(t.last_at)}</Table.Cell>
							<Table.Cell class="text-xs">{t.providers}</Table.Cell>
							<Table.Cell class="tabular text-right">{count(t.calls)}</Table.Cell>
							<Table.Cell class="tabular text-muted-foreground text-right">
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
		note="le dernier passé en premier"
		flush
		cap={calls.length ? 'max-h-[42rem]' : ''}
	>
		{#snippet actions()}
			{#if filter}
				<Button variant="secondary" size="sm" class="h-8 shrink-0" onclick={() => (filter = null)}>
					<X class="size-3.5" />
					fil {short(filter)}
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
			<Table.Root>
				<Table.Header class="bg-card sticky top-0 z-10">
					<Table.Row>
						<Table.Head class="w-[6rem]">quand</Table.Head>
						<Table.Head>client</Table.Head>
						<Table.Head>demandé</Table.Head>
						<Table.Head>parti chez</Table.Head>
						<Table.Head>sous le nom</Table.Head>
						<Table.Head class="text-right">entrée</Table.Head>
						<Table.Head class="text-right">sortie</Table.Head>
						<Table.Head class="text-right">cache écrit / lu</Table.Head>
						<Table.Head class="text-right">durée</Table.Head>
						<Table.Head class="text-right">statut</Table.Head>
					</Table.Row>
				</Table.Header>
				<Table.Body>
					{#each calls as c (c.id)}
						<Table.Row class={failed(c) ? 'bg-destructive/5' : ''}>
							<Table.Cell class="text-muted-foreground text-xs" title={c.at}>
								{when(c.at)}
							</Table.Cell>
							<Table.Cell class="text-sm">{c.client}</Table.Cell>
							<Table.Cell class="font-mono text-xs">{c.model_requested}</Table.Cell>
							<Table.Cell>
								<span class="flex items-center gap-2 text-sm">
									{c.provider}
									<TrustBadge trust={c.trust} />
								</span>
							</Table.Cell>
							<Table.Cell class="text-muted-foreground font-mono text-xs">
								{c.model_sent}
								{#if c.streamed}<span class="ml-1 opacity-60">flux</span>{/if}
							</Table.Cell>
							<Table.Cell class="tabular text-muted-foreground text-right text-xs">
								{tokens(c.input_tokens)}
							</Table.Cell>
							<Table.Cell class="tabular text-muted-foreground text-right text-xs">
								{tokens(c.output_tokens)}
							</Table.Cell>
							<!-- The two cache counts of a single call, which is the only place
							     a rewritten prefix is visible: a turn that writes more than it
							     reads back paid for a context the upstream already had. -->
							<Table.Cell class="tabular text-muted-foreground text-right text-xs">
								{#if c.cache_write_tokens === null && c.cache_read_tokens === null}
									—
								{:else}
									<span
										class={(c.cache_write_tokens ?? 0) > (c.cache_read_tokens ?? 0)
											? 'text-trust-low font-medium'
											: ''}
									>
										{tokens(c.cache_write_tokens ?? 0)}
									</span>
									/ {tokens(c.cache_read_tokens ?? 0)}
								{/if}
							</Table.Cell>
							<Table.Cell class="tabular text-muted-foreground text-right text-xs">
								{ms(c.duration_ms)}
							</Table.Cell>
							<Table.Cell class="tabular text-right text-xs">
								{#if c.error}
									<span class="text-destructive" title={c.error}>échec</span>
								{:else if c.status && c.status >= 400}
									<span class="text-destructive">{c.status}</span>
								{:else if c.status}
									<span class="text-muted-foreground">{c.status}</span>
								{:else}
									<span class="text-muted-foreground">en vol</span>
								{/if}
							</Table.Cell>
						</Table.Row>
					{/each}
				</Table.Body>
			</Table.Root>
		{/if}
	</Panel>
</div>
