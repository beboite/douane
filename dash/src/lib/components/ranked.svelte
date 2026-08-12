<script lang="ts">
	import CacheBadge from './cache-badge.svelte';
	import Empty from './empty.svelte';
	import Inbox from '@lucide/svelte/icons/inbox';
	import type { Bucket } from '$lib/api';
	import { count, tokens } from '$lib/format';

	/** A breakdown as a ranked list rather than a table.
	 *
	 *  The five-column table this replaces made every row cost a left-to-right
	 *  read to answer "which one is the big one". A bar answers that before the
	 *  numbers are read at all, and the numbers are still there for when the
	 *  answer is not enough. */
	let { rows, empty }: { rows: Bucket[]; empty: string } = $props();

	const top = $derived(Math.max(1, ...rows.map((r) => r.calls)));
</script>

{#if rows.length === 0}
	<Empty icon={Inbox} title="rien sur cette fenêtre" hint={empty} tight />
{:else}
	<ul class="divide-border divide-y">
		{#each rows as row (row.key)}
			<li class="relative px-5 py-3">
				<!-- The bar is the row's background, so the ranking is legible before
				     a single digit is. -->
				<div
					class="bg-primary/10 absolute inset-y-1.5 left-2 rounded-lg transition-[width] duration-700 ease-out"
					style="width: calc({(row.calls / top) * 100}% - 1rem)"
				></div>

				<div class="relative flex items-center gap-3">
					<span class="min-w-0 flex-1 truncate text-sm font-medium">{row.key}</span>

					{#if row.errors > 0}
						<span class="text-destructive shrink-0 text-xs font-semibold">
							{count(row.errors)} en échec
						</span>
					{/if}

					<span class="tabular text-muted-foreground hidden shrink-0 text-xs sm:inline">
						{tokens(row.input_tokens + row.output_tokens)} tokens
					</span>
					<!-- Next to the token count on purpose: the tokens say how much went
					     through, this says how much of it was paid for twice. -->
					<span class="hidden shrink-0 sm:inline">
						<CacheBadge read={row.cache_read_tokens} write={row.cache_write_tokens} />
					</span>
					<span class="tabular w-12 shrink-0 text-right text-sm font-semibold">
						{count(row.calls)}
					</span>
				</div>
			</li>
		{/each}
	</ul>
{/if}
