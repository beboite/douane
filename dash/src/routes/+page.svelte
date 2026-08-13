<script lang="ts">
	import { fly } from 'svelte/transition';
	import { cubicOut } from 'svelte/easing';
	import { Button } from '$lib/components/ui/button';
	import * as Card from '$lib/components/ui/card';
	import { Input } from '$lib/components/ui/input';
	import { Label } from '$lib/components/ui/label';
	import * as Select from '$lib/components/ui/select';
	import { Toaster } from '$lib/components/ui/sonner';
	import Activity from '@lucide/svelte/icons/activity';
	import Boxes from '@lucide/svelte/icons/boxes';
	import KeyRound from '@lucide/svelte/icons/key-round';
	import LogOut from '@lucide/svelte/icons/log-out';
	import Moon from '@lucide/svelte/icons/moon';
	import RefreshCw from '@lucide/svelte/icons/refresh-cw';
	import ShieldCheck from '@lucide/svelte/icons/shield-check';
	import Signpost from '@lucide/svelte/icons/signpost';
	import Sun from '@lucide/svelte/icons/sun';
	import Users from '@lucide/svelte/icons/users';
	import Vault from '@lucide/svelte/icons/vault';

	import TabAgents from '$lib/components/tab-agents.svelte';
	import TabModels from '$lib/components/tab-models.svelte';
	import TabProtection from '$lib/components/tab-protection.svelte';
	import TabRoutes from '$lib/components/tab-routes.svelte';
	import TabTraffic from '$lib/components/tab-traffic.svelte';
	import TabVault from '$lib/components/tab-vault.svelte';
	import {
		api,
		ApiError,
		UNAUTHORIZED,
		type Alert,
		type Call,
		type CatalogView,
		type ConfigView,
		type Grant,
		type LiveView,
		type Summary,
		type Thread,
		type VaultView
	} from '$lib/api';
	import { count } from '$lib/format';
	import { session } from '$lib/session.svelte';

	const WINDOWS = [
		{ value: '1', label: 'dernière heure' },
		{ value: '24', label: '24 heures' },
		{ value: '168', label: '7 jours' },
		{ value: '720', label: '30 jours' }
	];

	let hours = $state('24');
	/* The screen the dashboard opens on: who is working right now. The barrier's
	   verdict is one click away and keeps its own screen; this is the one that
	   answers the question asked on arrival. */
	let tab = $state('agents');
	let typed = $state('');
	let rejected = $state('');

	let summary = $state<Summary | null>(null);
	let config = $state<ConfigView | null>(null);
	let catalog = $state<CatalogView | null>(null);
	let vault = $state<VaultView | null>(null);
	let calls = $state<Call[]>([]);
	let threads = $state<Thread[]>([]);
	let alerts = $state<Alert[]>([]);
	let grants = $state<Grant[]>([]);
	let thread = $state<string | null>(null);
	let live = $state<LiveView | null>(null);
	/** How long an agent stays listed after its last call. */
	let idle = $state(300);

	let loading = $state(false);
	let failure = $state('');
	let dark = $state(true);

	const atWork = $derived(live?.agents.length ?? 0);
	/** Agents whose current call has no answer yet: generating, right now. */
	const answering = $derived(live?.agents.filter((a) => a.in_flight > 0).length ?? 0);

	const pending = $derived(grants.filter((g) => g.status === 'pending').length);
	const repointed = $derived(config?.routes.filter((r) => r.overridden).length ?? 0);
	const served = $derived(catalog?.models.filter((m) => m.enabled).length ?? 0);

	/* What the barrier reads off: everything the broker did not simply wave
	   through. A refusal and a substitution are both interceptions here, because
	   both mean a credential was about to leave and did not. */
	const stopped = $derived(
		alerts.filter((a) => a.action === 'block' || a.action === 'substitute').length
	);

	/* Each screen names itself and says in one line what it answers, so the
	   panels below can keep their titles short. */
	const SCREENS: Record<string, { label: string; icon: typeof Activity; desc: string }> = {
		agents: {
			label: 'Agents',
			icon: Users,
			desc: 'qui travaille en ce moment, sur quel modèle et chez qui'
		},
		protection: {
			label: 'Protection',
			icon: ShieldCheck,
			desc: 'ce qui a été arrêté avant de sortir, et qui attend une décision'
		},
		trafic: {
			label: 'Trafic',
			icon: Activity,
			desc: 'qui a appelé quoi, et ce que ça a consommé'
		},
		modeles: {
			label: 'Modèles',
			icon: Boxes,
			desc: 'ce que chaque fournisseur vend, et ce que douane accepte de servir'
		},
		routes: {
			label: 'Routage',
			icon: Signpost,
			desc: 'où part chaque nom de modèle, et si les clés marchent encore'
		},
		coffre: {
			label: 'Coffre',
			icon: Vault,
			desc: 'les valeurs déposées, reconnues en sortie, jamais relues'
		}
	};

	async function load() {
		if (!session.unlocked) return;
		loading = true;
		failure = '';
		const h = Number(hours);
		try {
			const [s, c, m, v, k, t, a, g] = await Promise.all([
				api.summary(session.token, h),
				api.config(session.token),
				api.models(session.token),
				api.vault(session.token),
				api.calls(session.token, 200, { thread: thread ?? undefined }),
				api.threads(session.token, h),
				api.alerts(session.token),
				api.grants(session.token)
			]);
			summary = s.summary;
			config = c;
			catalog = m;
			vault = v;
			calls = k;
			threads = t;
			alerts = a;
			grants = g;
		} catch (e) {
			if (e instanceof ApiError && e.status === UNAUTHORIZED) {
				// The token stopped working: rotated, or wrong from the start.
				// Dropping it is the only useful response, and re-asking says so.
				rejected = 'ce jeton a été refusé';
				session.lock();
			} else {
				failure = (e as Error).message;
			}
		} finally {
			loading = false;
		}
	}

	/* The live screen has a loop of its own. Everything else on the dashboard is
	   fine at half a minute; an agent that started four seconds ago is not. It
	   fails silently on purpose: a red banner blinking every few seconds over a
	   working page is worse than a count that stopped moving. */
	async function refreshLive() {
		if (!session.unlocked) return;
		try {
			live = await api.live(session.token, idle);
		} catch (e) {
			if (e instanceof ApiError && e.status === UNAUTHORIZED) {
				rejected = 'ce jeton a été refusé';
				session.lock();
			}
		}
	}

	/** From an agent to its calls: the traffic screen, filtered on its thread. */
	function openThread(t: string) {
		thread = t;
		tab = 'trafic';
	}

	function unlock(event: SubmitEvent) {
		event.preventDefault();
		rejected = '';
		session.unlock(typed);
		typed = '';
	}

	function toggleTheme() {
		dark = !dark;
		document.documentElement.classList.toggle('dark', dark);
		try {
			localStorage.setItem('douane-theme', dark ? 'dark' : 'light');
		} catch {
			/* the choice holds for this page load */
		}
	}

	$effect(() => {
		dark = document.documentElement.classList.contains('dark');
	});

	// Reloads on unlock, on a change of window, and when a conversation is
	// picked apart. Everything else is a button.
	$effect(() => {
		void hours;
		void thread;
		void session.token;
		void load();
	});

	$effect(() => {
		if (!session.unlocked) return;
		const timer = setInterval(load, 30_000);
		return () => clearInterval(timer);
	});

	$effect(() => {
		void idle;
		void session.token;
		if (!session.unlocked) return;
		void refreshLive();
		const timer = setInterval(() => {
			// A hidden tab polls nothing: the answer would be stale the moment it
			// is looked at again, and the loop keeps a laptop awake for a screen
			// nobody is reading.
			if (!document.hidden) void refreshLive();
		}, 4_000);
		return () => clearInterval(timer);
	});
</script>

<Toaster position="bottom-right" />

<!-- The signature of the whole dashboard: the boom of a border post.
     Raised means nothing was stopped on this window, lowered means something
     was. It is the one drawing in the app, it says the state before a word is
     read, and it is the only thing allowed to move. -->
{#snippet barrier(raised: boolean, cls: string)}
	<!-- The viewBox is tall enough to hold the boom when it is up: swing a
	     118-unit arm off a pivot at y=72 and anything steeper than about 35
	     degrees leaves the frame, which reads as a broken drawing rather than
	     as an open road. -->
	<svg
		viewBox="0 0 200 150"
		class={cls}
		role="img"
		aria-label={raised ? 'barrière levée' : 'barrière baissée'}
	>
		<!-- The road under it, so the boom reads as blocking a way through. -->
		<line x1="0" y1="140" x2="200" y2="140" stroke="currentcolor" stroke-width="3" opacity="0.2" />
		<line
			x1="112"
			y1="140"
			x2="196"
			y2="140"
			stroke="currentcolor"
			stroke-width="3"
			stroke-dasharray="12 14"
			opacity="0.4"
		/>
		<!-- The post, on its footing. -->
		<rect x="18" y="130" width="34" height="9" rx="4" fill="currentcolor" opacity="0.45" />
		<rect x="28" y="72" width="14" height="60" rx="6" fill="currentcolor" opacity="0.85" />
		<circle cx="35" cy="72" r="8.5" fill="currentcolor" />
		<!-- The boom, hinged on the post. A CSS transition, so the reduced-motion
		     rule already covers it. -->
		<g
			style="transform-origin: 35px 72px; transition: transform 700ms cubic-bezier(0.34, 1.25, 0.5, 1); transform: rotate({raised
				? -32
				: 0}deg)"
		>
			<rect x="35" y="64" width="118" height="16" rx="8" fill="currentcolor" opacity="0.22" />
			{#each [0, 1, 2, 3] as i (i)}
				<rect
					x={41 + i * 28}
					y="64"
					width="17"
					height="16"
					rx="4"
					fill="currentcolor"
					opacity="0.95"
				/>
			{/each}
		</g>
	</svg>
{/snippet}

{#snippet navBadge(n: number, alarm: boolean)}
	{#if n > 0}
		<span
			class="tabular ml-auto inline-flex h-5 min-w-5 items-center justify-center rounded-full px-1.5 text-[0.7rem] font-bold {alarm
				? 'bg-destructive text-destructive-foreground'
				: 'bg-sidebar-accent text-sidebar-accent-foreground'}"
		>
			{count(n)}
		</span>
	{/if}
{/snippet}

<!-- The live count, which is the only badge on the rail that moves on its own.
     The dot is not decoration: it appears when a call is in flight and stops
     when none is, so the rail says "something is happening" from any screen. -->
{#snippet liveBadge()}
	{#if atWork > 0}
		<span class="ml-auto inline-flex shrink-0 items-center gap-1.5">
			{#if answering > 0}
				<span class="text-trust-max relative flex size-2 rounded-full bg-current">
					<span
						class="absolute inline-flex size-full animate-ping rounded-full bg-current opacity-60"
					></span>
				</span>
			{/if}
			<span
				class="tabular bg-sidebar-accent text-sidebar-accent-foreground inline-flex h-5 min-w-5 items-center justify-center rounded-full px-1.5 text-[0.7rem] font-bold"
			>
				{count(atWork)}
			</span>
		</span>
	{/if}
{/snippet}

{#snippet navItems(compact: boolean)}
	{#each Object.entries(SCREENS) as [id, s] (id)}
		{@const ScreenIcon = s.icon}
		{@const active = tab === id}
		<button
			onclick={() => (tab = id)}
			aria-current={active ? 'page' : undefined}
			class="focusable flex items-center gap-2.5 rounded-lg text-sm transition-colors {compact
				? 'shrink-0 px-3 py-2'
				: 'w-full px-3 py-2.5'} {active
				? 'bg-primary text-primary-foreground font-semibold shadow-sm'
				: 'text-sidebar-foreground/75 hover:bg-sidebar-accent hover:text-sidebar-foreground font-medium'}"
		>
			<ScreenIcon class="size-4 shrink-0" />
			{s.label}
			{#if id === 'agents'}{@render liveBadge()}
			{:else if id === 'protection'}{@render navBadge(pending, true)}
			{:else if id === 'routes'}{@render navBadge(repointed, false)}
			{:else if id === 'modeles'}{@render navBadge(served, false)}
			{:else if id === 'coffre'}{@render navBadge(vault?.entries.length ?? 0, false)}{/if}
		</button>
	{/each}
{/snippet}

{#if !session.unlocked}
	<main class="flex min-h-svh items-center justify-center p-6">
		<div class="w-full max-w-sm">
			<Card.Root class="shadow-lifted overflow-hidden">
				<div class="bg-sidebar text-primary flex justify-center border-b px-6 py-5">
					{@render barrier(true, 'h-24')}
				</div>
				<!-- The gap lives on the form, not on the card: a single child absorbs
				     the card's own `flex-col gap`, and the header ends up glued to the
				     field under it. -->
				<form onsubmit={unlock} class="flex flex-col gap-6 pt-6">
					<Card.Header>
						<div class="wordmark text-foreground text-xl">douane</div>
						<Card.Description class="mt-1 text-sm">
							Le poste de contrôle des clés. Il n'ouvre qu'au porteur du jeton.
						</Card.Description>
					</Card.Header>

					<Card.Content class="grid gap-2">
						<Label for="token">Jeton d'administration</Label>
						<Input
							id="token"
							type="password"
							bind:value={typed}
							placeholder="Collez le jeton"
							autocomplete="off"
							spellcheck="false"
						/>
						{#if rejected}
							<p class="text-destructive text-sm font-medium">{rejected}</p>
						{/if}

						<Button
							type="submit"
							size="lg"
							class="mt-3 w-full"
							disabled={typed.trim().length === 0}
						>
							<KeyRound class="size-4" />
							Ouvrir le tableau de bord
						</Button>

						<p class="text-muted-foreground mt-3 text-xs leading-relaxed">
							Le jeton vit dans le coffre sous <span class="bg-sunken rounded px-1 py-0.5"
								>douane.admin_token</span
							>, avec une copie locale sur la machine. Il reste dans cet onglet et disparaît à sa
							fermeture.
						</p>
					</Card.Content>
				</form>
			</Card.Root>
		</div>
	</main>
{:else}
	{@const screen = SCREENS[tab]}
	<div class="min-h-svh">
		<!-- The rail. Always there on a desk-sized screen: the five screens, and
		     at the bottom the one number the whole thing exists for. -->
		<aside
			class="bg-sidebar border-sidebar-border fixed inset-y-0 left-0 z-40 hidden w-64 flex-col border-r lg:flex"
		>
			<div class="flex items-center gap-3 px-5 pt-5 pb-3">
				<span class="text-primary shrink-0">
					{@render barrier(stopped === 0, 'h-9')}
				</span>
				<div class="min-w-0">
					<div class="wordmark text-sidebar-foreground">douane</div>
					<div class="text-muted-foreground text-[0.7rem]">contrôle des sorties</div>
				</div>
			</div>

			<nav class="flex flex-col gap-1 p-3">
				{@render navItems(false)}
			</nav>

			<div class="mt-auto p-3">
				<!-- The one number worth carrying on every screen: how many values
				     the broker would recognise on the way out right now. -->
				<div
					class="bg-card rounded-xl p-3.5 shadow-sm"
					title="valeurs du coffre que douane reconnaîtrait dans un corps partant"
				>
					<div class="flex items-baseline gap-2">
						<span class="ledger text-2xl leading-none">{count(config?.watching ?? 0)}</span>
						<span class="bg-trust-max relative flex size-2 shrink-0 rounded-full">
							<span
								class="bg-trust-max absolute inline-flex size-full animate-ping rounded-full opacity-60"
							></span>
						</span>
					</div>
					<p class="text-muted-foreground mt-1 text-xs leading-snug">
						valeurs surveillées en sortie
					</p>
				</div>

				<div class="text-muted-foreground mt-3 flex items-center justify-between px-1 text-xs">
					<span>Proxy de sortie</span>
					<span class="font-medium">
						{#if config?.egress.listening}
							à l'écoute
						{:else}
							éteint
						{/if}
					</span>
				</div>
			</div>
		</aside>

		<div class="lg:pl-64">
			<header class="bg-background/85 sticky top-0 z-30 border-b backdrop-blur-md">
				<div class="flex items-center gap-3 px-4 py-3 sm:px-6">
					<!-- On a phone the rail is gone, so the name moves up here. -->
					<span class="wordmark text-foreground lg:hidden">douane</span>

					<div class="hidden min-w-0 lg:block">
						<h1 class="text-base leading-tight font-semibold">{screen.label}</h1>
						<p class="text-muted-foreground truncate text-xs">{screen.desc}</p>
					</div>

					<div class="ml-auto flex shrink-0 items-center gap-2">
						{#if tab === 'trafic'}
							<Select.Root type="single" bind:value={hours}>
								<Select.Trigger class="h-9 w-[9.5rem]">
									{WINDOWS.find((w) => w.value === hours)?.label}
								</Select.Trigger>
								<Select.Content>
									{#each WINDOWS as w (w.value)}
										<Select.Item value={w.value} label={w.label}>{w.label}</Select.Item>
									{/each}
								</Select.Content>
							</Select.Root>
						{/if}

						<Button variant="outline" size="sm" class="h-9" onclick={load} disabled={loading}>
							<RefreshCw class="size-4 {loading ? 'animate-spin' : ''}" />
							<span class="hidden sm:inline">Recharger</span>
						</Button>

						<Button
							variant="ghost"
							size="icon"
							class="size-9"
							onclick={toggleTheme}
							title="thème clair ou sombre"
						>
							{#if dark}<Sun class="size-4" />{:else}<Moon class="size-4" />{/if}
						</Button>

						<Button
							variant="ghost"
							size="icon"
							class="size-9"
							onclick={() => session.lock()}
							title="oublier le jeton dans cet onglet"
						>
							<LogOut class="size-4" />
						</Button>
					</div>
				</div>

				<!-- The screens as a scrolling row, phone only. -->
				<nav
					class="bg-sidebar flex gap-1 overflow-x-auto border-t px-2 py-2 [scrollbar-width:none] lg:hidden [&::-webkit-scrollbar]:hidden"
				>
					{@render navItems(true)}
				</nav>
			</header>

			<main class="mx-auto w-full max-w-[1400px] p-4 sm:p-6">
				{#if failure}
					<p
						class="border-destructive/40 bg-destructive/10 text-destructive mb-4 rounded-xl border px-4 py-3 text-sm font-medium"
					>
						{failure}
					</p>
				{/if}

				<!-- The screen's own name and line, said once, on a phone where the
				     topbar had no room for it. -->
				<div class="mb-4 lg:hidden">
					<h1 class="text-lg leading-tight font-semibold">{screen.label}</h1>
					<p class="text-muted-foreground text-sm">{screen.desc}</p>
				</div>

				<!-- Each screen slides in a few pixels. Enough to say the page
				     changed, short enough that nobody waits for it. -->
				{#key tab}
					<div in:fly={{ y: 8, duration: 220, easing: cubicOut }}>
						{#if tab === 'agents'}
							<TabAgents {live} {catalog} bind:seconds={idle} {openThread} />
						{:else if tab === 'protection'}
							<!-- The verdict, before any number: the boom is up or it is not. -->
							<Card.Root class="shadow-panel mb-5 overflow-hidden">
								<div class="flex flex-col gap-5 p-6 sm:flex-row sm:items-center sm:gap-7">
									<span class="{stopped === 0 ? 'text-trust-max' : 'text-primary'} shrink-0">
										{@render barrier(stopped === 0, 'h-24')}
									</span>
									<div class="min-w-0">
										<h2 class="text-xl leading-tight font-semibold sm:text-2xl">
											{#if stopped === 0}
												Rien n'a été arrêté
											{:else}
												<!-- Not "bloquées": most of these are substitutions, where
												     the call went through with the value replaced by its
												     name. Saying blocked would misreport the verdict. -->
												{count(stopped)}
												{stopped > 1 ? 'fuites arrêtées' : 'fuite arrêtée'}
											{/if}
										</h2>
										<p class="text-muted-foreground mt-1.5 text-sm leading-relaxed">
											{#if stopped === 0}
												Aucune valeur du coffre n'a été vue dans un corps partant. Le registre
												ci-dessous liste tout ce que douane a regardé passer.
											{:else}
												Une valeur du coffre partait vers un fournisseur. Elle a été remplacée par
												son nom ou l'appel a été refusé, selon la règle du niveau de confiance.
											{/if}
										</p>
									</div>
								</div>
							</Card.Root>
							<TabProtection {alerts} {grants} watching={config?.watching ?? 0} reload={load} />
						{:else if tab === 'trafic'}
							<TabTraffic {summary} hours={Number(hours)} {calls} {threads} bind:filter={thread} />
						{:else if tab === 'modeles'}
							<TabModels {catalog} reload={load} />
						{:else if tab === 'routes'}
							<TabRoutes {config} reload={load} />
						{:else if tab === 'coffre'}
							<TabVault {vault} reload={load} />
						{/if}
					</div>
				{/key}
			</main>
		</div>
	</div>
{/if}
