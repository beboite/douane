import adapter from '@sveltejs/adapter-static';
import { sveltekit } from '@sveltejs/kit/vite';
import tailwindcss from '@tailwindcss/vite';
import { defineConfig } from 'vite';

export default defineConfig({
	plugins: [
		tailwindcss(),
		sveltekit({
			compilerOptions: {
				runes: ({ filename }) =>
					filename.split(/[/\\]/).includes('node_modules') ? undefined : true
			},

			// One HTML file and a folder of hashed assets, all of it embedded in
			// the broker binary. No node process runs in production, and the page
			// fetches nothing from a third party: a dashboard about credentials
			// has no business making requests off-origin.
			adapter: adapter({ fallback: 'index.html', precompress: false, strict: false }),

			// Caddy only publishes `/admin*` for this host, so the app lives there
			// and its asset URLs have to say so.
			paths: { base: '/admin' },

			alias: {
				'@/*': './src/lib/*'
			}
		})
	]
});
