<script lang="ts">
	import * as Table from '$lib/components/ui/table';
	import Panel from './panel.svelte';
	import Stat from './stat.svelte';
	import TrustBadge from './trust-badge.svelte';
	import type { Alert, Bucket, ConfigView, Summary } from '$lib/api';
	import { count, money, tokens } from '$lib/format';

	let {
		summary,
		config,
		alerts,
		hours
	}: {
		summary: Summary | null;
		config: ConfigView | null;
		alerts: Alert[];
		hours: number;
	} = $props();

	const totals = $derived(summary?.totals);
	const blocked = $derived(alerts.filter((a) => a.action === 'block').length);

	/* An unpriced route is not a free one. Saying so here stops the cost figure
	   from being read as the whole bill. */
	const unpriced = $derived(config?.routes.filter((r) => !r.price).length ?? 0);
	const spentHint = $derived(
		unpriced > 0
			? `${unpriced} route${unpriced > 1 ? 's' : ''} sans tarif, donc non comptée${unpriced > 1 ? 's' : ''}`
			: 'toutes les routes sont tarifées'
	);
</script>

<div class="grid gap-4 sm:grid-cols-2 xl:grid-cols-4">
	<Stat label="appels" value={count(totals?.calls ?? 0)} hint="sur {hours} h" />
	<Stat
		label="dépensé"
		value={money(totals?.cost) || '0.00'}
		tone="accent"
		hint={spentHint}
	/>
	<Stat
		label="tokens"
		value={tokens((totals?.input_tokens ?? 0) + (totals?.output_tokens ?? 0))}
		hint="{tokens(totals?.input_tokens)} entrants · {tokens(totals?.output_tokens)} sortants"
	/>
	<Stat
		label="fuites bloquées"
		value={count(blocked)}
		tone={blocked > 0 ? 'warn' : 'plain'}
		hint="{count(alerts.length)} alertes au total"
	/>
</div>

{#snippet buckets(rows: Bucket[], head: string)}
	<Table.Root>
		<Table.Header>
			<Table.Row>
				<Table.Head>{head}</Table.Head>
				<Table.Head class="text-right">appels</Table.Head>
				<Table.Head class="text-right">erreurs</Table.Head>
				<Table.Head class="text-right">tokens</Table.Head>
				<Table.Head class="text-right">coût</Table.Head>
			</Table.Row>
		</Table.Header>
		<Table.Body>
			{#each rows as row (row.key)}
				<Table.Row>
					<Table.Cell class="font-medium">{row.key}</Table.Cell>
					<Table.Cell class="tabular text-right">{count(row.calls)}</Table.Cell>
					<Table.Cell
						class="tabular text-right {row.errors > 0 ? 'text-trust-low font-medium' : 'text-muted-foreground'}"
					>
						{count(row.errors)}
					</Table.Cell>
					<Table.Cell class="tabular text-muted-foreground text-right">
						{tokens(row.input_tokens + row.output_tokens)}
					</Table.Cell>
					<Table.Cell class="tabular text-right">{money(row.cost) || '—'}</Table.Cell>
				</Table.Row>
			{:else}
				<Table.Row>
					<Table.Cell colspan={5} class="text-muted-foreground py-8 text-center">
						rien sur cette fenêtre
					</Table.Cell>
				</Table.Row>
			{/each}
		</Table.Body>
	</Table.Root>
{/snippet}

<div class="mt-4 grid gap-4 lg:grid-cols-2">
	<Panel title="Par route" note="ce que chaque nom de modèle a coûté" flush>
		{@render buckets(summary?.by_route ?? [], 'route')}
	</Panel>
	<Panel title="Par client" note="quelle clé virtuelle a dépensé quoi" flush>
		{@render buckets(summary?.by_client ?? [], 'client')}
	</Panel>
</div>

<div class="mt-4 grid gap-4 lg:grid-cols-2">
	<Panel title="Fournisseurs" note="qui reçoit vraiment les conversations" flush>
		<Table.Root>
			<Table.Header>
				<Table.Row>
					<Table.Head>nom</Table.Head>
					<Table.Head>confiance</Table.Head>
					<Table.Head>adresse</Table.Head>
					<Table.Head class="text-right">clé</Table.Head>
				</Table.Row>
			</Table.Header>
			<Table.Body>
				{#each config?.providers ?? [] as p (p.name)}
					<Table.Row>
						<Table.Cell class="font-medium">{p.name}</Table.Cell>
						<Table.Cell><TrustBadge trust={p.trust} /></Table.Cell>
						<Table.Cell class="text-muted-foreground font-mono text-xs">{p.base_url}</Table.Cell>
						<Table.Cell class="text-right text-xs">
							{#if p.keyed}
								<span class="text-muted-foreground">en coffre</span>
							{:else}
								<span class="text-muted-foreground/60">aucune</span>
							{/if}
						</Table.Cell>
					</Table.Row>
				{/each}
			</Table.Body>
		</Table.Root>
	</Panel>

	<Panel title="État" note="ce que le broker fait en ce moment">
		<dl class="grid gap-3 text-sm">
			<div class="flex items-baseline justify-between gap-4">
				<dt class="text-muted-foreground">valeurs surveillées en sortie</dt>
				<dd class="tabular font-medium">{count(config?.watching ?? 0)}</dd>
			</div>
			<div class="flex items-baseline justify-between gap-4">
				<dt class="text-muted-foreground">routes sans tarif</dt>
				<dd class="tabular font-medium">
					{count(unpriced)} / {count(config?.routes.length ?? 0)}
				</dd>
			</div>
			<div class="flex items-baseline justify-between gap-4">
				<dt class="text-muted-foreground">clients autorisés</dt>
				<dd class="tabular font-medium">{count(config?.clients.length ?? 0)}</dd>
			</div>
			<div class="flex items-baseline justify-between gap-4">
				<dt class="text-muted-foreground">proxy de sortie</dt>
				<dd class="font-medium">
					{#if config?.egress.listening}
						à l'écoute · {count(config.egress.rules)} règles
					{:else}
						<span class="text-muted-foreground">éteint</span>
					{/if}
				</dd>
			</div>
		</dl>

		{#if config?.clients.length}
			<div class="mt-4 space-y-2 border-t pt-4">
				{#each config.clients as c (c.name)}
					<div class="flex items-center justify-between gap-3 text-sm">
						<span class="font-medium">{c.name}</span>
						<span class="flex items-center gap-2">
							{#if c.note}<span class="text-muted-foreground truncate text-xs">{c.note}</span>{/if}
							<TrustBadge trust={c.max_trust} />
						</span>
					</div>
				{/each}
			</div>
		{/if}
	</Panel>
</div>
