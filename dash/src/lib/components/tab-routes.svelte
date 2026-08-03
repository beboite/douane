<script lang="ts">
	import * as Table from '$lib/components/ui/table';
	import Signpost from '@lucide/svelte/icons/signpost';
	import Empty from './empty.svelte';
	import Panel from './panel.svelte';
	import RouteRow from './route-row.svelte';
	import type { ConfigView } from '$lib/api';

	let { config, reload }: { config: ConfigView | null; reload: () => Promise<void> } = $props();

	/* A route that has been repointed is keyed apart from one that has not, so a
	   reload rebuilds the row and its draft starts again from what the broker
	   actually serves rather than from what was typed before. */
	const key = (name: string, stamp: string) => `${name}:${stamp}`;
	const stamp = $derived(
		(config?.routes ?? []).map((r) => `${r.provider}/${r.model ?? ''}`).join('|')
	);
</script>

<Panel
	title="Routage"
	note="un changement prend effet à l'appel suivant. Le TOML n'est jamais réécrit, donc un redémarrage revient au fichier"
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
				<Table.Head class="w-[16rem]">le modèle demandé</Table.Head>
				<Table.Head class="w-[15rem]">part chez</Table.Head>
				<Table.Head>sous le nom</Table.Head>
				<Table.Head class="w-[10rem] text-right">tarif / Mtok</Table.Head>
				<Table.Head class="w-[9rem]"></Table.Head>
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
