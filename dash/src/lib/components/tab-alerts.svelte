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
</script>

<Panel
	title="Ce que le scrubber a vu partir"
	note="l’alerte nomme la référence ou le motif, jamais ce qui a été trouvé : une table pleine d’identifiants fuités serait pire que la fuite qu’elle signale"
	flush
>
	{#if alerts.length === 0}
		<Empty
			icon={ShieldCheck}
			title="rien n’a essayé de sortir"
			hint="chaque corps partant est comparé aux valeurs du coffre avant d’être envoyé, et aucun n’en portait"
		/>
	{:else}
		<Table.Root>
			<Table.Header>
				<Table.Row>
					<Table.Head class="w-[7rem]">quand</Table.Head>
					<Table.Head class="w-[9rem]">client</Table.Head>
					<Table.Head class="w-[16rem]">allait chez</Table.Head>
					<Table.Head class="w-[18rem]">quoi</Table.Head>
					<Table.Head>nature</Table.Head>
					<Table.Head class="w-[10rem] text-right">verdict</Table.Head>
				</Table.Row>
			</Table.Header>
			<Table.Body>
				{#each alerts as a (a.id)}
					<Table.Row class={a.action === 'block' ? 'bg-destructive/5' : ''}>
						<Table.Cell class="text-muted-foreground text-xs" title={a.at}>{when(a.at)}</Table.Cell>
						<Table.Cell class="text-sm">{a.client}</Table.Cell>
						<Table.Cell>
							<span class="flex items-center gap-2 text-sm">
								{a.provider}
								<TrustBadge trust={a.trust} />
							</span>
						</Table.Cell>
						<Table.Cell class="font-mono text-xs">{a.label}</Table.Cell>
						<Table.Cell class="text-muted-foreground text-xs">{a.kind}</Table.Cell>
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
