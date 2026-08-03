<script lang="ts">
	import type { Trust } from '$lib/api';

	let { trust, class: className = '' }: { trust: Trust | null; class?: string } = $props();

	/* The axis the whole project turns on, so it says what it means rather than
	   repeating the three letters the API sends. */
	const label: Record<string, string> = {
		max: 'machine à moi',
		mid: 'éditeur sous contrat',
		low: 'revendeur'
	};

	const tone: Record<string, string> = {
		max: 'border-trust-max/40 text-trust-max bg-trust-max/10',
		mid: 'border-trust-mid/40 text-trust-mid bg-trust-mid/10',
		low: 'border-trust-low/40 text-trust-low bg-trust-low/10'
	};
</script>

{#if trust}
	<span
		class="inline-flex shrink-0 items-center gap-1.5 rounded-full border px-2 py-0.5 text-xs font-medium {tone[
			trust
		] ?? 'border-border text-muted-foreground'} {className}"
		title={label[trust] ?? trust}
	>
		<span class="size-1.5 rounded-full bg-current"></span>
		{trust}
	</span>
{/if}
