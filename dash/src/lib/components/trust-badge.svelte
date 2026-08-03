<script lang="ts">
	import { Badge } from '$lib/components/ui/badge';
	import { cn } from '$lib/utils';
	import type { Trust } from '$lib/api';

	let { trust, class: className = '' }: { trust: Trust | null; class?: string } = $props();

	/* The axis the whole project turns on, so the badge says what the level means
	   rather than repeating the three letters the API sends. "low" is only
	   readable to someone who already knows; "revendeur" is readable to anyone,
	   and it is the one that matters. */
	const label: Record<string, string> = {
		max: 'à moi',
		mid: 'éditeur',
		low: 'revendeur'
	};

	const explains: Record<string, string> = {
		max: 'une machine que le propriétaire tient, la conversation ne quitte pas le réseau',
		mid: 'un éditeur sous contrat, il lit ce qu’il veut mais il a une réputation à perdre',
		low: 'un revendeur, on part du principe qu’il stocke et relit chaque octet'
	};

	const tone: Record<string, string> = {
		max: 'border-trust-max/35 text-trust-max bg-trust-max/10',
		mid: 'border-trust-mid/35 text-trust-mid bg-trust-mid/10',
		low: 'border-trust-low/35 text-trust-low bg-trust-low/10'
	};
</script>

{#if trust}
	<Badge
		variant="outline"
		class={cn(
			'shrink-0 gap-1.5 rounded-full px-2 py-0 text-[0.7rem] font-medium',
			tone[trust] ?? 'text-muted-foreground',
			className
		)}
		title={explains[trust] ?? trust}
	>
		<span class="size-1.5 rounded-full bg-current"></span>
		{label[trust] ?? trust}
	</Badge>
{/if}
