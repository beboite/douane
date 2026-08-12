<script lang="ts">
	import { toast } from 'svelte-sonner';
	import { Button } from '$lib/components/ui/button';
	import { Input } from '$lib/components/ui/input';
	import { Label } from '$lib/components/ui/label';
	import { Switch } from '$lib/components/ui/switch';
	import * as Dialog from '$lib/components/ui/dialog';
	import Boxes from '@lucide/svelte/icons/boxes';
	import Plus from '@lucide/svelte/icons/plus';
	import RefreshCw from '@lucide/svelte/icons/refresh-cw';
	import Search from '@lucide/svelte/icons/search';
	import Trash2 from '@lucide/svelte/icons/trash-2';
	import TriangleAlert from '@lucide/svelte/icons/triangle-alert';
	import Empty from './empty.svelte';
	import Panel from './panel.svelte';
	import TrustBadge from './trust-badge.svelte';
	import { api, type CatalogView, type ModelEntry } from '$lib/api';
	import { session } from '$lib/session.svelte';

	let { catalog, reload }: { catalog: CatalogView | null; reload: () => Promise<void> } = $props();

	let query = $state('');
	/* Providers whose full list is open. A reseller on an auto group sells
	   hundreds of ids, so a screen that printed all of them would be unusable on
	   the one provider that needs it most. */
	let opened = $state(new Set<string>());
	let busy = $state(new Set<string>());
	let refreshing = $state<string | null>(null);

	/** How many rows a provider shows before it asks to be opened. */
	const PREVIEW = 8;

	const providers = $derived(catalog?.providers ?? []);
	const collisions = $derived(Object.entries(catalog?.collisions ?? {}));

	function rowsFor(provider: string): ModelEntry[] {
		const all = (catalog?.models ?? []).filter((m) => m.provider === provider);
		const q = query.trim().toLowerCase();
		if (q) {
			return all.filter(
				(m) =>
					m.route.toLowerCase().includes(q) ||
					m.upstream_id.toLowerCase().includes(q) ||
					(m.label ?? '').toLowerCase().includes(q)
			);
		}
		/* Served first: on a provider with three hundred rows, the handful that
		   actually answer calls is the only part worth seeing by default. */
		const served = all.filter((m) => m.enabled);
		const rest = all.filter((m) => !m.enabled);
		return [...served, ...rest];
	}

	function shown(provider: string): ModelEntry[] {
		const rows = rowsFor(provider);
		if (query.trim() || opened.has(provider)) return rows;
		return rows.slice(0, PREVIEW);
	}

	function toggleOpen(provider: string) {
		const next = new Set(opened);
		if (next.has(provider)) next.delete(provider);
		else next.add(provider);
		opened = next;
	}

	function mark(route: string, on: boolean) {
		const next = new Set(busy);
		if (on) next.add(route);
		else next.delete(route);
		busy = next;
	}

	async function setEnabled(m: ModelEntry, enabled: boolean) {
		mark(m.route, true);
		try {
			await api.setModelEnabled(session.token, m.route, enabled);
			await reload();
			toast.success(enabled ? `${m.route} est servi` : `${m.route} ne répond plus`);
		} catch (e) {
			toast.error(e instanceof Error ? e.message : 'le changement a échoué');
		} finally {
			mark(m.route, false);
		}
	}

	async function setAll(provider: string, enabled: boolean) {
		mark(provider, true);
		try {
			const { changed } = await api.setProviderModelsEnabled(session.token, provider, enabled);
			await reload();
			toast.success(
				changed === 0
					? 'rien à changer'
					: `${changed} modèle${changed > 1 ? 's' : ''} ${enabled ? 'servis' : 'coupés'}`
			);
		} catch (e) {
			toast.error(e instanceof Error ? e.message : 'le changement a échoué');
		} finally {
			mark(provider, false);
		}
	}

	async function refresh(provider?: string) {
		refreshing = provider ?? '*';
		try {
			const { refreshed } = await api.refreshModels(session.token, provider);
			await reload();
			const failed = refreshed.filter((r) => !r.ok);
			const added = refreshed.reduce((n, r) => n + r.added, 0);
			const gone = refreshed.reduce((n, r) => n + r.missing, 0);
			const parts = [];
			if (added) parts.push(`${added} nouveau${added > 1 ? 'x' : ''}`);
			if (gone) parts.push(`${gone} disparu${gone > 1 ? 's' : ''}`);
			if (failed.length) {
				toast.error(
					`${failed.length} fournisseur${failed.length > 1 ? 's' : ''} n'a pas répondu : ${failed
						.map((r) => r.provider)
						.join(', ')}`
				);
			} else {
				toast.success(parts.length ? parts.join(', ') : 'aucun changement');
			}
		} catch (e) {
			toast.error(e instanceof Error ? e.message : 'la liste n’a pas pu être relue');
		} finally {
			refreshing = null;
		}
	}

	async function remove(m: ModelEntry) {
		mark(m.route, true);
		try {
			await api.deleteModel(session.token, m.route);
			await reload();
			toast.success(`${m.route} retiré de la liste`);
		} catch (e) {
			toast.error(e instanceof Error ? e.message : 'la suppression a échoué');
		} finally {
			mark(m.route, false);
		}
	}

	/* ------------------------------------------------------- adding by hand -- */

	let adding = $state<string | null>(null);
	let form = $state({ upstream_id: '', label: '', route: '', context_length: '', enabled: true });
	let saving = $state(false);

	function openAdd(provider: string) {
		form = { upstream_id: '', label: '', route: '', context_length: '', enabled: true };
		adding = provider;
	}

	async function save() {
		if (!adding || !form.upstream_id.trim()) return;
		saving = true;
		try {
			const { route } = await api.putModel(session.token, {
				provider: adding,
				upstream_id: form.upstream_id.trim(),
				route: form.route.trim() || undefined,
				label: form.label.trim() || null,
				context_length: form.context_length ? Number(form.context_length) : null,
				enabled: form.enabled
			});
			await reload();
			adding = null;
			toast.success(`${route} ajouté`);
		} catch (e) {
			toast.error(e instanceof Error ? e.message : 'l’ajout a échoué');
		} finally {
			saving = false;
		}
	}

	const compact = new Intl.NumberFormat('fr-FR', { notation: 'compact' });

	function windowLabel(n: number | null): string | null {
		return n ? `${compact.format(n)} jetons` : null;
	}
</script>

<div class="space-y-5">
	<!-- The one control that fills this screen when it is empty, and the one
	     people will look for afterwards, so it sits above everything. -->
	<div class="flex flex-wrap items-center gap-3">
		<div class="relative min-w-0 flex-1">
			<Search
				class="text-muted-foreground pointer-events-none absolute top-1/2 left-3 size-4 -translate-y-1/2"
			/>
			<Input
				bind:value={query}
				placeholder="Chercher un modèle"
				class="h-10 pl-9"
				aria-label="Chercher un modèle"
			/>
		</div>
		<Button size="lg" onclick={() => refresh()} disabled={refreshing !== null}>
			<RefreshCw class="size-4 {refreshing === '*' ? 'animate-spin' : ''}" />
			Relire les catalogues
		</Button>
	</div>

	{#if collisions.length}
		<div
			class="border-trust-mid/40 bg-trust-mid/10 text-trust-mid flex items-start gap-3 rounded-xl border p-4"
		>
			<TriangleAlert class="mt-0.5 size-4 shrink-0" />
			<div class="min-w-0 text-sm">
				<p class="font-semibold">
					{collisions.length} modèle{collisions.length > 1 ? 's' : ''} activé{collisions.length > 1
						? 's'
						: ''} ne ser{collisions.length > 1 ? 'vent' : 't'} rien
				</p>
				<ul class="text-foreground/80 mt-1 space-y-0.5 text-[0.8rem]">
					{#each collisions as [route, why] (route)}
						<li><span class="font-semibold">{route}</span> : {why}</li>
					{/each}
				</ul>
			</div>
		</div>
	{/if}

	{#if !catalog}
		<Panel title="Modèles">
			<Empty icon={Boxes} title="chargement" />
		</Panel>
	{:else if providers.length === 0}
		<Panel title="Modèles">
			<Empty
				icon={Boxes}
				title="aucun fournisseur configuré"
				hint="les fournisseurs se déclarent dans douane.toml ; leurs modèles se découvrent ici"
			/>
		</Panel>
	{:else}
		{#each providers as p (p.name)}
			{@const rows = rowsFor(p.name)}
			{@const visible = shown(p.name)}
			{@const served = (catalog.models ?? []).filter((m) => m.provider === p.name && m.enabled)
				.length}
			{@const total = (catalog.models ?? []).filter((m) => m.provider === p.name).length}
			<Panel
				title={p.name}
				note={total === 0
					? p.catalog
						? 'aucun modèle connu, relisez le catalogue'
						: 'ce fournisseur ne publie pas de liste, ajoutez ses modèles à la main'
					: `${served} servi${served > 1 ? 's' : ''} sur ${total} connu${total > 1 ? 's' : ''}`}
			>
				{#snippet actions()}
					<div class="flex flex-wrap items-center gap-2">
						<TrustBadge trust={p.trust} />
						{#if total > 0}
							<Button
								variant="outline"
								size="sm"
								disabled={busy.has(p.name)}
								onclick={() => setAll(p.name, served < total)}
							>
								{served < total ? 'Tout servir' : 'Tout couper'}
							</Button>
						{/if}
						{#if p.catalog}
							<Button
								variant="outline"
								size="sm"
								disabled={refreshing !== null}
								onclick={() => refresh(p.name)}
							>
								<RefreshCw class="size-3.5 {refreshing === p.name ? 'animate-spin' : ''}" />
								Relire
							</Button>
						{/if}
						<Button size="sm" onclick={() => openAdd(p.name)}>
							<Plus class="size-3.5" />
							Ajouter
						</Button>
					</div>
				{/snippet}

				{#if rows.length === 0}
					<Empty
						icon={Boxes}
						tight
						title={query.trim() ? 'aucun modèle ne correspond' : 'rien dans la liste'}
						hint={query.trim()
							? 'la recherche porte sur le nom de route, l’identifiant du fournisseur et le libellé'
							: p.catalog
								? 'relisez le catalogue pour demander à ce fournisseur ce qu’il vend'
								: 'ce fournisseur ne publie rien : ajoutez un modèle et donnez son identifiant exact'}
					/>
				{:else}
					<ul class="divide-border -my-1 divide-y">
						{#each visible as m (m.route)}
							<li class="flex items-center gap-3 py-3">
								<Switch
									checked={m.enabled}
									disabled={busy.has(m.route)}
									onCheckedChange={(v) => setEnabled(m, v)}
									aria-label="servir {m.route}"
								/>
								<div class="min-w-0 flex-1">
									<div class="flex flex-wrap items-center gap-2">
										<span class="truncate text-sm font-semibold">{m.label ?? m.route}</span>
										{#if m.source === 'manual'}
											<span class="pill pill-quiet">à la main</span>
										{/if}
										{#if !m.present}
											<span
												class="pill pill-warn"
												title="le fournisseur ne le liste plus. Il continue de répondre s’il est servi"
											>
												disparu
											</span>
										{/if}
									</div>
									<p class="text-muted-foreground mt-0.5 truncate text-xs">
										<span class="tabular">{m.route}</span>
										<span class="opacity-50">·</span>
										{m.upstream_id}
										{#if windowLabel(m.context_length)}
											<span class="opacity-50">·</span>
											<span class="tabular">{windowLabel(m.context_length)}</span>
										{/if}
									</p>
								</div>
								{#if m.source === 'manual'}
									<Button
										variant="ghost"
										size="icon"
										class="text-muted-foreground hover:text-destructive size-8 shrink-0"
										title="retirer {m.route} de la liste"
										disabled={busy.has(m.route)}
										onclick={() => remove(m)}
									>
										<Trash2 class="size-4" />
									</Button>
								{/if}
							</li>
						{/each}
					</ul>

					{#if !query.trim() && rows.length > PREVIEW}
						<Button
							variant="ghost"
							size="sm"
							class="text-muted-foreground mt-2 w-full"
							onclick={() => toggleOpen(p.name)}
						>
							{opened.has(p.name)
								? 'Replier'
								: `Voir les ${rows.length - PREVIEW} autres modèles`}
						</Button>
					{/if}
				{/if}
			</Panel>
		{/each}
	{/if}
</div>

<Dialog.Root open={adding !== null} onOpenChange={(v) => (adding = v ? adding : null)}>
	<Dialog.Content class="sm:max-w-lg">
		<Dialog.Header>
			<Dialog.Title>Ajouter un modèle sur {adding}</Dialog.Title>
			<Dialog.Description>
				Pour un fournisseur qui ne publie pas de liste, ou un modèle qu’il vend sans l’annoncer.
				L’identifiant doit être écrit exactement comme le fournisseur l’attend.
			</Dialog.Description>
		</Dialog.Header>

		<div class="space-y-4 py-2">
			<div class="space-y-2">
				<Label for="upstream">Identifiant chez le fournisseur</Label>
				<Input id="upstream" bind:value={form.upstream_id} placeholder="grok-4.6" />
			</div>
			<div class="grid gap-4 sm:grid-cols-2">
				<div class="space-y-2">
					<Label for="label">Nom affiché</Label>
					<Input id="label" bind:value={form.label} placeholder="Grok 4.6" />
				</div>
				<div class="space-y-2">
					<Label for="ctx">Fenêtre de contexte</Label>
					<Input id="ctx" bind:value={form.context_length} inputmode="numeric" placeholder="500000" />
				</div>
			</div>
			<div class="space-y-2">
				<Label for="route">Nom de route</Label>
				<Input
					id="route"
					bind:value={form.route}
					placeholder={adding ? `${providers.find((p) => p.name === adding)?.prefix ?? ''}grok-4.6` : ''}
				/>
				<p class="text-muted-foreground text-xs">
					Ce que les agents demanderont. Laissez vide pour le nom habituel : le préfixe du
					fournisseur suivi de l’identifiant.
				</p>
			</div>
			<label class="flex items-center gap-3 text-sm">
				<Switch bind:checked={form.enabled} aria-label="servir tout de suite" />
				Servir tout de suite
			</label>
		</div>

		<Dialog.Footer>
			<Button variant="outline" onclick={() => (adding = null)}>Annuler</Button>
			<Button onclick={save} disabled={saving || !form.upstream_id.trim()}>Ajouter</Button>
		</Dialog.Footer>
	</Dialog.Content>
</Dialog.Root>
