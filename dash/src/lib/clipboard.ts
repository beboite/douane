export async function copyText(value: string): Promise<void> {
	try {
		if (navigator.clipboard?.writeText) {
			await navigator.clipboard.writeText(value);
			return;
		}
	} catch { /* HTTP origins and denied clipboard access can still use selection-based copy. */ }

	const activeElement = document.activeElement instanceof HTMLElement ? document.activeElement : null;
	const input = activeElement instanceof HTMLInputElement || activeElement instanceof HTMLTextAreaElement ? activeElement : null;
	const inputSelection = input?.selectionStart != null && input.selectionEnd != null
		? { start: input.selectionStart, end: input.selectionEnd, direction: input.selectionDirection }
		: null;
	const selection = document.getSelection();
	const ranges = selection ? Array.from({ length: selection.rangeCount }, (_, index) => selection.getRangeAt(index).cloneRange()) : [];
	const textarea = document.createElement('textarea');
	textarea.value = value;
	textarea.readOnly = true;
	textarea.tabIndex = -1;
	textarea.setAttribute('aria-hidden', 'true');
	textarea.style.cssText = 'position:fixed;top:0;left:0;opacity:0;pointer-events:none;font-size:16px';

	try {
		// A modal dialog makes elements outside it inert, including a textarea in the body.
		(activeElement?.closest('dialog[open]') ?? document.body).append(textarea);
		textarea.focus({ preventScroll: true });
		textarea.select();
		textarea.setSelectionRange(0, value.length);
		if (!document.execCommand('copy')) throw new Error('Clipboard copy failed.');
	} finally {
		textarea.remove();
		activeElement?.focus({ preventScroll: true });
		if (selection) {
			selection.removeAllRanges();
			for (const range of ranges) selection.addRange(range);
		}
		if (input && inputSelection) input.setSelectionRange(inputSelection.start, inputSelection.end, inputSelection.direction ?? undefined);
	}
}
