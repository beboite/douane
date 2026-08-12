<script lang="ts">
	import { Switch as SwitchPrimitive } from "bits-ui";
	import { cn, type WithoutChildrenOrChild } from "$lib/utils.js";

	let {
		ref = $bindable(null),
		class: className,
		checked = $bindable(false),
		size = "default",
		...restProps
	}: WithoutChildrenOrChild<SwitchPrimitive.RootProps> & {
		size?: "sm" | "default";
	} = $props();
</script>

<!-- Bigger than the shadcn default, with an off state that can actually be
     seen. A switch is the whole control on the models screen, and at 32x18 on a
     near-transparent rail nobody could tell served from not served.

     The state variants are `data-[state=checked]`, not `data-checked`: this
     version of bits-ui writes `data-state`, so the shipped classes matched
     nothing and the rail had no background in either state. It went unnoticed
     because nothing used a switch until the models screen did. -->
<SwitchPrimitive.Root
	bind:ref
	bind:checked
	data-slot="switch"
	data-size={size}
	class={cn(
		"shrink-0 rounded-full border border-transparent focus-visible:border-ring focus-visible:ring-3 focus-visible:ring-ring/50 aria-invalid:border-destructive aria-invalid:ring-3 aria-invalid:ring-destructive/20 data-[size=default]:h-6 data-[size=default]:w-11 data-[size=sm]:h-5 data-[size=sm]:w-9 dark:aria-invalid:border-destructive/50 dark:aria-invalid:ring-destructive/40 data-[state=checked]:bg-primary data-[state=unchecked]:bg-foreground/30 dark:data-[state=unchecked]:bg-white/30 peer group/switch relative inline-flex items-center px-0.5 transition-all outline-none after:absolute after:-inset-x-3 after:-inset-y-2 data-disabled:cursor-not-allowed data-disabled:opacity-50",
		className
	)}
	{...restProps}
>
	<SwitchPrimitive.Thumb
		data-slot="switch-thumb"
		class="pointer-events-none block rounded-full bg-white shadow-sm ring-0 transition-transform group-data-[size=default]/switch:size-5 group-data-[size=sm]/switch:size-4 group-data-[size=default]/switch:data-[state=checked]:translate-x-5 group-data-[size=sm]/switch:data-[state=checked]:translate-x-4 data-[state=unchecked]:translate-x-0"
	/>
</SwitchPrimitive.Root>
