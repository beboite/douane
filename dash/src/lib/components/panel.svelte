<script lang="ts">
	import type { Snippet } from 'svelte';

	let {
		title,
		note,
		actions,
		children,
		flush = false,
		cap = ''
	}: {
		title: string;
		note?: string;
		actions?: Snippet;
		children: Snippet;
		/** Tables sit flush against the border; anything else gets padding. */
		flush?: boolean;
		/** A tailwind max-height for a table long enough to bury whatever comes
		 *  after it. Scrolls inside itself rather than down the page. */
		cap?: string;
	} = $props();
</script>

<section class="bg-card overflow-hidden rounded-lg border">
	<header class="flex items-center justify-between gap-4 border-b px-4 py-3">
		<div class="min-w-0">
			<h2 class="text-sm font-semibold">{title}</h2>
			{#if note}
				<p class="text-muted-foreground mt-0.5 truncate text-xs">{note}</p>
			{/if}
		</div>
		{@render actions?.()}
	</header>
	<div class="{flush ? 'overflow-x-auto' : 'p-4'} {cap ? `${cap} overflow-y-auto` : ''}">
		{@render children()}
	</div>
</section>
