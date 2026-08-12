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

	const upper = (s: string, fallback: string) =>
		s.trim().toUpperCase().replace(/[^A-Z0-9]/g, '_') || fallback;
	const placeholder = $derived(`__DOUANE_${upper(namespace, 'ESPACE')}_${upper(key, 'CLE')}__`);

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
				toast.error('Un dépôt sans valeur ne dépose rien');
				return;
			}
			const res = await api.putSecret(session.token, namespace.trim(), key.trim(), body);
			// Cleared before anything else can go wrong, so a failure never leaves
			// a credential sitting in a form field behind an error toast.
			value = '';
			await reload();
			open = false;
			toast.success(`${res.entry.namespace}.${res.entry.key} est en coffre`, {
				description: `Le scrubber surveille ${res.watching} valeurs en sortie`
			});
		} catch (e) {
			toast.error('Rien n’a été écrit', { description: (e as Error).message });
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
					{existing ? 'Remplacer une valeur' : 'Déposer une valeur'}
				</Dialog.Title>
				<Dialog.Description>
					Le coffre ne rend jamais une valeur. Elle entre ici, elle ressort chez le fournisseur, et
					nulle part ailleurs : elle se remplace, elle ne se relit pas.
				</Dialog.Description>
			</Dialog.Header>

			<div class="grid gap-4 py-5">
				<div class="grid grid-cols-2 gap-3">
					<div class="grid gap-2">
						<Label for="ns">Espace</Label>
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
						<Label for="key">Clé</Label>
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
					<p class="text-destructive text-xs leading-relaxed">
						Un espace ne peut pas contenir de point : la référence se coupe au premier, donc
						<span class="bg-sunken rounded px-1 py-0.5 font-medium">a.b.c</span>
						se lirait comme l’espace <span class="bg-sunken rounded px-1 py-0.5 font-medium">a</span
						>.
					</p>
				{/if}

				<div class="grid gap-2">
					<div class="flex items-center justify-between">
						<Label for="value">
							Valeur
							{#if existing}<span class="text-muted-foreground font-normal">
									· vide pour ne changer que la note</span
								>{/if}
						</Label>
						<!-- Shows what is being typed into this field, nothing else: a value
						     already in the vault has no way back out to any screen. -->
						<Button
							type="button"
							variant="ghost"
							size="xs"
							onclick={() => (reveal = !reveal)}
						>
							{#if reveal}<EyeOff class="size-3.5" />Masquer la saisie{:else}<Eye
									class="size-3.5"
								/>Voir la saisie{/if}
						</Button>
					</div>
					<Input
						id="value"
						type={reveal ? 'text' : 'password'}
						bind:value
						placeholder="Collez la valeur"
						autocomplete="off"
						spellcheck="false"
					/>
				</div>

				<div class="grid gap-2">
					<Label for="note">Note</Label>
					<Textarea
						id="note"
						bind:value={note}
						rows={2}
						placeholder="À quoi elle sert, où la retrouver"
						class="resize-none"
					/>
				</div>

				<!-- The two names this deposit creates, shown while it is being typed:
				     the reference names the entry, the placeholder is what leaves the
				     building. Neither is the value. -->
				<div class="bg-sunken grid gap-2 rounded-xl p-4 text-xs">
					<div class="flex flex-wrap items-baseline gap-x-2 gap-y-1">
						<span class="text-muted-foreground">Référencée</span>
						<span class="bg-background rounded-md px-1.5 py-0.5 font-semibold">{reference}</span>
					</div>
					<div class="flex flex-wrap items-baseline gap-x-2 gap-y-1">
						<span class="text-muted-foreground">Repère</span>
						<span class="bg-background rounded-md px-1.5 py-0.5 font-semibold">{placeholder}</span>
					</div>
					<p class="text-muted-foreground leading-relaxed">
						Le repère est le nom que l’agent écrit à la place du secret dans une conversation. Il ne
						vaut rien pour un fournisseur, douane seule sait à quelle valeur il renvoie.
					</p>
				</div>
			</div>

			<Dialog.Footer>
				<Button type="button" variant="outline" onclick={() => (open = false)}>Annuler</Button>
				<Button type="submit" disabled={!valid || busy}>
					{existing ? 'Remplacer la valeur' : 'Déposer la valeur'}
				</Button>
			</Dialog.Footer>
		</form>
	</Dialog.Content>
</Dialog.Root>
