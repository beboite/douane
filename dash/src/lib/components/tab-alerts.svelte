<script lang="ts">
	import * as Table from '$lib/components/ui/table';
	import ShieldAlert from '@lucide/svelte/icons/shield-alert';
	import ShieldCheck from '@lucide/svelte/icons/shield-check';
	import Empty from './empty.svelte';
	import Panel from './panel.svelte';
	import TrustBadge from './trust-badge.svelte';
	import type { Alert } from '$lib/api';
	import { when } from '$lib/format';

	let { alerts }: { alerts: Alert[] } = $props();

	const tone: Record<string, string> = {
		block: 'text-destructive',
		warn: 'text-trust-mid',
		allow: 'text-muted-foreground'
	};

	const verdict: Record<string, string> = {
		block: 'refusé',
		warn: 'laissé passer, noté',
		allow: 'autorisé'
	};

	/* Two families under one roof. `vault` and `shape` are the scrubber saying a
	   body carried something it recognised; `trust` and `route` are the router
	   saying no route this client may use could serve the request. Both are
	   douane refusing, both belong here, and reading `trust` as a leak would be
	   a scare over a routing rule. */
	const nature: Record<string, string> = {
		vault: 'valeur du coffre',
		shape: 'forme de clé',
		trust: 'plafond de confiance',
		route: 'route non autorisée'
	};

	const leak = (kind: string) => kind === 'vault' || kind === 'shape';
</script>

<Panel
	title="Ce que douane a refusé"
	note="une fuite reconnue dans un corps partant, ou une route qu’aucun plafond du client ne laissait prendre. L’alerte nomme la référence ou le motif, jamais ce qui a été trouvé : une table pleine d’identifiants fuités serait pire que la fuite qu’elle signale"
	flush
>
	{#if alerts.length === 0}
		<Empty
			icon={ShieldCheck}
			title="rien n’a été refusé"
			hint="aucun corps partant ne portait de valeur du coffre, et chaque appel a trouvé une route que son client avait le droit de prendre"
		/>
	{:else}
		<Table.Root>
			<Table.Header>
				<Table.Row>
					<Table.Head class="w-[7rem]">quand</Table.Head>
					<Table.Head class="w-[9rem]">client</Table.Head>
					<Table.Head class="w-[16rem]">allait chez</Table.Head>
					<Table.Head class="w-[18rem]">ce qui a été refusé</Table.Head>
					<Table.Head>nature</Table.Head>
					<Table.Head class="w-[10rem] text-right">verdict</Table.Head>
				</Table.Row>
			</Table.Header>
			<Table.Body>
				{#each alerts as a (a.id)}
					<!-- Only a leak gets the red wash. A routing refusal is a rule doing
					     its job, and tinting the whole table alarms over config. -->
					<Table.Row class={a.action === 'block' && leak(a.kind) ? 'bg-destructive/5' : ''}>
						<Table.Cell class="text-muted-foreground text-xs" title={a.at}>{when(a.at)}</Table.Cell>
						<Table.Cell class="text-sm">{a.client}</Table.Cell>
						<Table.Cell>
							<span class="flex items-center gap-2 text-sm">
								{a.provider}
								<TrustBadge trust={a.trust} />
							</span>
						</Table.Cell>
						<Table.Cell class="font-mono text-xs">{a.label}</Table.Cell>
						<Table.Cell class="text-xs">
							<span class={leak(a.kind) ? 'text-trust-low' : 'text-muted-foreground'}>
								{nature[a.kind] ?? a.kind}
							</span>
						</Table.Cell>
						<Table.Cell class="text-right">
							<span
								class="inline-flex items-center gap-1.5 text-xs font-medium {tone[a.action] ??
									''}"
							>
								{#if a.action === 'block'}<ShieldAlert class="size-3.5" />{/if}
								{verdict[a.action] ?? a.action}
							</span>
						</Table.Cell>
					</Table.Row>
				{/each}
			</Table.Body>
		</Table.Root>
	{/if}
</Panel>
