<script lang="ts">
	import * as Chart from '$lib/components/ui/chart';
	import { BarChart } from 'layerchart';
	import type { Slot } from '$lib/api';
	import { axisLabel, count, slotLabel, tokens } from '$lib/format';

	/** The window as a histogram.
	 *
	 *  Bars rather than a curve, because every point is a count inside a slot and
	 *  nothing happened between two of them. A smoothed line over the same data
	 *  invents a rise where there was one call and a fall where there was one
	 *  fewer, so a quiet day reads as a mountain range.
	 *
	 *  Only quantities that are one number get a height. A cost is a map keyed by
	 *  currency, so plotting it would mean adding yuan to dollars to get pixels,
	 *  which is the one thing this project refuses to do anywhere else. Cost
	 *  appears in the tooltip, per unit, and never as a bar. */
	let {
		series,
		measure = $bindable('calls')
	}: {
		series: Slot[];
		measure: 'calls' | 'tokens';
	} = $props();

	const width = $derived(series[0]?.seconds ?? 3600);

	const points = $derived(
		series.map((s) => ({
			at: new Date(s.at),
			calls: s.calls,
			tokens: s.input_tokens + s.output_tokens
		}))
	);

	const config = $derived({
		calls: { label: 'appels', color: 'var(--chart-1)' },
		tokens: { label: 'tokens', color: 'var(--chart-1)' }
	} satisfies Chart.ChartConfig);

	const shape = $derived(measure === 'calls' ? count : tokens);

	/* There is no such thing as half an appel. On a quiet window the scale runs
	   0 to 2 and d3 offers 0,5 and 1,5 as ticks, which read as a measurement
	   this axis cannot make. Fractions lose their label and keep their grid
	   line, so the spacing stays even. */
	const yLabel = $derived((v: number) =>
		measure === 'calls' && !Number.isInteger(v) ? '' : shape(v)
	);
</script>

<Chart.Container {config} class="aspect-auto h-[13rem] w-full">
	<BarChart
		data={points}
		x="at"
		series={[{ key: measure, label: config[measure].label, color: `var(--color-${measure})` }]}
		bandPadding={0.25}
		props={{
			bars: { radius: 3, rounded: 'top', strokeWidth: 0 },
			xAxis: { ticks: 6, format: (v: Date) => axisLabel(v.toISOString(), width) },
			yAxis: { ticks: 4, format: yLabel },
			grid: { class: 'stroke-grid' },
			highlight: { area: { class: 'fill-accent/40' } }
		}}
	>
		{#snippet tooltip()}
			<Chart.Tooltip
				indicator="line"
				labelFormatter={(v: Date) => slotLabel(v.toISOString(), width)}
			/>
		{/snippet}
	</BarChart>
</Chart.Container>
