<script lang="ts">
	import { Button } from '$lib/components/ui/button';
	import { Input } from '$lib/components/ui/input';
	import * as Select from '$lib/components/ui/select';
	import * as Table from '$lib/components/ui/table';
	import { untrack } from 'svelte';
	import { toast } from 'svelte-sonner';
	import RotateCcw from '@lucide/svelte/icons/rotate-ccw';
	import Check from '@lucide/svelte/icons/check';
	import TrustBadge from './trust-badge.svelte';
	import { api, type ProviderView, type RouteView } from '$lib/api';
	import { session } from '$lib/session.svelte';

	let {
		route,
		providers,
		reload
	}: {
		route: RouteView;
		providers: ProviderView[];
		reload: () => Promise<void>;
	} = $props();

	/* The row owns what it is being edited to, and takes the starting point once
	   on purpose: the parent keys these rows on what the broker serves, so a
	   reload rebuilds the row rather than overwriting a half-typed edit. */
	let provider = $state(untrack(() => route.provider));
	let model = $state(untrack(() => route.model ?? ''));
	let busy = $state(false);

	const dirty = $derived(provider !== route.provider || model !== (route.model ?? ''));
	const trustOf = (name: string) => providers.find((p) => p.name === name)?.trust ?? null;

	async function apply() {
		busy = true;
		try {
			await api.setRoute(session.token, route.name, {
				provider,
				model: model.trim() === '' ? null : model.trim()
			});
			await reload();
			toast.success(`${route.name} part maintenant chez ${provider}`);
		} catch (e) {
			toast.error(`${route.name} n'a pas bougé`, { description: (e as Error).message });
		} finally {
			busy = false;
		}
	}

	async function revert() {
		busy = true;
		try {
			await api.clearRoute(session.token, route.name);
			await reload();
			toast.success(`${route.name} suit de nouveau le fichier`);
		} catch (e) {
			toast.error(`${route.name} n'a pas bougé`, { description: (e as Error).message });
		} finally {
			busy = false;
		}
	}
</script>

<Table.Row class={route.overridden ? 'bg-accent/40' : ''}>
	<Table.Cell>
		<div class="font-medium">{route.name}</div>
		{#if route.overridden}
			<div class="text-muted-foreground mt-0.5 text-xs">
				le fichier dit {route.base_provider}{route.base_model ? ` · ${route.base_model}` : ''}
			</div>
		{/if}
	</Table.Cell>

	<Table.Cell>
		<Select.Root type="single" bind:value={provider}>
			<Select.Trigger class="w-full">
				<span class="flex items-center gap-2 truncate">
					{provider}
					<TrustBadge trust={trustOf(provider)} />
				</span>
			</Select.Trigger>
			<Select.Content>
				{#each providers as p (p.name)}
					<Select.Item value={p.name} label={p.name}>
						<span class="flex w-full items-center justify-between gap-3">
							{p.name}
							<TrustBadge trust={p.trust} />
						</span>
					</Select.Item>
				{/each}
			</Select.Content>
		</Select.Root>
	</Table.Cell>

	<Table.Cell>
		<Input
			bind:value={model}
			placeholder="le nom demandé, tel quel"
			class="max-w-[22rem] font-mono text-xs"
		/>
	</Table.Cell>

	<Table.Cell>
		<div class="flex justify-end gap-1.5">
			{#if dirty}
				<Button size="sm" disabled={busy} onclick={apply}>
					<Check class="size-3.5" />
					appliquer
				</Button>
			{:else if route.overridden}
				<Button
					size="sm"
					variant="ghost"
					disabled={busy}
					onclick={revert}
					title="oublier la dérogation et suivre le fichier"
				>
					<RotateCcw class="size-3.5" />
					fichier
				</Button>
			{/if}
		</div>
	</Table.Cell>
</Table.Row>
