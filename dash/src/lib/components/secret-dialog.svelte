<script lang="ts">
	import { Button } from '$lib/components/ui/button';
	import * as Dialog from '$lib/components/ui/dialog';
	import { Input } from '$lib/components/ui/input';
	import { Label } from '$lib/components/ui/label';
	import { Textarea } from '$lib/components/ui/textarea';
	import Eye from '@lucide/svelte/icons/eye';
	import EyeOff from '@lucide/svelte/icons/eye-off';
	import { toast } from 'svelte-sonner';
	import { api, type EntryInfo } from '$lib/api';
	import { session } from '$lib/session.svelte';

	let {
		open = $bindable(false),
		/** Set to replace an existing entry; left out to deposit a new one. */
		existing = null,
		reload
	}: {
		open?: boolean;
		existing?: EntryInfo | null;
		reload: () => Promise<void>;
	} = $props();

	let namespace = $state('');
	let key = $state('');
	let value = $state('');
	let note = $state('');
	let reveal = $state(false);
	let busy = $state(false);

	/* Reopening the dialog starts from whatever it is pointed at now, and never
	   from what was typed the last time it was open. */
	$effect(() => {
		if (open) {
			namespace = existing?.namespace ?? '';
			key = existing?.key ?? '';
			note = existing?.note ?? '';
			value = '';
			reveal = false;
		}
	});

	const valid = $derived(
		namespace.trim().length > 0 && !namespace.includes('.') && key.trim().length > 0
	);
	const reference = $derived(`${namespace.trim() || 'espace'}.${key.trim() || 'clé'}`);

	async function submit(event: SubmitEvent) {
		event.preventDefault();
		if (!valid || busy) return;
		busy = true;
		try {
			const body =
				value.length > 0
					? { value, note: note.trim() || null }
					: existing
						? { note: note.trim() || null }
						: null;
			if (!body) {
				toast.error('un dépôt sans valeur ne dépose rien');
				return;
			}
			const res = await api.putSecret(session.token, namespace.trim(), key.trim(), body);
			// Cleared before anything else can go wrong, so a failure never leaves
			// a credential sitting in a form field behind an error toast.
			value = '';
			await reload();
			open = false;
			toast.success(`${res.entry.namespace}.${res.entry.key} est en coffre`, {
				description: `le scrubber surveille ${res.watching} valeurs en sortie`
			});
		} catch (e) {
			toast.error('rien n’a été écrit', { description: (e as Error).message });
		} finally {
			busy = false;
		}
	}
</script>

<Dialog.Root bind:open>
	<Dialog.Content class="sm:max-w-lg">
		<form onsubmit={submit}>
			<Dialog.Header>
				<Dialog.Title>
					{existing ? 'Remplacer une valeur' : 'Déposer un secret'}
				</Dialog.Title>
				<Dialog.Description>
					Le coffre ne rend jamais une valeur. Une fois déposée, elle ressort chez le fournisseur et
					nulle part ailleurs, et se remplace sans jamais se relire.
				</Dialog.Description>
			</Dialog.Header>

			<div class="grid gap-4 py-4">
				<div class="grid grid-cols-2 gap-3">
					<div class="grid gap-2">
						<Label for="ns">espace</Label>
						<Input
							id="ns"
							bind:value={namespace}
							disabled={!!existing}
							placeholder="proxy"
							autocomplete="off"
							spellcheck="false"
						/>
					</div>
					<div class="grid gap-2">
						<Label for="key">clé</Label>
						<Input
							id="key"
							bind:value={key}
							disabled={!!existing}
							placeholder="password"
							autocomplete="off"
							spellcheck="false"
						/>
					</div>
				</div>

				{#if namespace.includes('.')}
					<p class="text-destructive text-xs">
						un espace ne peut pas contenir de point : la référence se coupe au premier, donc
						<code>a.b.c</code> se lirait comme l’espace <code>a</code>.
					</p>
				{/if}

				<div class="grid gap-2">
					<div class="flex items-center justify-between">
						<Label for="value">
							valeur
							{#if existing}<span class="text-muted-foreground font-normal">
									· vide pour ne changer que la note</span
								>{/if}
						</Label>
						<Button
							type="button"
							variant="ghost"
							size="sm"
							class="h-6 px-2 text-xs"
							onclick={() => (reveal = !reveal)}
						>
							{#if reveal}<EyeOff class="size-3.5" />masquer{:else}<Eye class="size-3.5" />montrer{/if}
						</Button>
					</div>
					<Input
						id="value"
						type={reveal ? 'text' : 'password'}
						bind:value
						placeholder="collez la valeur"
						autocomplete="off"
						spellcheck="false"
						class="font-mono"
					/>
				</div>

				<div class="grid gap-2">
					<Label for="note">note</Label>
					<Textarea
						id="note"
						bind:value={note}
						rows={2}
						placeholder="à quoi elle sert, où la retrouver"
						class="resize-none"
					/>
				</div>

				<p class="text-muted-foreground text-xs">
					sera référencée <code class="text-foreground font-mono">{reference}</code>, et remplaçable
					dans un prompt par
					<code class="text-foreground font-mono">
						__DOUANE_{namespace.trim().toUpperCase().replace(/[^A-Z0-9]/g, '_') || 'ESPACE'}_{key
							.trim()
							.toUpperCase()
							.replace(/[^A-Z0-9]/g, '_') || 'CLE'}__
					</code>
				</p>
			</div>

			<Dialog.Footer>
				<Button type="button" variant="ghost" onclick={() => (open = false)}>annuler</Button>
				<Button type="submit" disabled={!valid || busy}>
					{existing ? 'remplacer' : 'déposer'}
				</Button>
			</Dialog.Footer>
		</form>
	</Dialog.Content>
</Dialog.Root>
