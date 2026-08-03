<script lang="ts">
	import * as AlertDialog from '$lib/components/ui/alert-dialog';
	import { Badge } from '$lib/components/ui/badge';
	import { Button, buttonVariants } from '$lib/components/ui/button';
	import { Input } from '$lib/components/ui/input';
	import Copy from '@lucide/svelte/icons/copy';
	import Link2 from '@lucide/svelte/icons/link-2';
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

	async function copy(text: string) {
		try {
			await navigator.clipboard.writeText(text);
			toast.success('copié', { description: text });
		} catch {
			toast.error('le presse-papier a refusé');
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
				description: `le scrubber surveille ${res.watching} valeurs en sortie`
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
	note="{count(vault?.entries.length ?? 0)} entrées · {count(
		vault?.watching ?? 0
	)} valeurs surveillées en sortie. Aucune ne se relit, ni ici ni ailleurs"
>
	{#snippet actions()}
		<div class="flex shrink-0 items-center gap-2">
			<div class="relative">
				<Search
					class="text-muted-foreground pointer-events-none absolute top-1/2 left-2.5 size-3.5 -translate-y-1/2"
				/>
				<Input
					bind:value={filter}
					placeholder="filtrer"
					class="h-8 w-44 pl-8"
					autocomplete="off"
					spellcheck="false"
				/>
			</div>
			<Button size="sm" class="h-8" onclick={deposit}>
				<Plus class="size-3.5" />
				déposer
			</Button>
		</div>
	{/snippet}

	<div class="space-y-6">
		{#each groups as [namespace, entries] (namespace)}
			<section>
				<div class="mb-2 flex items-baseline gap-2">
					<h3 class="font-mono text-sm font-semibold">{namespace}</h3>
					<Badge variant="secondary" class="rounded-full px-1.5 py-0 text-[0.65rem]">
						{entries.length}
					</Badge>
					<div class="bg-border h-px flex-1"></div>
				</div>

				<ul class="divide-border bg-sunken/50 divide-y overflow-hidden rounded-lg border">
					{#each entries as entry (reference(entry))}
						{@const locked = inUse.has(reference(entry))}
						{@const sameAs = twinsOf(entry)}
						<li class="hover:bg-card/70 group flex items-center gap-4 px-3 py-2.5 transition-colors">
							<!-- Fixed rather than elastic: the placeholder belongs next to the
							     name it stands for, and a flexible column would push it to the
							     far side of a wide screen. -->
							<div class="w-[20rem] min-w-0 shrink-0">
								<div class="flex flex-wrap items-center gap-x-2 gap-y-1">
									<span class="font-mono text-sm font-medium">{entry.key}</span>
									{#if locked}
										<span
											class="text-muted-foreground inline-flex items-center gap-1 text-[0.65rem]"
											title="la configuration en cours s’authentifie avec, le broker refuse de la supprimer"
										>
											<Lock class="size-3" />
											en service
										</span>
									{/if}
									{#if sameAs.length}
										<span
											class="text-trust-mid inline-flex items-center gap-1 text-[0.65rem]"
											title="même valeur que {sameAs.join(', ')}"
										>
											<Link2 class="size-3" />
											même valeur que {sameAs.length === 1 ? sameAs[0] : `${sameAs.length} autres`}
										</span>
									{/if}
								</div>
								{#if entry.note}
									<p class="text-muted-foreground mt-0.5 truncate text-xs">{entry.note}</p>
								{/if}
							</div>

							<button
								class="text-muted-foreground hover:text-foreground hidden min-w-0 flex-1 items-center gap-1.5 truncate font-mono text-xs transition-colors md:inline-flex"
								onclick={() => copy(entry.placeholder)}
								title="copier le repère à donner à un agent à la place de la valeur"
							>
								{entry.placeholder}
								<Copy class="size-3 opacity-0 transition-opacity group-hover:opacity-100" />
							</button>

							<div
								class="text-muted-foreground tabular hidden w-[9.5rem] shrink-0 text-right text-xs md:block"
							>
								<div class="font-mono">{entry.fingerprint.slice(0, 12)}</div>
								<div class="mt-0.5 opacity-70">
									{bytes(entry.bytes)} · <span title={entry.updated_at}>{ago(entry.updated_at)}</span>
								</div>
							</div>

							<div class="flex shrink-0 gap-0.5">
								<Button
									size="icon"
									variant="ghost"
									class="size-7"
									title="remplacer la valeur ou la note"
									onclick={() => replace(entry)}
								>
									<PenLine class="size-3.5" />
								</Button>
								<Button
									size="icon"
									variant="ghost"
									class="size-7 {locked ? '' : 'hover:text-destructive'}"
									disabled={locked}
									title={locked ? 'la configuration en cours en dépend' : 'retirer du coffre'}
									onclick={() => (deleting = entry)}
								>
									<Trash2 class="size-3.5" />
								</Button>
							</div>
						</li>
					{/each}
				</ul>
			</section>
		{:else}
			<Empty
				icon={KeyRound}
				title={filter ? 'rien qui corresponde' : 'le coffre est vide'}
				hint={filter
					? 'le filtre porte sur la référence et sur la note, jamais sur la valeur'
					: 'déposez une première clé, le scrubber apprendra à la reconnaître en sortie dans la seconde'}
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
			<AlertDialog.Cancel>annuler</AlertDialog.Cancel>
			<AlertDialog.Action
				class={buttonVariants({ variant: 'destructive' })}
				disabled={busy}
				onclick={confirmDelete}
			>
				retirer
			</AlertDialog.Action>
		</AlertDialog.Footer>
	</AlertDialog.Content>
</AlertDialog.Root>
