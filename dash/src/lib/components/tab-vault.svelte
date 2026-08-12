<script lang="ts">
	import * as AlertDialog from '$lib/components/ui/alert-dialog';
	import { Badge } from '$lib/components/ui/badge';
	import { Button, buttonVariants } from '$lib/components/ui/button';
	import { Input } from '$lib/components/ui/input';
	import Copy from '@lucide/svelte/icons/copy';
	import Lock from '@lucide/svelte/icons/lock';
	import Plus from '@lucide/svelte/icons/plus';
	import Search from '@lucide/svelte/icons/search';
	import Trash2 from '@lucide/svelte/icons/trash-2';
	import PenLine from '@lucide/svelte/icons/pen-line';
	import KeyRound from '@lucide/svelte/icons/key-round';
	import { toast } from 'svelte-sonner';
	import Empty from './empty.svelte';
	import Panel from './panel.svelte';
	import SecretDialog from './secret-dialog.svelte';
	import { api, type EntryInfo, type VaultView } from '$lib/api';
	import { ago, bytes, count } from '$lib/format';
	import { session } from '$lib/session.svelte';

	let { vault, reload }: { vault: VaultView | null; reload: () => Promise<void> } = $props();

	let filter = $state('');
	let dialogOpen = $state(false);
	let editing = $state<EntryInfo | null>(null);
	let deleting = $state<EntryInfo | null>(null);
	let busy = $state(false);

	const inUse = $derived(new Set(vault?.in_use ?? []));
	const reference = (e: EntryInfo) => `${e.namespace}.${e.key}`;

	const shown = $derived.by(() => {
		const q = filter.trim().toLowerCase();
		const entries = vault?.entries ?? [];
		return q
			? entries.filter(
					(e) =>
						reference(e).toLowerCase().includes(q) || (e.note ?? '').toLowerCase().includes(q)
				)
			: entries;
	});

	/** Two entries with the same fingerprint hold the same value. Worth saying
	 *  out loud: it is either one credential filed twice under two names, or two
	 *  services sharing one, and both are things to know before rotating either.
	 *  The fingerprint is a hash, so this compares without reading anything. */
	const twins = $derived.by(() => {
		const by = new Map<string, string[]>();
		for (const e of vault?.entries ?? []) {
			by.set(e.fingerprint, [...(by.get(e.fingerprint) ?? []), reference(e)]);
		}
		return by;
	});

	const twinsOf = (e: EntryInfo) => (twins.get(e.fingerprint) ?? []).filter((r) => r !== reference(e));

	/** Grouped by namespace, because that is how they are named and how they are
	 *  looked for: everything proxy holds sits together. */
	const groups = $derived.by(() => {
		const by = new Map<string, EntryInfo[]>();
		for (const e of shown) {
			const list = by.get(e.namespace) ?? [];
			list.push(e);
			by.set(e.namespace, list);
		}
		return [...by.entries()].sort(([a], [b]) => a.localeCompare(b));
	});

	function deposit() {
		editing = null;
		dialogOpen = true;
	}

	function replace(entry: EntryInfo) {
		editing = entry;
		dialogOpen = true;
	}

	/** Only ever called on a placeholder, which is public by construction. No
	 *  surface on this page can reach a value, so none of them can copy one. */
	async function copy(text: string) {
		try {
			await navigator.clipboard.writeText(text);
			toast.success('Repère copié', { description: text });
		} catch {
			toast.error('Le presse-papier a refusé');
		}
	}

	async function confirmDelete() {
		if (!deleting) return;
		busy = true;
		const gone = reference(deleting);
		try {
			const res = await api.deleteSecret(session.token, deleting.namespace, deleting.key);
			deleting = null;
			await reload();
			toast.success(`${gone} n’est plus en coffre`, {
				description: `Le scrubber surveille ${res.watching} valeurs en sortie`
			});
		} catch (e) {
			toast.error(`${gone} est toujours là`, { description: (e as Error).message });
		} finally {
			busy = false;
		}
	}
</script>

<Panel
	title="Coffre"
	note="{count(vault?.entries.length ?? 0)} entrées, {count(
		vault?.watching ?? 0
	)} valeurs surveillées en sortie. Aucune ne se relit, ici comme ailleurs"
>
	{#snippet actions()}
		<div class="flex shrink-0 flex-wrap items-center gap-2">
			<div class="relative">
				<Search
					class="text-muted-foreground pointer-events-none absolute top-1/2 left-2.5 size-3.5 -translate-y-1/2"
				/>
				<Input
					bind:value={filter}
					placeholder="Filtrer"
					class="h-8 w-40 pl-8 sm:w-48"
					autocomplete="off"
					spellcheck="false"
				/>
			</div>
			<!-- The reason this screen exists, so it is filled, named by its verb and
			     the first thing the eye lands on in the header. -->
			<Button size="sm" class="h-8 px-3" onclick={deposit}>
				<Plus class="size-3.5" />
				Déposer une valeur
			</Button>
		</div>
	{/snippet}

	<div class="space-y-5">
		{#if groups.length}
			<!-- The placeholder is the one idea a newcomer has to get, and nothing
			     else on the page teaches it. Said here once, with an example. -->
			<p class="text-muted-foreground rounded-xl border p-4 text-xs leading-relaxed">
				Chaque entrée reçoit un <span class="text-foreground font-semibold">repère</span>, par
				exemple
				<span class="bg-sunken text-foreground rounded-md px-1.5 py-0.5 font-medium">
					__DOUANE_PROXY_PASSWORD__
				</span>. C’est le nom qu’un agent écrit dans une conversation à la place du secret : il ne vaut
				rien pour un fournisseur, douane seule sait à quelle valeur il renvoie.
			</p>
		{/if}

		{#each groups as [namespace, entries] (namespace)}
			<section>
				<div class="mb-2.5 flex items-center gap-2.5">
					<h3 class="text-sm font-semibold">{namespace}</h3>
					<Badge
						variant="secondary"
						class="tabular rounded-full px-2 py-0 text-[0.7rem] font-semibold"
					>
						{entries.length}
					</Badge>
					<div class="bg-border h-px flex-1"></div>
				</div>

				<ul class="divide-border bg-sunken/40 divide-y overflow-hidden rounded-xl border">
					{#each entries as entry (reference(entry))}
						{@const locked = inUse.has(reference(entry))}
						{@const sameAs = twinsOf(entry)}
						<li
							class="hover:bg-card/70 flex flex-wrap items-center gap-x-4 gap-y-2 px-4 py-3 transition-colors"
						>
							<div class="min-w-0 flex-1 basis-[13rem]">
								<div class="flex flex-wrap items-center gap-x-2 gap-y-1">
									<span class="text-sm font-semibold">{entry.key}</span>
									{#if locked}
										<span
											class="pill pill-substitute"
											title="la configuration en cours s’authentifie avec"
										>
											en service
										</span>
									{/if}
									{#if sameAs.length}
										<span class="pill pill-warn" title="même valeur que {sameAs.join(', ')}">
											même valeur que {sameAs.length === 1 ? sameAs[0] : `${sameAs.length} autres`}
										</span>
									{/if}
								</div>
								{#if entry.note}
									<p class="text-muted-foreground mt-1 truncate text-xs">{entry.note}</p>
								{/if}
							</div>

							<button
								class="focusable bg-sunken hover:bg-accent text-muted-foreground hover:text-foreground inline-flex min-w-0 basis-full items-center gap-1.5 rounded-md px-2 py-1 text-xs transition-colors md:flex-1 md:basis-[12rem]"
								onclick={() => copy(entry.placeholder)}
								title="copier le repère, à donner à un agent à la place de la valeur"
							>
								<span class="truncate">{entry.placeholder}</span>
								<Copy class="size-3 shrink-0 opacity-60" />
							</button>

							<div
								class="text-muted-foreground hidden shrink-0 items-center gap-2 text-xs lg:flex"
							>
								<span
									class="bg-sunken tabular rounded-md px-2 py-1"
									title="empreinte de la valeur, jamais la valeur"
								>
									{entry.fingerprint.slice(0, 12)}
								</span>
								<span class="tabular w-[8.5rem] text-right">
									{bytes(entry.bytes)} ·
									<span title={entry.updated_at}>{ago(entry.updated_at)}</span>
								</span>
							</div>

							<div class="ml-auto flex shrink-0 items-center gap-1">
								<Button
									size="icon-sm"
									variant="ghost"
									title="Remplacer la valeur ou la note"
									onclick={() => replace(entry)}
								>
									<PenLine class="size-3.5" />
									<span class="sr-only">Remplacer</span>
								</Button>
								<!-- The broker answers 409 on an entry the running config
								     authenticates with, so the row says why instead of showing a
								     button that could only fail. -->
								{#if locked}
									<span
										class="text-muted-foreground flex size-7 items-center justify-center"
										title="la configuration en cours en dépend, elle ne peut pas être retirée"
									>
										<Lock class="size-3.5" />
									</span>
								{:else}
									<Button
										size="icon-sm"
										variant="ghost"
										class="hover:text-destructive"
										title="Retirer du coffre"
										onclick={() => (deleting = entry)}
									>
										<Trash2 class="size-3.5" />
										<span class="sr-only">Retirer</span>
									</Button>
								{/if}
							</div>
						</li>
					{/each}
				</ul>
			</section>
		{:else}
			<Empty
				icon={KeyRound}
				title={filter ? 'Rien qui corresponde' : 'Le coffre est vide'}
				hint={filter
					? 'Le filtre porte sur le nom et sur la note, jamais sur la valeur'
					: 'Déposez une première valeur : elle reçoit un repère à donner aux agents, et le scrubber la reconnaît en sortie dans la seconde'}
			/>
		{/each}
	</div>
</Panel>

<SecretDialog bind:open={dialogOpen} existing={editing} {reload} />

<AlertDialog.Root
	open={deleting !== null}
	onOpenChange={(o) => {
		if (!o) deleting = null;
	}}
>
	<AlertDialog.Content>
		<AlertDialog.Header>
			<AlertDialog.Title>Retirer {deleting ? reference(deleting) : ''} ?</AlertDialog.Title>
			<AlertDialog.Description>
				La valeur n’est nulle part ailleurs et ne se relit pas, donc elle ne revient pas. Ce qui s’en
				sert cessera de fonctionner à l’appel suivant.
			</AlertDialog.Description>
		</AlertDialog.Header>
		<AlertDialog.Footer>
			<AlertDialog.Cancel>Annuler</AlertDialog.Cancel>
			<AlertDialog.Action
				class={buttonVariants({ variant: 'destructive' })}
				disabled={busy}
				onclick={confirmDelete}
			>
				Retirer du coffre
			</AlertDialog.Action>
		</AlertDialog.Footer>
	</AlertDialog.Content>
</AlertDialog.Root>
