<script lang="ts">
	import type { Component } from 'svelte';

	/** What an empty table says instead of leaving a grey rectangle.
	 *
	 *  Every one of these carries a second line saying whether nothing happened
	 *  or nothing matched, because those look identical and mean opposite
	 *  things: one is a quiet broker, the other is a filter still on. */
	let {
		icon,
		title,
		hint,
		tight = false
	}: {
		icon: Component<{ class?: string }>;
		title: string;
		hint?: string;
		tight?: boolean;
	} = $props();

	const Icon = $derived(icon);
</script>

<!-- An empty screen is an invitation, not a grey rectangle: a soft panel, the
     icon in a circle, and one line saying what would fill it. -->
<div class="p-5">
	<div
		class="bg-sunken text-muted-foreground flex flex-col items-center justify-center gap-2.5 rounded-xl text-center {tight
			? 'py-8'
			: 'py-14'}"
	>
		<span class="bg-background flex size-9 items-center justify-center rounded-full shadow-sm">
			<Icon class="text-muted-foreground size-4" />
		</span>
		<p class="text-foreground text-sm font-semibold">{title}</p>
		{#if hint}
			<p class="max-w-sm px-4 text-[0.8rem] leading-relaxed">{hint}</p>
		{/if}
	</div>
</div>
