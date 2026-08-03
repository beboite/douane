<script lang="ts">
	import * as AlertDialog from '$lib/components/ui/alert-dialog';
	import { Button, buttonVariants } from '$lib/components/ui/button';
	import { Input } from '$lib/components/ui/input';
	import * as Table from '$lib/components/ui/table';
	import Copy from '@lucide/svelte/icons/copy';
	import Lock from '@lucide/svelte/icons/lock';
	import Plus from '@lucide/svelte/icons/plus';
	import Trash2 from '@lucide/svelte/icons/trash-2';
	import PenLine from '@lucide/svelte/icons/pen-line';
	import { toast } from 'svelte-sonner';
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
			<Input
				bind:value={filter}
				placeholder="filtrer"
				class="h-8 w-40"
				autocomplete="off"
				spellcheck="false"
			/>
			<Button size="sm" onclick={deposit}>
				<Plus class="size-3.5" />
				déposer
			</Button>
		</div>
	{/snippet}

	<div class="space-y-5">
		{#each groups as [namespace, entries] (namespace)}
			<div>
				<h3 class="text-muted-foreground mb-1.5 font-mono text-xs tracking-wide">
					{namespace}
				</h3>
				<div class="overflow-hidden rounded-md border">
					<Table.Root>
						<Table.Body>
							{#each entries as entry (reference(entry))}
								{@const locked = inUse.has(reference(entry))}
								<Table.Row>
									<Table.Cell class="w-[18rem]">
										<div class="flex items-center gap-2">
											<span class="font-mono text-sm">{entry.key}</span>
											{#if locked}
												<span
													class="text-muted-foreground inline-flex items-center gap-1 text-[0.65rem]"
													title="la configuration en cours s’authentifie avec, le broker refuse de la supprimer"
												>
													<Lock class="size-3" />
													en service
												</span>
											{/if}
										</div>
										{#if entry.note}
											<div class="text-muted-foreground mt-0.5 text-xs">{entry.note}</div>
										{/if}
									</Table.Cell>

									<Table.Cell class="text-muted-foreground font-mono text-xs">
										<button
											class="hover:text-foreground inline-flex items-center gap-1.5"
											onclick={() => copy(entry.placeholder)}
											title="copier le repère à donner à un agent à la place de la valeur"
										>
											{entry.placeholder}
											<Copy class="size-3" />
										</button>
									</Table.Cell>

									<Table.Cell class="text-muted-foreground text-right font-mono text-xs">
										{#if twinsOf(entry).length}
											<span
												class="text-trust-mid"
												title="même valeur que {twinsOf(entry).join(', ')}"
											>
												{entry.fingerprint.slice(0, 12)}
											</span>
										{:else}
											{entry.fingerprint.slice(0, 12)}
										{/if}
									</Table.Cell>

									<Table.Cell class="tabular text-muted-foreground text-right text-xs">
										{bytes(entry.bytes)}
									</Table.Cell>

									<Table.Cell class="text-muted-foreground text-right text-xs">
										<span title={entry.updated_at}>{ago(entry.updated_at)}</span>
									</Table.Cell>

									<Table.Cell class="w-[7rem]">
										<div class="flex justify-end gap-0.5">
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
												title={locked
													? 'la configuration en cours en dépend'
													: 'retirer du coffre'}
												onclick={() => (deleting = entry)}
											>
												<Trash2 class="size-3.5" />
											</Button>
										</div>
									</Table.Cell>
								</Table.Row>
							{/each}
						</Table.Body>
					</Table.Root>
				</div>
			</div>
		{:else}
			<p class="text-muted-foreground py-8 text-center text-sm">
				{filter ? 'rien qui corresponde' : 'le coffre est vide'}
			</p>
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
