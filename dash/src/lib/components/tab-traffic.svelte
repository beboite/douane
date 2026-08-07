<script lang="ts">
	import { Button } from '$lib/components/ui/button';
	import * as Table from '$lib/components/ui/table';
	import MessagesSquare from '@lucide/svelte/icons/messages-square';
	import Radio from '@lucide/svelte/icons/radio';
	import X from '@lucide/svelte/icons/x';
	import Empty from './empty.svelte';
	import Panel from './panel.svelte';
	import TrustBadge from './trust-badge.svelte';
	import type { Call, Thread } from '$lib/api';
	import { amount, count, ms, short, tokens, when } from '$lib/format';

	let {
		calls,
		threads,
		filter = $bindable()
	}: {
		calls: Call[];
		threads: Thread[];
		/** The thread fingerprint the call list is narrowed to, if any. Bound so
		 *  clicking a conversation above filters the log below. */
		filter: string | null;
	} = $props();

	const failed = (c: Call) => c.error !== null || (c.status !== null && c.status >= 400);
</script>

<div class="space-y-4">
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
						<Table.Head class="text-right">coût</Table.Head>
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
							<Table.Cell class="tabular text-right">
								{Object.entries(t.cost)
									.map(([c, v]) => amount(v, c))
									.join(' + ') || '—'}
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
						<Table.Head class="text-right">coût</Table.Head>
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
								{:else}
									{amount(c.cost, c.currency) || '—'}
								{/if}
							</Table.Cell>
						</Table.Row>
					{/each}
				</Table.Body>
			</Table.Root>
		{/if}
	</Panel>
</div>
