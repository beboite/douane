<script lang="ts">
	import { tokens } from '$lib/format';

	/** How well an upstream held the prompt cache.
	 *
	 *  The figure is read tokens per written token. A pool that keeps a client on
	 *  the same account reads the same prefix back turn after turn and lands in
	 *  the tens; one that spreads the client over rotating accounts finds nothing
	 *  and writes the whole context again, which sits near one and is where a bill
	 *  goes when nothing else changed. Writing costs more than a plain input token
	 *  and reading costs far less, so the two are worth watching apart even though
	 *  they count the same tokens seen twice.
	 *
	 *  Nothing is drawn when neither number is there: a route whose upstream
	 *  reports no cache at all has no ratio, and a `×0` would read as a broken one
	 *  rather than an absent one. */
	let {
		read,
		write,
		compact = false
	}: { read: number; write: number; compact?: boolean } = $props();

	const ratio = $derived(write > 0 ? read / write : null);

	/* Said in words rather than as `×∞`, which reads like a division that went
	   wrong. Nothing was written on this window, so every read came off a prefix
	   the upstream already held. */
	const shown = $derived(
		ratio === null
			? 'que des relectures'
			: '×' +
					new Intl.NumberFormat('fr-FR', { maximumFractionDigits: ratio < 10 ? 1 : 0 }).format(
						ratio
					)
	);

	/* A prefix an upstream actually holds is read back on every turn of the
	   conversation, so a working route sits in the tens. Three is where it stops
	   being a slow session and starts being a prefix that survives a turn or two
	   before it has to be written again, which is a state a token total hides
	   entirely. One is far too low a mark: a pool rewriting the whole context
	   every third call still scores above it. */
	const poor = $derived(ratio !== null && ratio < 3);

	const title = $derived(
		`${tokens(write)} tokens écrits en cache, ${tokens(read)} relus` +
			(ratio === null
				? '. Rien n’a été écrit sur cette fenêtre, le préfixe vient d’avant.'
				: poor
					? '. Un préfixe que le fournisseur garde se relit des dizaines de fois, donc sous 3 le contexte est réécrit au lieu d’être relu, et une écriture coûte plus cher qu’une entrée normale.'
					: '. Une lecture coûte bien moins qu’une écriture, donc plus ce chiffre est haut moins le contexte est repayé.')
	);
</script>

<!-- A ratio worth acting on gets the shape of a status, the rest stays a quiet
     number: a pill on every row would say nothing, a pill on the two routes that
     rewrite the context says exactly which ones to move. -->
{#if read > 0 || write > 0}
	{#if poor}
		<span class="pill pill-warn tabular" {title}>
			{compact ? '' : 'cache '}{shown}
		</span>
	{:else}
		<span class="tabular text-muted-foreground shrink-0 text-xs" {title}>
			{compact || ratio === null ? '' : 'cache '}{shown}
		</span>
	{/if}
{/if}
