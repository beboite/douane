<script lang="ts">
	import { toast } from 'svelte-sonner';
	import { Button } from '$lib/components/ui/button';
	import { Input } from '$lib/components/ui/input';
	import { Label } from '$lib/components/ui/label';
	import { Switch } from '$lib/components/ui/switch';
	import * as Dialog from '$lib/components/ui/dialog';
	import Boxes from '@lucide/svelte/icons/boxes';
	import Pencil from '@lucide/svelte/icons/pencil';
	import Plus from '@lucide/svelte/icons/plus';
	import RefreshCw from '@lucide/svelte/icons/refresh-cw';
	import Search from '@lucide/svelte/icons/search';
	import Trash2 from '@lucide/svelte/icons/trash-2';
	import TriangleAlert from '@lucide/svelte/icons/triangle-alert';
	import Empty from './empty.svelte';
	import Panel from './panel.svelte';
	import TrustBadge from './trust-badge.svelte';
	import { api, type CatalogProvider, type CatalogView, type ModelEntry } from '$lib/api';
	import { session } from '$lib/session.svelte';

	let { catalog, reload }: { catalog: CatalogView | null; reload: () => Promise<void> } = $props();

	/** The search that runs over every provider at once. */
	let query = $state('');
	/** One search box per provider, because a reseller with five hundred ids is
	    the only place this screen is ever hard to use. */
	let narrow = $state<Record<string, string>>({});
	/** Providers whose full list is open, and those showing what they cannot
	    serve on this surface. */
	let opened = $state(new Set<string>());
	let others = $state(new Set<string>());
	let busy = $state(new Set<string>());
	let refreshing = $state<string | null>(null);

	/** How many rows a provider shows before it asks to be opened. */
	const PREVIEW = 8;

	const providers = $derived(catalog?.providers ?? []);
	/** Everything switched on that serves nothing, minus the case that is not a
	    problem: the file serving the same model under the same name. */
	const problems = $derived(
		Object.entries(catalog?.collisions ?? {}).filter(([, c]) => c.kind !== 'already_in_file')
	);

	/** Two providers on one host are one subscription sold on two surfaces. */
	function siblings(p: CatalogProvider): CatalogProvider[] {
		return providers.filter((o) => o.name !== p.name && o.host === p.host);
	}

	function all(provider: string): ModelEntry[] {
		return (catalog?.models ?? []).filter((m) => m.provider === provider);
	}

	/** What this provider cannot serve at all: the ids it sells on its other
	    surface. Kept out of the way rather than deleted, because "openlux has
	    grok, just not here" is the answer people are looking for. */
	function elsewhere(provider: string): ModelEntry[] {
		return all(provider).filter((m) => m.blocked?.kind === 'wrong_dialect');
	}

	function rowsFor(provider: string): ModelEntry[] {
		const q = `${query.trim()} ${narrow[provider]?.trim() ?? ''}`.trim().toLowerCase();
		let rows = all(provider);
		if (!others.has(provider)) rows = rows.filter((m) => m.blocked?.kind !== 'wrong_dialect');
		if (q) {
			const words = q.split(/\s+/);
			return rows.filter((m) => {
				const hay = `${m.route} ${m.upstream_id} ${m.label ?? ''}`.toLowerCase();
				return words.every((w) => hay.includes(w));
			});
		}
		/* Served first: on a provider with three hundred rows, the handful that
		   actually answer calls is the only part worth seeing by default. */
		return [...rows.filter((m) => m.enabled), ...rows.filter((m) => !m.enabled)];
	}

	function searching(provider: string): boolean {
		return Boolean(query.trim() || narrow[provider]?.trim());
	}

	function shown(provider: string): ModelEntry[] {
		const rows = rowsFor(provider);
		if (searching(provider) || opened.has(provider)) return rows;
		return rows.slice(0, PREVIEW);
	}

	function toggle(set: Set<string>, key: string): Set<string> {
		const next = new Set(set);
		if (next.has(key)) next.delete(key);
		else next.add(key);
		return next;
	}

	function mark(route: string, on: boolean) {
		const next = new Set(busy);
		if (on) next.add(route);
		else next.delete(route);
		busy = next;
	}

	/** What a row's refusal means, in the language of this screen. */
	function why(m: ModelEntry): string | null {
		switch (m.blocked?.kind) {
			case 'wrong_dialect':
				return m.endpoints.length
					? `répond en ${m.endpoints.join(', ')}, pas ici`
					: 'répond sur une autre surface';
			case 'conflict':
				return `douane.toml donne déjà ce nom à ${m.blocked.other}`;
			case 'no_provider':
				return 'ce fournisseur a quitté douane.toml';
			case 'already_in_file':
				return m.blocked.other && m.blocked.other !== m.route
					? `déjà servi sous le nom ${m.blocked.other}`
					: 'déjà servi par douane.toml';
			default:
				return null;
		}
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
			const { changed, skipped } = await api.setProviderModelsEnabled(
				session.token,
				provider,
				enabled
			);
			await reload();
			if (changed === 0) {
				toast.success('rien à changer');
			} else {
				const head = `${changed} modèle${changed > 1 ? 's' : ''} ${enabled ? 'servi' : 'coupé'}${changed > 1 ? 's' : ''}`;
				toast.success(
					enabled && skipped
						? `${head}, ${skipped} laissé${skipped > 1 ? 's' : ''} de côté : ce fournisseur ne les sert pas ici`
						: head
				);
			}
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

	/* ------------------------------------------- adding and correcting rows -- */

	let adding = $state<string | null>(null);
	let editing = $state<ModelEntry | null>(null);
	let form = $state({ upstream_id: '', label: '', route: '', context_length: '', enabled: true });
	let saving = $state(false);

	function openAdd(provider: string) {
		form = { upstream_id: '', label: '', route: '', context_length: '', enabled: true };
		editing = null;
		adding = provider;
	}

	function openEdit(m: ModelEntry) {
		form = {
			upstream_id: m.upstream_id,
			label: m.label ?? '',
			route: m.route,
			context_length: m.context_length ? String(m.context_length) : '',
			enabled: m.enabled
		};
		adding = null;
		editing = m;
	}

	async function save() {
		saving = true;
		try {
			if (editing) {
				await api.editModel(session.token, editing.route, {
					label: form.label.trim(),
					context_length: form.context_length ? Number(form.context_length) : 0,
					route: form.route.trim() || editing.route
				});
				await reload();
				toast.success(`${form.route.trim() || editing.route} corrigé`);
				editing = null;
			} else if (adding && form.upstream_id.trim()) {
				const { route } = await api.putModel(session.token, {
					provider: adding,
					upstream_id: form.upstream_id.trim(),
					route: form.route.trim() || undefined,
					label: form.label.trim() || null,
					context_length: form.context_length ? Number(form.context_length) : null,
					enabled: form.enabled
				});
				await reload();
				toast.success(`${route} ajouté`);
				adding = null;
			}
		} catch (e) {
			toast.error(e instanceof Error ? e.message : 'l’enregistrement a échoué');
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
				placeholder="Chercher un modèle chez tous les fournisseurs"
				class="h-10 pl-9"
				aria-label="Chercher un modèle chez tous les fournisseurs"
			/>
		</div>
		<Button size="lg" onclick={() => refresh()} disabled={refreshing !== null}>
			<RefreshCw class="size-4 {refreshing === '*' ? 'animate-spin' : ''}" />
			Relire les catalogues
		</Button>
	</div>

	{#if problems.length}
		<div
			class="border-trust-mid/40 bg-trust-mid/10 text-trust-mid flex items-start gap-3 rounded-xl border p-4"
		>
			<TriangleAlert class="mt-0.5 size-4 shrink-0" />
			<div class="min-w-0 text-sm">
				<p class="font-semibold">
					{problems.length} modèle{problems.length > 1 ? 's' : ''} activé{problems.length > 1
						? 's'
						: ''} ne ser{problems.length > 1 ? 'vent' : 't'} rien
				</p>
				<ul class="text-foreground/80 mt-1 space-y-0.5 text-[0.8rem]">
					{#each problems as [route, c] (route)}
						<li>
							<span class="font-semibold">{route}</span> :
							{#if c.kind === 'wrong_dialect'}
								ce fournisseur vend ce modèle sur son autre surface
							{:else if c.kind === 'conflict'}
								douane.toml donne déjà ce nom à {c.other}
							{:else}
								son fournisseur a quitté douane.toml
							{/if}
						</li>
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
			{@const mine = all(p.name)}
			{@const served = mine.filter((m) => m.enabled && !m.blocked).length}
			{@const reachable = mine.filter((m) => m.blocked?.kind !== 'wrong_dialect').length}
			{@const elsewhere_count = elsewhere(p.name).length}
			{@const twin = siblings(p)}
			<Panel
				title={p.name}
				note={mine.length === 0
					? p.catalog
						? 'aucun modèle connu, relisez le catalogue'
						: 'ce fournisseur ne publie pas de liste, ajoutez ses modèles à la main'
					: `${served} servi${served > 1 ? 's' : ''} sur ${reachable} joignable${reachable > 1 ? 's' : ''}`}
			>
				{#snippet actions()}
					<div class="flex flex-wrap items-center gap-2">
						<TrustBadge trust={p.trust} />
						{#if reachable > 0}
							<Button
								variant="outline"
								size="sm"
								disabled={busy.has(p.name)}
								onclick={() => setAll(p.name, served < reachable)}
							>
								{served < reachable ? 'Tout servir' : 'Tout couper'}
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

				{#if twin.length}
					<p class="text-muted-foreground -mt-1 mb-3 text-xs">
						Même abonnement que {twin.map((t) => t.name).join(', ')}, autre dialecte : cette
						surface parle {p.wire === 'openai' ? 'OpenAI' : 'Anthropic'}, et chaque modèle ne
						répond que sur la sienne.
					</p>
				{/if}

				{#if mine.length > PREVIEW}
					<div class="relative mb-3">
						<Search
							class="text-muted-foreground pointer-events-none absolute top-1/2 left-3 size-4 -translate-y-1/2"
						/>
						<Input
							value={narrow[p.name] ?? ''}
							oninput={(e) => (narrow = { ...narrow, [p.name]: e.currentTarget.value })}
							placeholder="Filtrer les {mine.length} modèles de {p.name}"
							class="h-9 pl-9"
							aria-label="Filtrer les modèles de {p.name}"
						/>
					</div>
				{/if}

				{#if rows.length === 0}
					<Empty
						icon={Boxes}
						tight
						title={searching(p.name) ? 'aucun modèle ne correspond' : 'rien dans la liste'}
						hint={searching(p.name)
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
									disabled={busy.has(m.route) || m.blocked?.kind === 'wrong_dialect'}
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
										{#if m.blocked}
											<span
												class="pill {m.blocked.kind === 'already_in_file' ? 'pill-quiet' : 'pill-warn'}"
												title={why(m) ?? ''}
											>
												{m.blocked.kind === 'already_in_file' ? 'déjà dans le fichier' : why(m)}
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
								<Button
									variant="ghost"
									size="icon"
									class="text-muted-foreground hover:text-foreground size-8 shrink-0"
									title="corriger {m.route}"
									disabled={busy.has(m.route)}
									onclick={() => openEdit(m)}
								>
									<Pencil class="size-4" />
								</Button>
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

					{#if !searching(p.name) && rows.length > PREVIEW}
						<Button
							variant="ghost"
							size="sm"
							class="text-muted-foreground mt-2 w-full"
							onclick={() => (opened = toggle(opened, p.name))}
						>
							{opened.has(p.name)
								? 'Replier'
								: `Voir les ${rows.length - PREVIEW} autres modèles`}
						</Button>
					{/if}
				{/if}

				{#if elsewhere_count}
					<Button
						variant="ghost"
						size="sm"
						class="text-muted-foreground mt-1 w-full"
						onclick={() => (others = toggle(others, p.name))}
					>
						{others.has(p.name)
							? 'Masquer les modèles de l’autre surface'
							: `${elsewhere_count} modèle${elsewhere_count > 1 ? 's' : ''} vendu${elsewhere_count > 1 ? 's' : ''} sur l’autre surface`}
					</Button>
				{/if}
			</Panel>
		{/each}
	{/if}
</div>

<Dialog.Root
	open={adding !== null || editing !== null}
	onOpenChange={(v) => {
		if (!v) {
			adding = null;
			editing = null;
		}
	}}
>
	<Dialog.Content class="sm:max-w-lg">
		<Dialog.Header>
			<Dialog.Title>
				{editing ? `Corriger ${editing.route}` : `Ajouter un modèle sur ${adding}`}
			</Dialog.Title>
			<Dialog.Description>
				{#if editing}
					Le nom affiché et la fenêtre de contexte voyagent jusqu’aux agents, qui n’ont donc plus
					à en garder une copie. Une relecture du catalogue ne les écrase pas.
				{:else}
					Pour un fournisseur qui ne publie pas de liste, ou un modèle qu’il vend sans
					l’annoncer. L’identifiant doit être écrit exactement comme le fournisseur l’attend.
				{/if}
			</Dialog.Description>
		</Dialog.Header>

		<div class="space-y-4 py-2">
			<div class="space-y-2">
				<Label for="upstream">Identifiant chez le fournisseur</Label>
				<Input
					id="upstream"
					bind:value={form.upstream_id}
					placeholder="grok-4.6"
					disabled={editing !== null}
				/>
			</div>
			<div class="grid gap-4 sm:grid-cols-2">
				<div class="space-y-2">
					<Label for="label">Nom affiché</Label>
					<Input id="label" bind:value={form.label} placeholder="Grok 4.6" />
				</div>
				<div class="space-y-2">
					<Label for="ctx">Fenêtre de contexte</Label>
					<Input
						id="ctx"
						bind:value={form.context_length}
						inputmode="numeric"
						placeholder="500000"
					/>
				</div>
			</div>
			<div class="space-y-2">
				<Label for="route">Nom de route</Label>
				<Input
					id="route"
					bind:value={form.route}
					placeholder={adding
						? `${providers.find((p) => p.name === adding)?.prefix ?? ''}grok-4.6`
						: ''}
				/>
				<p class="text-muted-foreground text-xs">
					Ce que les agents demanderont. Laissez vide pour le nom habituel : le préfixe du
					fournisseur suivi de l’identifiant.
				</p>
			</div>
			{#if !editing}
				<label class="flex items-center gap-3 text-sm">
					<Switch bind:checked={form.enabled} aria-label="servir tout de suite" />
					Servir tout de suite
				</label>
			{/if}
		</div>

		<Dialog.Footer>
			<Button
				variant="outline"
				onclick={() => {
					adding = null;
					editing = null;
				}}
			>
				Annuler
			</Button>
			<Button onclick={save} disabled={saving || (!editing && !form.upstream_id.trim())}>
				{editing ? 'Enregistrer' : 'Ajouter'}
			</Button>
		</Dialog.Footer>
	</Dialog.Content>
</Dialog.Root>
