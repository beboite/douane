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
			<li class="relative px-4 py-2.5">
				<!-- The bar is the row's background, so the ranking is legible before
				     a single digit is. -->
				<div
					class="bg-primary/10 absolute inset-y-1 left-1 rounded-md transition-[width] duration-700 ease-out"
					style="width: calc({(row.calls / top) * 100}% - 0.5rem)"
				></div>

				<div class="relative flex items-baseline gap-3">
					<span class="min-w-0 flex-1 truncate text-sm font-medium">{row.key}</span>

					{#if row.errors > 0}
						<span class="text-trust-low shrink-0 text-xs font-medium">
							{count(row.errors)} en échec
						</span>
					{/if}

					<span class="tabular text-muted-foreground hidden shrink-0 text-xs sm:inline">
						{tokens(row.input_tokens + row.output_tokens)} tok
					</span>
					<!-- Next to the token count on purpose: the tokens say how much went
					     through, this says how much of it was paid for twice. -->
					<span class="hidden sm:inline">
						<CacheBadge read={row.cache_read_tokens} write={row.cache_write_tokens} />
					</span>
					<span class="tabular shrink-0 text-sm font-medium">{count(row.calls)}</span>
				</div>
			</li>
		{/each}
	</ul>
{/if}
