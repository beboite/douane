<script lang="ts">
	import * as Card from '$lib/components/ui/card';
	import { cn } from '$lib/utils';
	import type { Snippet } from 'svelte';

	let {
		title,
		note,
		actions,
		children,
		flush = false,
		cap = '',
		class: className = ''
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
		class?: string;
	} = $props();
</script>

<Card.Root class={cn('shadow-panel gap-0 overflow-hidden py-0', className)}>
	<Card.Header
		class="flex flex-row items-center justify-between gap-4 space-y-0 border-b px-4 py-3 [.border-b]:pb-3"
	>
		<div class="min-w-0">
			<Card.Title class="text-sm leading-tight font-semibold">{title}</Card.Title>
			{#if note}
				<Card.Description class="mt-1 text-xs leading-snug">{note}</Card.Description>
			{/if}
		</div>
		{@render actions?.()}
	</Card.Header>
	<Card.Content
		class={cn(
			flush ? 'overflow-x-auto p-0' : 'p-4',
			cap && `${cap} overflow-y-auto`
		)}
	>
		{@render children()}
	</Card.Content>
</Card.Root>
