<script lang="ts">
	import type { Snippet } from 'svelte';

	let {
		label,
		value,
		hint,
		tone = 'plain',
		children
	}: {
		label: string;
		value: string;
		hint?: string;
		tone?: 'plain' | 'accent' | 'warn';
		children?: Snippet;
	} = $props();

	const toneClass = {
		plain: 'text-foreground',
		accent: 'text-primary',
		warn: 'text-trust-low'
	} as const;
</script>

<div class="bg-card rounded-lg border p-4">
	<div class="text-muted-foreground text-xs font-medium tracking-wide uppercase">{label}</div>
	<div class="tabular mt-1.5 text-2xl leading-none font-semibold {toneClass[tone]}">
		{value || '—'}
	</div>
	{#if hint}
		<div class="text-muted-foreground mt-1.5 text-xs">{hint}</div>
	{/if}
	{@render children?.()}
</div>
