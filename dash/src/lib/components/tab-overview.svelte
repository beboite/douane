<script lang="ts">
	import * as Card from '$lib/components/ui/card';
	import * as ToggleGroup from '$lib/components/ui/toggle-group';
	import Activity from '@lucide/svelte/icons/activity';
	import Coins from '@lucide/svelte/icons/coins';
	import Server from '@lucide/svelte/icons/server';
	import ShieldAlert from '@lucide/svelte/icons/shield-alert';
	import Sigma from '@lucide/svelte/icons/sigma';
	import ActivityChart from './activity-chart.svelte';
	import Metric from './metric.svelte';
	import Panel from './panel.svelte';
	import Ranked from './ranked.svelte';
	import TrustBadge from './trust-badge.svelte';
	import type { Alert, ConfigView, Summary } from '$lib/api';
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

	let measure = $state<'calls' | 'tokens'>('calls');

	const totals = $derived(summary?.totals);

	/* The headline counts leaks only. A routing refusal is douane declining to
	   spend money at a provider the client is not allowed to reach, which is
	   worth seeing but is not a credential getting out, and folding the two into
	   one figure would turn a config rule into an alarm. */
	const blocked = $derived(
		alerts.filter((a) => a.action === 'block' && (a.kind === 'vault' || a.kind === 'shape')).length
	);
	const refused = $derived(
		alerts.filter((a) => a.action === 'block' && (a.kind === 'trust' || a.kind === 'route')).length
	);
	const series = $derived(summary?.series ?? []);

	const window = $derived(
		hours <= 1 ? 'la dernière heure' : hours <= 24 ? `les ${hours} dernières heures` : `les ${Math.round(hours / 24)} derniers jours`
	);

	/* An unpriced route is not a free one. Saying so next to the figure stops the
	   cost from being read as the whole bill. */
	const unpriced = $derived(config?.routes.filter((r) => !r.price).length ?? 0);
	const spentHint = $derived(
		unpriced > 0
			? `${unpriced} route${unpriced > 1 ? 's' : ''} sans tarif, donc non comptée${unpriced > 1 ? 's' : ''}`
			: 'toutes les routes sont tarifées'
	);

	const errors = $derived(totals?.errors ?? 0);
</script>

<div class="space-y-4">
	<Card.Root class="shadow-panel gap-0 overflow-hidden py-0">
		<!-- The four figures and the curve they came from, in one object. Split
		     across five cards they read as five unrelated facts. -->
		<div class="divide-border grid divide-y sm:grid-cols-2 sm:divide-x xl:grid-cols-4">
			<Metric
				label="appels"
				value={totals?.calls ?? 0}
				format={count}
				icon={Activity}
				hint="sur {window}"
			/>
			<Metric
				label="dépensé"
				text={money(totals?.cost) || '0.00'}
				icon={Coins}
				tone="accent"
				hint={spentHint}
			/>
			<Metric
				label="tokens"
				value={(totals?.input_tokens ?? 0) + (totals?.output_tokens ?? 0)}
				format={tokens}
				icon={Sigma}
				hint="{tokens(totals?.input_tokens)} entrants · {tokens(totals?.output_tokens)} sortants"
			/>
			<Metric
				label="fuites bloquées"
				value={blocked}
				format={count}
				icon={ShieldAlert}
				tone={blocked > 0 ? 'warn' : 'quiet'}
				hint={refused > 0
					? `${count(refused)} refus de routage en plus`
					: `${count(alerts.length)} alertes au total`}
			/>
		</div>

		<div class="border-t px-2 pt-4 pb-2">
			<div class="mb-2 flex items-center justify-between gap-3 px-2">
				<div class="min-w-0">
					<h2 class="text-sm font-semibold">Activité</h2>
					<p class="text-muted-foreground text-xs">
						{#if errors > 0}
							<span class="text-trust-low font-medium">{count(errors)} en échec</span> sur
							{count(totals?.calls ?? 0)} appels
						{:else}
							ce qui est passé par le broker sur {window}
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
		<Panel title="Par route" note="ce que chaque nom de modèle a coûté" flush>
			<Ranked rows={summary?.by_route ?? []} empty="aucun agent n’a demandé de modèle" />
		</Panel>
		<Panel title="Par client" note="quelle clé virtuelle a dépensé quoi" flush>
			<Ranked rows={summary?.by_client ?? []} empty="aucune clé virtuelle ne s’est présentée" />
		</Panel>
	</div>

	<!-- `items-start` so a panel with one row does not stretch to match a panel
	     with ten and leave a rectangle of nothing under it. -->
	<div class="grid items-start gap-4 lg:grid-cols-2">
		<Panel title="Fournisseurs" note="qui reçoit vraiment les conversations" flush>
			<ul class="divide-border divide-y">
				{#each config?.providers ?? [] as p (p.name)}
					<li class="flex items-center gap-3 px-4 py-2.5">
						<Server class="text-muted-foreground/60 size-4 shrink-0" />
						<div class="min-w-0 flex-1">
							<div class="flex items-center gap-2">
								<span class="truncate text-sm font-medium">{p.name}</span>
								<TrustBadge trust={p.trust} />
							</div>
							<div class="text-muted-foreground truncate font-mono text-xs">{p.base_url}</div>
						</div>
						<span class="text-muted-foreground shrink-0 text-xs">
							{p.keyed ? 'clé en coffre' : 'sans clé'}
						</span>
					</li>
				{/each}
			</ul>
		</Panel>

		<Panel title="État" note="ce que le broker fait en ce moment">
			<dl class="grid gap-2.5 text-sm">
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
					<dt class="text-muted-foreground">proxy de sortie</dt>
					<dd class="font-medium">
						{#if config?.egress.listening}
							à l’écoute · {count(config.egress.rules)} règles
						{:else}
							<span class="text-muted-foreground">éteint</span>
						{/if}
					</dd>
				</div>
			</dl>

			{#if config?.clients.length}
				<div class="mt-4 space-y-2 border-t pt-3.5">
					<p class="eyebrow">clés virtuelles admises</p>
					{#each config.clients as c (c.name)}
						<div class="flex items-center justify-between gap-3 text-sm">
							<span class="truncate font-medium">{c.name}</span>
							<span class="flex shrink-0 items-center gap-2">
								{#if c.note}
									<span class="text-muted-foreground hidden max-w-[16rem] truncate text-xs sm:inline">
										{c.note}
									</span>
								{/if}
								<TrustBadge trust={c.max_trust} />
							</span>
						</div>
					{/each}
				</div>
			{/if}
		</Panel>
	</div>
</div>
