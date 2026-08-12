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

<!-- An empty register is drawn as an empty case on the form: a dashed frame,
     nothing inside it, and one line saying why. -->
<div class="p-4">
	<div
		class="border-border/80 text-muted-foreground flex flex-col items-center justify-center gap-2 rounded-md border border-dashed text-center {tight
			? 'py-7'
			: 'py-12'}"
	>
		<Icon class="text-muted-foreground/60 size-4" />
		<p class="text-foreground/80 text-sm font-medium">{title}</p>
		{#if hint}
			<p class="max-w-xs text-xs leading-relaxed">{hint}</p>
		{/if}
	</div>
</div>
