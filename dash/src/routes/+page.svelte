<script lang="ts">
	import { Button } from '$lib/components/ui/button';
	import { Input } from '$lib/components/ui/input';
	import { Label } from '$lib/components/ui/label';
	import * as Select from '$lib/components/ui/select';
	import * as Tabs from '$lib/components/ui/tabs';
	import { Toaster } from '$lib/components/ui/sonner';
	import KeyRound from '@lucide/svelte/icons/key-round';
	import LockKeyhole from '@lucide/svelte/icons/lock-keyhole';
	import Moon from '@lucide/svelte/icons/moon';
	import RefreshCw from '@lucide/svelte/icons/refresh-cw';
	import Sun from '@lucide/svelte/icons/sun';

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
	<main class="flex min-h-svh items-center justify-center p-6">
		<form onsubmit={unlock} class="bg-card w-full max-w-sm rounded-lg border p-6">
			<div class="flex items-center gap-2.5">
				<div class="bg-primary/10 text-primary flex size-9 items-center justify-center rounded-md">
					<LockKeyhole class="size-4.5" />
				</div>
				<div>
					<h1 class="leading-tight font-semibold">douane</h1>
					<p class="text-muted-foreground text-xs">le tableau de bord du broker</p>
				</div>
			</div>

			<div class="mt-6 grid gap-2">
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
			</div>

			<Button type="submit" class="mt-4 w-full" disabled={typed.trim().length === 0}>
				<KeyRound class="size-4" />
				ouvrir
			</Button>

			<p class="text-muted-foreground mt-4 text-xs leading-relaxed">
				Il vit dans le coffre sous <code class="font-mono">douane.admin_token</code>, avec une copie
				locale dans <code class="font-mono">%APPDATA%\douane\admin_token.key</code>. Il reste dans
				cet onglet et disparaît à sa fermeture.
			</p>
		</form>
	</main>
{:else}
	<div class="min-h-svh">
		<header class="bg-background/80 sticky top-0 z-30 border-b backdrop-blur">
			<div class="mx-auto flex max-w-[1600px] items-center gap-3 px-4 py-2.5 sm:px-6">
				<div class="flex items-center gap-2.5">
					<div
						class="bg-primary/10 text-primary flex size-8 items-center justify-center rounded-md"
					>
						<LockKeyhole class="size-4" />
					</div>
					<div class="leading-tight">
						<div class="text-sm font-semibold">douane</div>
						<div class="text-muted-foreground text-[0.7rem]">
							{count(config?.watching ?? 0)} valeurs surveillées en sortie
						</div>
					</div>
				</div>

				<div class="ml-auto flex items-center gap-2">
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

		<main class="mx-auto max-w-[1600px] p-4 sm:p-6">
			{#if failure}
				<p
					class="border-destructive/40 bg-destructive/10 text-destructive mb-4 rounded-md border px-3 py-2 text-sm"
				>
					{failure}
				</p>
			{/if}

			<Tabs.Root bind:value={tab}>
				<Tabs.List>
					<Tabs.Trigger value="apercu">Aperçu</Tabs.Trigger>
					<Tabs.Trigger value="trafic">Trafic</Tabs.Trigger>
					<Tabs.Trigger value="routes">
						Routes
						{#if config?.routes.some((r) => r.overridden)}
							<span class="bg-primary ml-1.5 size-1.5 rounded-full"></span>
						{/if}
					</Tabs.Trigger>
					<Tabs.Trigger value="coffre">Coffre</Tabs.Trigger>
					<Tabs.Trigger value="alertes">
						Alertes
						{#if blocked > 0}
							<span
								class="bg-destructive/15 text-destructive ml-1.5 rounded-full px-1.5 text-[0.65rem] font-semibold"
							>
								{blocked}
							</span>
						{/if}
					</Tabs.Trigger>
				</Tabs.List>

				<Tabs.Content value="apercu" class="mt-4">
					<TabOverview {summary} {config} {alerts} hours={Number(hours)} />
				</Tabs.Content>

				<Tabs.Content value="trafic" class="mt-4">
					<TabTraffic {calls} {threads} bind:filter={thread} />
				</Tabs.Content>

				<Tabs.Content value="routes" class="mt-4">
					<TabRoutes {config} reload={load} />
				</Tabs.Content>

				<Tabs.Content value="coffre" class="mt-4">
					<TabVault {vault} reload={load} />
				</Tabs.Content>

				<Tabs.Content value="alertes" class="mt-4">
					<TabAlerts {alerts} />
				</Tabs.Content>
			</Tabs.Root>
		</main>
	</div>
{/if}
