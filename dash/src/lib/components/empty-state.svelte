<script lang="ts">
	import Inbox from '@lucide/svelte/icons/inbox';
	let { title, description, action, onclick }: { title: string; description: string; action?: string; onclick?: () => void } = $props();
</script>

<div class="empty-state">
	<div class="empty-icon"><Inbox size={22}/></div>
	<h3>{title}</h3><p>{description}</p>
	{#if action && onclick}<button class="button primary" {onclick}>{action}</button>{/if}
</div>

<style>
	.empty-state { min-height: 260px; display: flex; flex-direction: column; align-items: center; justify-content: center; padding: 36px 20px; text-align: center; }
	.empty-icon { display: grid; place-items: center; width: 52px; height: 52px; border: 1px solid #7899ff30; border-radius: 14px; background: #5478f610; color: var(--blue); margin-bottom: 18px; }
	h3 { font-family: var(--font-display); font-size: 19px; font-weight: 600; letter-spacing: -.025em; margin: 0 0 9px; }
	p { font-size: 15px; line-height: 1.6; color: var(--muted); max-width: 380px; margin: 0 0 20px; }
</style>
