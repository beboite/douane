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

<div
	class="text-muted-foreground flex flex-col items-center justify-center gap-2 text-center {tight
		? 'py-8'
		: 'py-14'}"
>
	<div class="bg-sunken text-muted-foreground/70 flex size-9 items-center justify-center rounded-full">
		<Icon class="size-4" />
	</div>
	<p class="text-foreground/80 text-sm font-medium">{title}</p>
	{#if hint}
		<p class="max-w-xs text-xs leading-relaxed">{hint}</p>
	{/if}
</div>
