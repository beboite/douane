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

	/* The stamp on each entry of the register. One word, the way a customs
	   officer answers. */
	const stamp: Record<string, { label: string; tone: string }> = {
		block: { label: 'bloqué', tone: 'stamp-block' },
		substitute: { label: 'substitué', tone: 'stamp-substitute' },
		warn: { label: 'noté', tone: 'stamp-warn' },
		allow: { label: 'passé', tone: 'stamp-quiet' }
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

	const grantStamp: Record<string, { label: string; tone: string }> = {
		approved: { label: 'accordé', tone: 'stamp-ok' },
		denied: { label: 'refusé', tone: 'stamp-block' },
		cancelled: { label: 'retiré', tone: 'stamp-quiet' }
	};
</script>

<div class="space-y-4">
	<div class="shadow-panel bg-card divide-border grid divide-y rounded-xl border sm:grid-cols-3 sm:divide-x sm:divide-y-0">
		<Metric
			label="valeurs surveillées"
			value={watching}
			format={count}
			icon={Eye}
			hint="reconnues dans tout ce qui part"
		/>
		<Metric
			label="fuites arrêtées"
			value={stopped}
			format={count}
			icon={stopped > 0 ? ShieldAlert : ShieldCheck}
			tone={stopped > 0 ? 'warn' : 'quiet'}
			hint={stopped > 0 ? 'bloquées ou réécrites avant de partir' : 'rien à arrêter pour le moment'}
		/>
		<Metric
			label="demandes en attente"
			value={pending.length}
			format={count}
			icon={HandCoins}
			tone={pending.length > 0 ? 'accent' : 'quiet'}
			hint={pending.length > 0 ? 'un agent attend votre décision' : 'aucun agent n’attend'}
		/>
	</div>

	{#if pending.length > 0}
		<Panel
			title="Demandes d’accès"
			note="un agent a demandé une clé par MCP et continue de travailler en attendant. Accorder livre le nom, jamais la valeur : elle ne se déplace que par la rédemption locale"
			flush
		>
			<ul class="divide-border divide-y">
				{#each pending as g (g.id)}
					<li class="px-4 py-3.5" transition:slide={{ duration: 180, easing: cubicOut }}>
						<div class="flex flex-wrap items-center gap-x-3 gap-y-2">
							<div class="min-w-0 flex-1">
								<div class="flex flex-wrap items-baseline gap-x-2">
									<span class="text-sm font-semibold">{g.client}</span>
									<span class="text-muted-foreground text-xs">demande</span>
									<code class="bg-muted rounded px-1.5 py-0.5 font-mono text-xs">{g.reference}</code>
									<span class="text-muted-foreground text-xs" title={g.at}>{ago(g.at)}</span>
								</div>
								<p class="text-muted-foreground mt-1 border-l-2 pl-2.5 text-sm italic">
									{g.reason}
								</p>
							</div>

							<div class="flex shrink-0 items-center gap-1.5">
								<Select.Root
									type="single"
									value={ttl[g.id] ?? '24'}
									onValueChange={(v) => (ttl = { ...ttl, [g.id]: v })}
								>
									<Select.Trigger class="h-8 w-[7.5rem] text-xs">
										{TTLS.find((t) => t.value === (ttl[g.id] ?? '24'))?.label}
									</Select.Trigger>
									<Select.Content>
										{#each TTLS as t (t.value)}
											<Select.Item value={t.value} label={t.label}>{t.label}</Select.Item>
										{/each}
									</Select.Content>
								</Select.Root>
								<Button size="sm" class="h-8" disabled={busy === g.id} onclick={() => decide(g, true)}>
									accorder
								</Button>
								<Button
									size="sm"
									variant="outline"
									class="text-destructive h-8"
									disabled={busy === g.id}
									onclick={() => (refusing = refusing === g.id ? null : g.id)}
								>
									refuser
								</Button>
							</div>
						</div>

						{#if refusing === g.id}
							<div
								class="mt-2.5 flex items-center gap-1.5"
								transition:slide={{ duration: 150, easing: cubicOut }}
							>
								<Input
									bind:value={refusalNote}
									placeholder="un mot pour l’agent (facultatif)"
									class="h-8 max-w-md text-xs"
								/>
								<Button
									size="sm"
									variant="destructive"
									class="h-8"
									disabled={busy === g.id}
									onclick={() => decide(g, false)}
								>
									confirmer le refus
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
		note="ce que douane a arrêté ou réécrit avant que ça parte. Chaque ligne nomme la référence ou le motif, jamais le texte trouvé : un registre plein d’identifiants serait pire que la fuite qu’il signale"
		flush
		cap={alerts.length ? 'max-h-[30rem]' : ''}
	>
		{#if alerts.length === 0}
			<Empty
				icon={ShieldCheck}
				title="rien à signaler"
				hint="aucun corps partant ne portait de valeur du coffre, et chaque appel avait le droit de passer"
			/>
		{:else}
			<ul class="divide-border divide-y">
				{#each alerts as a (a.id)}
					<li class="flex items-center gap-3 px-4 py-2.5">
						<span class="stamp {stamp[a.action]?.tone ?? 'stamp-quiet'}">
							{stamp[a.action]?.label ?? a.action}
						</span>
						<div class="min-w-0 flex-1">
							<p class="truncate text-sm">
								<span class="font-mono text-xs">{story(a)}</span>
							</p>
							<p class="text-muted-foreground mt-0.5 flex items-center gap-2 text-xs">
								{a.client}
								<TrustBadge trust={a.trust} />
								<span title={a.at}>{when(a.at)}</span>
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
			note="les demandes déjà tranchées, et si la clé accordée a servi"
			flush
		>
			<ul class="divide-border divide-y">
				{#each decided as g (g.id)}
					<li class="flex items-center gap-3 px-4 py-2.5">
						<span class="stamp {grantStamp[g.status]?.tone ?? 'stamp-quiet'}">
							{grantStamp[g.status]?.label ?? g.status}
						</span>
						<div class="min-w-0 flex-1">
							<p class="truncate text-sm">
								<span class="font-medium">{g.client}</span>
								<span class="text-muted-foreground">·</span>
								<code class="font-mono text-xs">{g.reference}</code>
							</p>
							<p class="text-muted-foreground mt-0.5 truncate text-xs">
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

<style>
	/* The customs stamp: one word, inked, slightly off the horizontal the way a
	   hand stamp lands. The register is the signature of this page and the only
	   place that earns the flourish. */
	.stamp {
		display: inline-block;
		flex-shrink: 0;
		width: 5.5rem;
		text-align: center;
		padding: 0.15rem 0.4rem;
		border: 1.5px solid currentcolor;
		border-radius: 0.25rem;
		font-size: 0.62rem;
		font-weight: 700;
		letter-spacing: 0.09em;
		text-transform: uppercase;
		transform: rotate(-2deg);
		opacity: 0.9;
	}
	.stamp-block {
		color: var(--destructive);
	}
	.stamp-substitute {
		color: var(--primary);
	}
	.stamp-warn {
		color: var(--trust-mid);
	}
	.stamp-ok {
		color: var(--trust-max);
	}
	.stamp-quiet {
		color: var(--muted-foreground);
		transform: none;
		border-style: dashed;
		border-width: 1px;
	}
	@media (prefers-reduced-motion: reduce) {
		.stamp {
			transform: none;
		}
	}
</style>
