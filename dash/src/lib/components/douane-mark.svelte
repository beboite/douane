<script lang="ts">
	let { size = 38 }: { size?: number } = $props();
</script>

<svg width={size} height={size} viewBox="0 0 48 48" fill="none" aria-hidden="true">
	<path d="M6 41h36" stroke="currentColor" stroke-width="2" opacity=".25"/>
	<path d="M13 24v15" stroke="currentColor" stroke-width="5" stroke-linecap="round"/>
	<path d="m13 24 24-14" stroke="currentColor" stroke-width="7" stroke-linecap="round" opacity=".24"/>
	<path d="m14 23 3-2m6-3 3-2m6-3 3-2" stroke="currentColor" stroke-width="7"/>
	<circle cx="13" cy="24" r="4" fill="currentColor"/>
</svg>
