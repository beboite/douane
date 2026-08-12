<script lang="ts">
	import { toast } from 'svelte-sonner';
	import { Button } from '$lib/components/ui/button';
	import * as Table from '$lib/components/ui/table';
	import Activity from '@lucide/svelte/icons/activity';
	import Server from '@lucide/svelte/icons/server';
	import Signpost from '@lucide/svelte/icons/signpost';
	import Empty from './empty.svelte';
	import Panel from './panel.svelte';
	import RouteRow from './route-row.svelte';
	import TrustBadge from './trust-badge.svelte';
	import { api, type ConfigView, type Probe } from '$lib/api';
	import { session } from '$lib/session.svelte';

	let { config, reload }: { config: ConfigView | null; reload: () => Promise<void> } = $props();

	/* One real one-token call per provider, with the provider's real key. `ok`
	   therefore means "the key works today", which no listing can say. */
	let probing = $state(false);
	let probes = $state<Map<string, Probe> | null>(null);

	async function runProbe() {
		probing = true;
		try {
			const { probes: results } = await api.probe(session.token);
			probes = new Map(results.map((p) => [p.provider, p]));
			const dead = results.filter((p) => !p.ok);
			if (dead.length === 0) {
				toast.success('toutes les clés répondent');
			} else {
				toast.error(
					`${dead.length} clé${dead.length > 1 ? 's' : ''} en panne : ${dead.map((p) => p.provider).join(', ')}`
				);
			}
		} catch (e) {
			toast.error(e instanceof Error ? e.message : 'la sonde a échoué');
		} finally {
			probing = false;
		}
	}

	/* A route that has been repointed is keyed apart from one that has not, so a
	   reload rebuilds the row and its draft starts again from what the broker
	   actually serves rather than from what was typed before. */
	const key = (name: string, stamp: string) => `${name}:${stamp}`;
	const stamp = $derived(
		(config?.routes ?? []).map((r) => `${r.provider}/${r.model ?? ''}`).join('|')
	);
</script>

<div class="space-y-5">
	<Panel
		title="Routage"
		note="un changement s’applique à l’appel suivant et vit en base. Le fichier n’est jamais réécrit, donc un redémarrage revient au TOML"
		flush
	>
		{#if (config?.routes ?? []).length === 0}
			<Empty
				icon={Signpost}
				title="aucune route configurée"
				hint="une route donne un nom de modèle à un agent et décide chez qui l’appel part réellement"
			/>
		{:else}
			<Table.Root>
				<Table.Header>
					<Table.Row>
						<Table.Head class="w-[18rem] px-5 py-3">Le modèle demandé</Table.Head>
						<Table.Head class="w-[15rem] px-3 py-3">Part chez</Table.Head>
						<Table.Head class="px-3 py-3">Sous le nom</Table.Head>
						<Table.Head class="w-[13rem] px-5 py-3 text-right">Action</Table.Head>
					</Table.Row>
				</Table.Header>
				<Table.Body>
					{#each config?.routes ?? [] as route (key(route.name, stamp))}
						<RouteRow {route} providers={config?.providers ?? []} {reload} />
					{/each}
				</Table.Body>
			</Table.Root>
		{/if}
	</Panel>

	<div class="grid items-start gap-5 lg:grid-cols-2">
		<Panel
			title="Fournisseurs"
			note="qui reçoit vraiment les conversations. Le test dépense un petit appel réel par fournisseur"
			flush
		>
			{#snippet actions()}
				<Button size="sm" class="shrink-0" disabled={probing} onclick={runProbe}>
					<Activity class="size-4" />
					{probing ? 'test en cours' : 'tester les clés'}
				</Button>
			{/snippet}
			<ul class="divide-border divide-y">
				{#each config?.providers ?? [] as p (p.name)}
					{@const probe = probes?.get(p.name)}
					<li class="px-5 py-3">
						<div class="flex items-center gap-3">
							<Server class="text-muted-foreground/60 size-4 shrink-0" />
							<div class="min-w-0 flex-1">
								<div class="flex items-center gap-2">
									<span class="truncate text-sm font-medium">{p.name}</span>
									<TrustBadge trust={p.trust} />
								</div>
								<div class="text-muted-foreground truncate text-xs">{p.base_url}</div>
							</div>
							<!-- The dot carries the answer, so it says the three states apart: the
							     key answered, the key did not, or nobody has asked today. -->
							{#if probe}
								<span
									class="pill tabular {probe.ok ? 'pill-ok' : 'pill-block'}"
									title={probe.ok ? 'la clé a répondu' : 'la clé n’a pas répondu'}
								>
									{probe.ok ? `répond en ${probe.ms} ms` : (probe.status ? `erreur ${probe.status}` : 'injoignable')}
								</span>
							{:else}
								<span class="pill pill-quiet" title="aucun test depuis l’ouverture de la page">
									{p.keyed ? 'clé en coffre' : 'sans clé'}
								</span>
							{/if}
						</div>
						{#if probe && !probe.ok && probe.detail}
							<p class="text-destructive/90 mt-2 ml-7 line-clamp-2 text-xs">{probe.detail}</p>
						{/if}
					</li>
				{/each}
			</ul>
		</Panel>

		<Panel title="Clés virtuelles" note="qui a le droit d’appeler, et jusqu’où">
			<div class="space-y-3">
				{#each config?.clients ?? [] as c (c.name)}
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
		</Panel>
	</div>
</div>
