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
	 *  One colour for both measures, and it is the accent rather than one of the
	 *  three trust colours: green, amber and red mean who read the conversation,
	 *  and a chart borrowing one would turn a busy hour into an alarm. */
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

	/* There is no such thing as half an appel. On a quiet window the scale runs
	   0 to 2 and d3 offers 0,5 and 1,5 as ticks, which read as a measurement
	   this axis cannot make. Fractions lose their label and keep their grid
	   line, so the spacing stays even. */
	const yLabel = $derived.by(() => {
		const shape = measure === 'calls' ? count : tokens;
		return (v: number) => (measure === 'calls' && !Number.isInteger(v) ? '' : shape(v));
	});
</script>

<Chart.Container {config} class="aspect-auto h-[14rem] w-full">
	<BarChart
		data={points}
		x="at"
		series={[{ key: measure, label: config[measure].label, color: `var(--color-${measure})` }]}
		bandPadding={0.3}
		props={{
			bars: { radius: 5, rounded: 'top', strokeWidth: 0 },
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
