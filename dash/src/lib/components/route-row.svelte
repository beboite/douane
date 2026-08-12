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

	/* A route born on the models screen has no line in the file to derogate from:
	   its provider and its upstream name are the catalogue entry itself, so
	   editing it here would fight whatever that screen writes next. It is shown
	   because it is served, and it is read-only because it is edited elsewhere. */
	const readonly = $derived(route.from_catalog);

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
	<Table.Cell class="px-5 py-3">
		<div class="flex items-center gap-2">
			<span class="font-medium">{route.name}</span>
			{#if readonly}
				<span class="pill pill-quiet" title="activé depuis l’écran Modèles, pas écrit dans le fichier">
					catalogue
				</span>
			{/if}
		</div>
		{#if route.overridden}
			<div class="text-muted-foreground mt-1 text-xs">
				le fichier dit {route.base_provider}{route.base_model ? ` · ${route.base_model}` : ''}
			</div>
		{/if}
	</Table.Cell>

	<Table.Cell class="px-3 py-3">
		{#if readonly}
			<span class="flex items-center gap-2 text-sm">
				{route.provider}
				<TrustBadge trust={trustOf(route.provider)} />
			</span>
		{:else}
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
		{/if}
	</Table.Cell>

	<Table.Cell class="px-3 py-3">
		{#if readonly}
			<span class="text-muted-foreground text-sm">{route.model ?? 'le nom, tel quel'}</span>
		{:else}
			<Input bind:value={model} placeholder="le nom demandé, tel quel" class="max-w-[22rem]" />
		{/if}
	</Table.Cell>

	<Table.Cell class="px-5 py-3">
		<div class="flex justify-end gap-2">
			{#if readonly}
				<span class="text-muted-foreground text-xs">se règle dans Modèles</span>
			{:else if dirty}
				<Button size="sm" disabled={busy} onclick={apply}>
					<Check class="size-4" />
					appliquer
				</Button>
			{:else if route.overridden}
				<Button
					size="sm"
					variant="outline"
					disabled={busy}
					onclick={revert}
					title="oublier la dérogation et suivre de nouveau le fichier"
				>
					<RotateCcw class="size-4" />
					revenir au fichier
				</Button>
			{/if}
		</div>
	</Table.Cell>
</Table.Row>
