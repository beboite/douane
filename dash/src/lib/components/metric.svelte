<script lang="ts">
	import { Tween } from 'svelte/motion';
	import { cubicOut } from 'svelte/easing';
	import type { Component } from 'svelte';

	/** One headline number.
	 *
	 *  Numbers count up to their value rather than appearing, which is the only
	 *  reliable way to notice that a reload changed one: a figure that swaps in
	 *  place while you are looking somewhere else on the page changes silently.
	 *  Anything that is not a single number (a cost in two currencies) is passed
	 *  as `text` and simply rendered, since counting two units at once would
	 *  suggest they can be added. */
	let {
		label,
		value,
		text,
		format = (n: number) => String(Math.round(n)),
		hint,
		icon,
		tone = 'plain'
	}: {
		label: string;
		value?: number;
		text?: string;
		format?: (n: number) => string;
		hint?: string;
		icon?: Component<{ class?: string }>;
		tone?: 'plain' | 'accent' | 'warn' | 'quiet';
	} = $props();

	const Icon = $derived(icon);

	const counted = new Tween(0, { duration: 700, easing: cubicOut });
	$effect(() => {
		counted.set(value ?? 0);
	});

	const toneClass = {
		plain: 'text-foreground',
		accent: 'text-primary',
		warn: 'text-trust-low',
		quiet: 'text-muted-foreground'
	} as const;

	/* A cost in two currencies is three times as long as a call count and would
	   otherwise run into the divider. It steps down rather than wrapping, since
	   a headline figure on two lines stops being a headline. */
	const shown = $derived(text ?? format(counted.current));
	const size = $derived(
		shown.length > 22 ? 'text-lg' : shown.length > 14 ? 'text-xl' : 'text-[1.75rem]'
	);
</script>

<div class="min-w-0 px-4 py-3.5 sm:px-5">
	<div class="flex items-center gap-1.5">
		{#if Icon}
			<Icon class="text-muted-foreground/70 size-3.5 shrink-0" />
		{/if}
		<span class="eyebrow truncate">{label}</span>
	</div>
	<div
		class="tabular mt-1.5 truncate leading-none font-semibold tracking-tight {size} {toneClass[
			tone
		]}"
	>
		{shown}
	</div>
	{#if hint}
		<div class="text-muted-foreground mt-1.5 truncate text-xs" title={hint}>{hint}</div>
	{/if}
</div>
