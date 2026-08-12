<script lang="ts">
	import { slide } from 'svelte/transition';
	import { cubicOut } from 'svelte/easing';
	import { toast } from 'svelte-sonner';
	import { Button } from '$lib/components/ui/button';
	import { Input } from '$lib/components/ui/input';
	import * as Select from '$lib/components/ui/select';
	import Eye from '@lucide/svelte/icons/eye';
	import HandCoins from '@lucide/svelte/icons/hand-coins';
	import ShieldCheck from '@lucide/svelte/icons/shield-check';
	import ShieldAlert from '@lucide/svelte/icons/shield-alert';
	import Empty from './empty.svelte';
	import Metric from './metric.svelte';
	import Panel from './panel.svelte';
	import TrustBadge from './trust-badge.svelte';
	import { api, type Alert, type Grant } from '$lib/api';
	import { ago, count, when } from '$lib/format';
	import { session } from '$lib/session.svelte';

	let {
		alerts,
		grants,
		watching,
		reload
	}: {
		alerts: Alert[];
		grants: Grant[];
		watching: number;
		reload: () => Promise<void>;
	} = $props();

	const TTLS = [
		{ value: '1', label: '1 heure' },
		{ value: '24', label: '24 heures' },
		{ value: '168', label: '7 jours' }
	];

	const pending = $derived(grants.filter((g) => g.status === 'pending'));
	const decided = $derived(grants.filter((g) => g.status !== 'pending').slice(0, 20));

	/* What the scrubber actually stopped: a vault value or a credential shape,
	   blocked or rewritten. Routing refusals live in the feed below but are a
	   rule doing its job, not a leak, so they stay out of the headline. */
	const stopped = $derived(
		alerts.filter(
			(a) =>
				(a.kind === 'vault' || a.kind === 'shape') &&
				(a.action === 'block' || a.action === 'substitute')
		).length
	);

	let ttl = $state<Record<number, string>>({});
	let refusing = $state<number | null>(null);
	let refusalNote = $state('');
	let busy = $state<number | null>(null);

	async function decide(g: Grant, approve: boolean) {
		busy = g.id;
		try {
			await api.decideGrant(session.token, g.id, {
				approve,
				note: approve ? undefined : refusalNote.trim() || undefined,
				ttl_hours: approve ? Number(ttl[g.id] ?? '24') : undefined
			});
			toast.success(
				approve
					? `${g.reference} accordé à ${g.client}`
					: `${g.reference} refusé à ${g.client}`
			);
		} catch (e) {
			// A 409 is the other tab having answered first, which is fine.
			toast.error((e as Error).message);
		} finally {
			busy = null;
			refusing = null;
			refusalNote = '';
			await reload();
		}
	}

	/* The verdict on each entry of the register. One word, read at a glance,
	   which is the only job a status has. */
	const verdict: Record<string, { label: string; tone: string }> = {
		block: { label: 'Bloqué', tone: 'pill-block' },
		substitute: { label: 'Substitué', tone: 'pill-substitute' },
		warn: { label: 'Noté', tone: 'pill-warn' },
		allow: { label: 'Passé', tone: 'pill-quiet' }
	};

	/* What happened, said as a sentence. The reference or the pattern, never
	   the matched text: the broker does not repeat what it caught. */
	function story(a: Alert): string {
		switch (a.kind) {
			case 'vault':
				return `la valeur de ${a.label} allait chez ${a.provider}`;
			case 'shape':
				return `quelque chose ressemblant à ${a.label} allait chez ${a.provider}`;
			case 'trust':
				return `${a.label} demandait ${a.provider}, au-dessus du plafond de ${a.client}`;
			case 'route':
				return `${a.client} demandait la route ${a.label}, qui ne lui est pas ouverte`;
			default:
				return `${a.label} chez ${a.provider}`;
		}
	}

	const grantVerdict: Record<string, { label: string; tone: string }> = {
		approved: { label: 'Accordé', tone: 'pill-ok' },
		denied: { label: 'Refusé', tone: 'pill-block' },
		cancelled: { label: 'Retiré', tone: 'pill-quiet' }
	};
</script>

<div class="space-y-5">
	<!-- Three cards, side by side, each carrying one figure. They sit on the
	     same surface as the panels below so the eye reads one page, not a
	     header bolted onto a list. -->
	<div class="grid gap-4 sm:grid-cols-3">
		<div class="shadow-panel bg-card ring-foreground/10 rounded-xl ring-1">
			<Metric
				label="valeurs surveillées"
				value={watching}
				format={count}
				icon={Eye}
				hint="reconnues dans tout ce qui part"
			/>
		</div>
		<!-- Deliberately not red: a leak that was caught is the broker doing its
		     job, and an alarm colour here reads as something being wrong. -->
		<div class="shadow-panel bg-card ring-foreground/10 rounded-xl ring-1">
			<Metric
				label="fuites arrêtées"
				value={stopped}
				format={count}
				icon={stopped > 0 ? ShieldAlert : ShieldCheck}
				tone={stopped > 0 ? 'accent' : 'quiet'}
				hint={stopped > 0 ? 'bloquées ou réécrites avant de partir' : 'rien à arrêter'}
			/>
		</div>
		<div class="shadow-panel bg-card ring-foreground/10 rounded-xl ring-1">
			<Metric
				label="en attente"
				value={pending.length}
				format={count}
				icon={HandCoins}
				tone={pending.length > 0 ? 'accent' : 'quiet'}
				hint={pending.length > 0 ? 'un agent attend votre décision' : 'aucun agent n’attend'}
			/>
		</div>
	</div>

	{#if pending.length > 0}
		<Panel
			title="Demandes d’accès"
			note="Accorder livre le nom de la référence, jamais sa valeur."
			flush
		>
			<ul class="divide-border divide-y">
				{#each pending as g (g.id)}
					<li class="px-5 py-4" transition:slide={{ duration: 180, easing: cubicOut }}>
						<div class="flex flex-wrap items-start justify-between gap-x-4 gap-y-3">
							<div class="min-w-0 flex-1">
								<div class="flex flex-wrap items-baseline gap-x-2 gap-y-1">
									<span class="text-sm font-semibold">{g.client}</span>
									<span class="text-muted-foreground text-xs">demande</span>
									<span class="bg-muted rounded-md px-2 py-0.5 text-xs font-semibold">
										{g.reference}
									</span>
									<span class="text-muted-foreground text-xs" title={g.at}>{ago(g.at)}</span>
								</div>
								<p class="text-muted-foreground mt-1.5 border-l-2 pl-3 text-sm">
									{g.reason}
								</p>
							</div>

							<!-- The one place in the app where a click costs something, so the
							     two answers are spelled out and filled, never an icon. -->
							<div class="flex shrink-0 flex-wrap items-center gap-2">
								<Select.Root
									type="single"
									value={ttl[g.id] ?? '24'}
									onValueChange={(v) => (ttl = { ...ttl, [g.id]: v })}
								>
									<Select.Trigger class="w-[7.5rem]" aria-label="Durée de l’accès">
										{TTLS.find((t) => t.value === (ttl[g.id] ?? '24'))?.label}
									</Select.Trigger>
									<Select.Content>
										{#each TTLS as t (t.value)}
											<Select.Item value={t.value} label={t.label}>{t.label}</Select.Item>
										{/each}
									</Select.Content>
								</Select.Root>
								<Button
									size="sm"
									class="h-8 px-4"
									disabled={busy === g.id}
									onclick={() => decide(g, true)}
								>
									Accorder
								</Button>
								<Button
									size="sm"
									variant="destructive"
									class="h-8 px-4"
									disabled={busy === g.id}
									onclick={() => (refusing = refusing === g.id ? null : g.id)}
								>
									Refuser
								</Button>
							</div>
						</div>

						{#if refusing === g.id}
							<div
								class="mt-3 flex flex-wrap items-center gap-2"
								transition:slide={{ duration: 150, easing: cubicOut }}
							>
								<Input
									bind:value={refusalNote}
									placeholder="Un mot pour l’agent (facultatif)"
									class="h-8 max-w-md flex-1"
								/>
								<Button
									size="sm"
									variant="destructive"
									class="h-8 px-4"
									disabled={busy === g.id}
									onclick={() => decide(g, false)}
								>
									Confirmer le refus
								</Button>
							</div>
						{/if}
					</li>
				{/each}
			</ul>
		</Panel>
	{/if}

	<Panel
		title="Registre des interceptions"
		note="Chaque ligne nomme la référence ou le motif, jamais le texte trouvé."
		flush
		cap={alerts.length ? 'max-h-[30rem]' : ''}
	>
		{#if alerts.length === 0}
			<Empty
				icon={ShieldCheck}
				title="Rien à signaler"
				hint="Aucune requête sortante ne portait de valeur du coffre, et chaque appel avait le droit de passer."
			/>
		{:else}
			<ul class="divide-border divide-y">
				{#each alerts as a (a.id)}
					<li class="flex items-center gap-3 px-5 py-3">
						<span class="pill {verdict[a.action]?.tone ?? 'pill-quiet'}">
							{verdict[a.action]?.label ?? a.action}
						</span>
						<div class="min-w-0 flex-1">
							<p class="truncate text-sm" title={story(a)}>{story(a)}</p>
							<p class="text-muted-foreground mt-1 flex items-center gap-2 text-xs">
								{a.client}
								<TrustBadge trust={a.trust} />
								<span class="tabular" title={a.at}>{when(a.at)}</span>
							</p>
						</div>
					</li>
				{/each}
			</ul>
		{/if}
	</Panel>

	{#if decided.length > 0}
		<Panel
			title="Décisions récentes"
			note="Les demandes déjà tranchées, et si la clé accordée a servi."
			flush
		>
			<ul class="divide-border divide-y">
				{#each decided as g (g.id)}
					<li class="flex items-center gap-3 px-5 py-3">
						<span class="pill {grantVerdict[g.status]?.tone ?? 'pill-quiet'}">
							{grantVerdict[g.status]?.label ?? g.status}
						</span>
						<div class="min-w-0 flex-1">
							<p class="truncate text-sm">
								<span class="font-medium">{g.client}</span>
								<span class="text-muted-foreground">·</span>
								<span class="text-muted-foreground text-xs font-semibold">{g.reference}</span>
							</p>
							<p class="text-muted-foreground mt-1 truncate text-xs">
								{#if g.status === 'approved'}
									{g.used_at ? `utilisée ${ago(g.used_at)}` : 'jamais utilisée'}
									{#if g.expires_at}
										· expire {when(g.expires_at)}
									{/if}
								{:else if g.note}
									« {g.note} »
								{:else if g.decided_at}
									{ago(g.decided_at)}
								{/if}
							</p>
						</div>
					</li>
				{/each}
			</ul>
		</Panel>
	{/if}
</div>
