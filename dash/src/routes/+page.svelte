<script lang="ts">
	import { fly } from 'svelte/transition';
	import { cubicOut } from 'svelte/easing';
	import { Badge } from '$lib/components/ui/badge';
	import { Button } from '$lib/components/ui/button';
	import * as Card from '$lib/components/ui/card';
	import { Input } from '$lib/components/ui/input';
	import { Label } from '$lib/components/ui/label';
	import * as Select from '$lib/components/ui/select';
	import * as Tabs from '$lib/components/ui/tabs';
	import { Toaster } from '$lib/components/ui/sonner';
	import Activity from '@lucide/svelte/icons/activity';
	import KeyRound from '@lucide/svelte/icons/key-round';
	import LayoutDashboard from '@lucide/svelte/icons/layout-dashboard';
	import LockKeyhole from '@lucide/svelte/icons/lock-keyhole';
	import Moon from '@lucide/svelte/icons/moon';
	import RefreshCw from '@lucide/svelte/icons/refresh-cw';
	import ShieldAlert from '@lucide/svelte/icons/shield-alert';
	import Signpost from '@lucide/svelte/icons/signpost';
	import Sun from '@lucide/svelte/icons/sun';
	import Vault from '@lucide/svelte/icons/vault';

	import TabAlerts from '$lib/components/tab-alerts.svelte';
	import TabOverview from '$lib/components/tab-overview.svelte';
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
	let tab = $state('apercu');
	let typed = $state('');
	let rejected = $state('');

	let summary = $state<Summary | null>(null);
	let config = $state<ConfigView | null>(null);
	let vault = $state<VaultView | null>(null);
	let calls = $state<Call[]>([]);
	let threads = $state<Thread[]>([]);
	let alerts = $state<Alert[]>([]);
	let thread = $state<string | null>(null);

	let loading = $state(false);
	let failure = $state('');
	let dark = $state(true);

	const blocked = $derived(alerts.filter((a) => a.action === 'block').length);
	const repointed = $derived(config?.routes.filter((r) => r.overridden).length ?? 0);

	async function load() {
		if (!session.unlocked) return;
		loading = true;
		failure = '';
		const h = Number(hours);
		try {
			const [s, c, v, k, t, a] = await Promise.all([
				api.summary(session.token, h),
				api.config(session.token),
				api.vault(session.token),
				api.calls(session.token, 200, { thread: thread ?? undefined }),
				api.threads(session.token, h),
				api.alerts(session.token)
			]);
			summary = s.summary;
			config = c;
			vault = v;
			calls = k;
			threads = t;
			alerts = a;
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

{#if !session.unlocked}
	<main class="relative flex min-h-svh items-center justify-center overflow-hidden p-6">
		<!-- A single soft light behind the card. The page a credential is typed
		     into should not look like a form on a wall. -->
		<div
			class="bg-primary/10 pointer-events-none absolute top-1/2 left-1/2 size-[22rem] -translate-x-1/2 -translate-y-[65%] rounded-full blur-[90px]"
		></div>

		<Card.Root class="shadow-lifted relative w-full max-w-sm">
			<!-- The gap lives on the form, not on the card: a single child absorbs
			     the card's own `flex-col gap`, and the header ends up glued to the
			     field under it. -->
			<form onsubmit={unlock} class="flex flex-col gap-6">
				<Card.Header>
					<div class="flex items-center gap-3">
						<div
							class="bg-primary text-primary-foreground flex size-10 shrink-0 items-center justify-center rounded-xl"
						>
							<LockKeyhole class="size-5" />
						</div>
						<div>
							<Card.Title class="text-lg leading-tight">douane</Card.Title>
							<Card.Description class="text-xs">le tableau de bord du broker</Card.Description>
						</div>
					</div>
				</Card.Header>

				<Card.Content class="grid gap-2">
					<Label for="token">jeton d’administration</Label>
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
						ouvrir
					</Button>

					<p class="text-muted-foreground mt-2 text-xs leading-relaxed">
						Il vit dans le coffre sous <code class="font-mono">douane.admin_token</code>, avec une
						copie locale dans <code class="font-mono">%APPDATA%\douane\admin_token.key</code>. Il
						reste dans cet onglet et disparaît à sa fermeture.
					</p>
				</Card.Content>
			</form>
		</Card.Root>
	</main>
{:else}
	<!-- Declared out here rather than inside `Tabs.List`: a snippet written in a
	     component's children is read as a prop of that component. -->
	{#snippet item(value: string, label: string, Icon: typeof Activity, badge?: string)}
		<Tabs.Trigger
			{value}
			class="data-[state=active]:border-primary data-[state=active]:text-foreground text-muted-foreground gap-1.5 border-0 border-b-2 border-transparent px-3 py-2.5 text-sm shadow-none data-[state=active]:bg-transparent data-[state=active]:shadow-none dark:data-[state=active]:bg-transparent"
		>
			<Icon class="size-4" />
			{label}
			{#if badge}
				<Badge
					variant="secondary"
					class="ml-0.5 rounded-full px-1.5 py-0 text-[0.65rem] font-semibold"
				>
					{badge}
				</Badge>
			{/if}
		</Tabs.Trigger>
	{/snippet}

	<div class="min-h-svh">
		<header class="bg-background/85 sticky top-0 z-30 border-b backdrop-blur-md">
			<div class="mx-auto flex max-w-[1500px] items-center gap-3 px-4 py-2.5 sm:px-6">
				<div class="flex min-w-0 items-center gap-2.5">
					<div
						class="bg-primary text-primary-foreground flex size-8 shrink-0 items-center justify-center rounded-lg"
					>
						<LockKeyhole class="size-4" />
					</div>
					<span class="text-sm font-semibold">douane</span>
					<!-- The one number worth carrying on every screen: how many values
					     the broker would recognise on the way out right now. -->
					<Badge
						variant="secondary"
						class="hidden gap-1.5 rounded-full py-0 pr-2.5 pl-2 text-[0.7rem] font-normal sm:inline-flex"
						title="valeurs du coffre que le scrubber reconnaîtrait dans un corps partant"
					>
						<span class="bg-trust-max relative flex size-1.5 rounded-full">
							<span class="bg-trust-max absolute inline-flex size-full animate-ping rounded-full opacity-60"
							></span>
						</span>
						{count(config?.watching ?? 0)} valeurs surveillées
					</Badge>
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
		</header>

		<Tabs.Root bind:value={tab}>
			<!-- The nav sits in its own band under the header rather than floating
			     inside the content. Five screens is an app, not a widget. -->
			<div class="bg-background/85 sticky top-[3.3rem] z-20 border-b backdrop-blur-md">
				<div
					class="mx-auto w-full max-w-[1500px] overflow-x-auto px-4 [scrollbar-width:none] sm:px-6 [&::-webkit-scrollbar]:hidden"
				>
					<Tabs.List
						class="h-auto gap-1 rounded-none border-0 bg-transparent p-0 [&>*]:rounded-none"
					>
						{@render item('apercu', 'Aperçu', LayoutDashboard)}
						{@render item('trafic', 'Trafic', Activity)}
						{@render item('routes', 'Routes', Signpost, repointed > 0 ? String(repointed) : undefined)}
						{@render item('coffre', 'Coffre', Vault, count(vault?.entries.length ?? 0) || undefined)}
						{@render item('alertes', 'Alertes', ShieldAlert, blocked > 0 ? String(blocked) : undefined)}
					</Tabs.List>
				</div>
			</div>

			<!-- `w-full` is load bearing: `mx-auto` on a flex child cancels the
			     stretch, and `Tabs.Root` is a flex column, so without it the whole
			     page shrinks to the width of its widest table. -->
			<main class="mx-auto w-full max-w-[1500px] p-4 sm:p-6">
				{#if failure}
					<p
						class="border-destructive/40 bg-destructive/10 text-destructive mb-4 rounded-lg border px-3 py-2 text-sm"
					>
						{failure}
					</p>
				{/if}

				<!-- Each screen slides in a few pixels. Enough to say the page
				     changed, short enough that nobody waits for it. -->
				<Tabs.Content value="apercu">
					<div in:fly={{ y: 8, duration: 220, easing: cubicOut }}>
						<TabOverview {summary} {config} {alerts} hours={Number(hours)} />
					</div>
				</Tabs.Content>

				<Tabs.Content value="trafic">
					<div in:fly={{ y: 8, duration: 220, easing: cubicOut }}>
						<TabTraffic {calls} {threads} bind:filter={thread} />
					</div>
				</Tabs.Content>

				<Tabs.Content value="routes">
					<div in:fly={{ y: 8, duration: 220, easing: cubicOut }}>
						<TabRoutes {config} reload={load} />
					</div>
				</Tabs.Content>

				<Tabs.Content value="coffre">
					<div in:fly={{ y: 8, duration: 220, easing: cubicOut }}>
						<TabVault {vault} reload={load} />
					</div>
				</Tabs.Content>

				<Tabs.Content value="alertes">
					<div in:fly={{ y: 8, duration: 220, easing: cubicOut }}>
						<TabAlerts {alerts} />
					</div>
				</Tabs.Content>
			</main>
		</Tabs.Root>
	</div>
{/if}
