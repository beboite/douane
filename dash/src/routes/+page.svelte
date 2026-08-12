<script lang="ts">
	import { fly } from 'svelte/transition';
	import { cubicOut } from 'svelte/easing';
	import { Badge } from '$lib/components/ui/badge';
	import { Button } from '$lib/components/ui/button';
	import * as Card from '$lib/components/ui/card';
	import { Input } from '$lib/components/ui/input';
	import { Label } from '$lib/components/ui/label';
	import * as Select from '$lib/components/ui/select';
	import { Toaster } from '$lib/components/ui/sonner';
	import Activity from '@lucide/svelte/icons/activity';
	import KeyRound from '@lucide/svelte/icons/key-round';
	import Moon from '@lucide/svelte/icons/moon';
	import RefreshCw from '@lucide/svelte/icons/refresh-cw';
	import ShieldCheck from '@lucide/svelte/icons/shield-check';
	import Signpost from '@lucide/svelte/icons/signpost';
	import Sun from '@lucide/svelte/icons/sun';
	import Vault from '@lucide/svelte/icons/vault';

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
		type ConfigView,
		type Grant,
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
	let tab = $state('protection');
	let typed = $state('');
	let rejected = $state('');

	let summary = $state<Summary | null>(null);
	let config = $state<ConfigView | null>(null);
	let vault = $state<VaultView | null>(null);
	let calls = $state<Call[]>([]);
	let threads = $state<Thread[]>([]);
	let alerts = $state<Alert[]>([]);
	let grants = $state<Grant[]>([]);
	let thread = $state<string | null>(null);

	let loading = $state(false);
	let failure = $state('');
	let dark = $state(true);

	const pending = $derived(grants.filter((g) => g.status === 'pending').length);
	const repointed = $derived(config?.routes.filter((r) => r.overridden).length ?? 0);

	/* Each screen names itself and says in one line what it answers. The line
	   lives here, in the header, so the panels below can keep their titles
	   short. */
	const SCREENS: Record<string, { label: string; icon: typeof Activity; desc: string }> = {
		protection: {
			label: 'Protection',
			icon: ShieldCheck,
			desc: 'ce que le broker a arrêté avant que ça parte, et qui attend une décision'
		},
		trafic: {
			label: 'Trafic',
			icon: Activity,
			desc: 'ce qui est passé par le guichet sur la fenêtre choisie'
		},
		routes: {
			label: 'Routes',
			icon: Signpost,
			desc: 'qui reçoit vraiment chaque nom de modèle, et si les clés marchent encore'
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
			const [s, c, v, k, t, a, g] = await Promise.all([
				api.summary(session.token, h),
				api.config(session.token),
				api.vault(session.token),
				api.calls(session.token, 200, { thread: thread ?? undefined }),
				api.threads(session.token, h),
				api.alerts(session.token),
				api.grants(session.token)
			]);
			summary = s.summary;
			config = c;
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
</script>

<Toaster position="bottom-right" />

<!-- The round stamp of the office, drawn once and used wherever the office
     signs: the login page, and the corner of the rail. Two rings and the name
     run around them, the way a customs seal is engraved. -->
{#snippet seal(cls: string)}
	<svg viewBox="0 0 200 200" class={cls} aria-hidden="true">
		<defs>
			<path id="seal-ring" d="M100,100 m-77,0 a77,77 0 1,1 154,0 a77,77 0 1,1 -154,0" />
		</defs>
		<circle cx="100" cy="100" r="94" fill="none" stroke="currentcolor" stroke-width="3" />
		<circle cx="100" cy="100" r="60" fill="none" stroke="currentcolor" stroke-width="1.5" />
		<text
			fill="currentcolor"
			font-family="IBM Plex Mono, monospace"
			font-size="13.5"
			font-weight="600"
			letter-spacing="5"
		>
			<textPath href="#seal-ring">DOUANE · CONTRÔLE DES SORTIES · </textPath>
		</text>
		<text
			x="100"
			y="106"
			text-anchor="middle"
			fill="currentcolor"
			font-family="IBM Plex Mono, monospace"
			font-size="15"
			font-weight="600"
			letter-spacing="2"
		>
			N° 8787
		</text>
	</svg>
{/snippet}

{#snippet navBadge(n: number, alarm: boolean)}
	{#if n > 0}
		<span
			class="ml-auto inline-flex h-5 min-w-5 items-center justify-center rounded-full px-1.5 font-mono text-[0.65rem] font-semibold {alarm
				? 'bg-destructive text-destructive-foreground'
				: 'bg-sidebar-accent text-sidebar-accent-foreground'}"
		>
			{count(n)}
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
			class="flex items-center gap-2.5 rounded-md text-sm transition-colors {compact
				? 'shrink-0 px-3 py-2'
				: 'w-full px-3 py-2'} {active
				? 'bg-sidebar-accent text-sidebar-accent-foreground font-medium'
				: 'text-sidebar-foreground/70 hover:bg-sidebar-accent/60 hover:text-sidebar-foreground'}"
		>
			<ScreenIcon class="size-4 shrink-0 {active ? 'text-primary' : ''}" />
			{s.label}
			{#if id === 'protection'}{@render navBadge(pending, true)}
			{:else if id === 'routes'}{@render navBadge(repointed, false)}
			{:else if id === 'coffre'}{@render navBadge(vault?.entries.length ?? 0, false)}{/if}
		</button>
	{/each}
{/snippet}

{#if !session.unlocked}
	<main class="relative flex min-h-svh items-center justify-center overflow-hidden p-6">
		<div class="relative w-full max-w-sm">
			<!-- The seal leans over the top corner of the card, half on the paper
			     and half on the form, the way a stamp actually lands: over the
			     sheet, never under it. -->
			<div
				class="text-primary/30 dark:text-primary/35 pointer-events-none absolute -top-14 -right-9 z-10 size-36 -rotate-12"
			>
				{@render seal('size-full')}
			</div>

			<Card.Root class="shadow-lifted relative">
				<!-- The gap lives on the form, not on the card: a single child absorbs
				     the card's own `flex-col gap`, and the header ends up glued to the
				     field under it. -->
				<form onsubmit={unlock} class="flex flex-col gap-6">
					<Card.Header>
						<div class="wordmark text-primary">douane</div>
						<Card.Description class="mt-1 text-xs">
							contrôle des sorties · le guichet n'ouvre qu'au porteur du jeton
						</Card.Description>
					</Card.Header>

					<Card.Content class="grid gap-2">
						<Label for="token">jeton d'administration</Label>
						<Input
							id="token"
							type="password"
							bind:value={typed}
							placeholder="collez le jeton"
							autocomplete="off"
							spellcheck="false"
							class="font-mono"
						/>
						{#if rejected}
							<p class="text-destructive text-xs">{rejected}</p>
						{/if}

						<Button type="submit" class="mt-2 w-full" disabled={typed.trim().length === 0}>
							<KeyRound class="size-4" />
							ouvrir le guichet
						</Button>

						<p class="text-muted-foreground mt-2 text-xs leading-relaxed">
							Il vit dans le coffre sous <code class="font-mono">douane.admin_token</code>, avec une
							copie locale dans <code class="font-mono">%APPDATA%\douane\admin_token.key</code>. Il
							reste dans cet onglet et disparaît à sa fermeture.
						</p>
					</Card.Content>
				</form>
			</Card.Root>
		</div>
	</main>
{:else}
	{@const screen = SCREENS[tab]}
	<div class="min-h-svh">
		<!-- The rail. Always there on a desk-sized screen: the four registers, and
		     at the bottom the one number the office exists for. -->
		<aside
			class="bg-sidebar border-sidebar-border fixed inset-y-0 left-0 z-40 hidden w-60 flex-col border-r lg:flex"
		>
			<div class="border-sidebar-border flex items-center gap-3 border-b px-5 py-5">
				<div class="text-primary/70 size-9 shrink-0">
					{@render seal('size-full')}
				</div>
				<div class="min-w-0">
					<div class="wordmark text-sidebar-foreground">douane</div>
					<div class="text-muted-foreground mt-0.5 text-[0.65rem] tracking-wide">
						contrôle des sorties
					</div>
				</div>
			</div>

			<nav class="flex flex-col gap-1 p-3">
				{@render navItems(false)}
			</nav>

			<div class="mt-auto">
				<!-- The one number worth carrying on every screen: how many values
				     the broker would recognise on the way out right now. -->
				<div
					class="border-sidebar-border mx-3 mb-3 rounded-md border px-3 py-2.5"
					title="valeurs du coffre que le scrubber reconnaîtrait dans un corps partant"
				>
					<div class="flex items-center gap-2">
						<span class="bg-trust-max relative flex size-1.5 shrink-0 rounded-full">
							<span
								class="bg-trust-max absolute inline-flex size-full animate-ping rounded-full opacity-60"
							></span>
						</span>
						<span class="ledger text-base leading-none">{count(config?.watching ?? 0)}</span>
						<span class="text-muted-foreground text-[0.7rem] leading-tight"
							>valeurs surveillées en sortie</span
						>
					</div>
				</div>

				<div
					class="border-sidebar-border text-muted-foreground flex items-center justify-between border-t px-5 py-3 text-[0.7rem]"
				>
					<span>proxy de sortie</span>
					<span>
						{#if config?.egress.listening}
							à l'écoute · {config.egress.rules} règles
						{:else}
							éteint
						{/if}
					</span>
				</div>
			</div>
		</aside>

		<div class="lg:pl-60">
			<header class="bg-background/85 sticky top-0 z-30 border-b backdrop-blur-md">
				<div class="flex items-center gap-3 px-4 py-2.5 sm:px-6">
					<!-- On a phone the rail is gone, so the plaque moves up here. -->
					<span class="wordmark text-primary lg:hidden">douane</span>

					<div class="hidden min-w-0 lg:block">
						<h1 class="text-sm leading-tight font-semibold">{screen.label}</h1>
						<p class="text-muted-foreground truncate text-xs">{screen.desc}</p>
					</div>

					<div class="ml-auto flex shrink-0 items-center gap-1.5">
						<Select.Root type="single" bind:value={hours}>
							<Select.Trigger class="h-8 w-[9.5rem]">
								{WINDOWS.find((w) => w.value === hours)?.label}
							</Select.Trigger>
							<Select.Content>
								{#each WINDOWS as w (w.value)}
									<Select.Item value={w.value} label={w.label}>{w.label}</Select.Item>
								{/each}
							</Select.Content>
						</Select.Root>

						<Button
							variant="ghost"
							size="icon"
							class="size-8"
							onclick={load}
							disabled={loading}
							title="recharger"
						>
							<RefreshCw class="size-4 {loading ? 'animate-spin' : ''}" />
						</Button>

						<Button
							variant="ghost"
							size="icon"
							class="size-8"
							onclick={toggleTheme}
							title="thème clair ou sombre"
						>
							{#if dark}<Sun class="size-4" />{:else}<Moon class="size-4" />{/if}
						</Button>

						<Button
							variant="ghost"
							size="sm"
							class="h-8"
							onclick={() => session.lock()}
							title="oublier le jeton dans cet onglet"
						>
							fermer
						</Button>
					</div>
				</div>

				<!-- The four registers as a scrolling row, phone only. -->
				<nav
					class="bg-sidebar flex gap-1 overflow-x-auto border-t px-2 py-1.5 [scrollbar-width:none] lg:hidden [&::-webkit-scrollbar]:hidden"
				>
					{@render navItems(true)}
				</nav>
			</header>

			<main class="mx-auto w-full max-w-[1400px] p-4 sm:p-6">
				{#if failure}
					<p
						class="border-destructive/40 bg-destructive/10 text-destructive mb-4 rounded-lg border px-3 py-2 text-sm"
					>
						{failure}
					</p>
				{/if}

				<!-- The screen's own name and line, said once, on a phone where the
				     topbar had no room for it. -->
				<div class="mb-4 lg:hidden">
					<h1 class="text-base leading-tight font-semibold">{screen.label}</h1>
					<p class="text-muted-foreground text-xs">{screen.desc}</p>
				</div>

				<!-- Each screen slides in a few pixels. Enough to say the page
				     changed, short enough that nobody waits for it. -->
				{#key tab}
					<div in:fly={{ y: 8, duration: 220, easing: cubicOut }}>
						{#if tab === 'protection'}
							<TabProtection {alerts} {grants} watching={config?.watching ?? 0} reload={load} />
						{:else if tab === 'trafic'}
							<TabTraffic {summary} hours={Number(hours)} {calls} {threads} bind:filter={thread} />
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
