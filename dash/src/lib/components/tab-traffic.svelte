<script lang="ts">
	import { Button } from '$lib/components/ui/button';
	import * as Table from '$lib/components/ui/table';
	import X from '@lucide/svelte/icons/x';
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
		cap="max-h-[22rem]"
	>
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
						class="cursor-pointer {filter === t.fingerprint ? 'bg-accent/50' : ''}"
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
				{:else}
					<Table.Row>
						<Table.Cell colspan={7} class="text-muted-foreground py-8 text-center">
							aucune conversation sur cette fenêtre
						</Table.Cell>
					</Table.Row>
				{/each}
			</Table.Body>
		</Table.Root>
	</Panel>

	<Panel title="Appels" note="le dernier passé en premier" flush cap="max-h-[42rem]">
		{#snippet actions()}
			{#if filter}
				<Button variant="ghost" size="sm" onclick={() => (filter = null)}>
					<X class="size-3.5" />
					fil {short(filter)}
				</Button>
			{/if}
		{/snippet}

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
				{:else}
					<Table.Row>
						<Table.Cell colspan={9} class="text-muted-foreground py-8 text-center">
							aucun appel
						</Table.Cell>
					</Table.Row>
				{/each}
			</Table.Body>
		</Table.Root>
	</Panel>
</div>
