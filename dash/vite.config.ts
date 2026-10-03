import adapter from '@sveltejs/adapter-static';
import { sveltekit } from '@sveltejs/kit/vite';
import { defineConfig } from 'vite';

export default defineConfig({
	plugins: [
		sveltekit({
			compilerOptions: {
				runes: ({ filename }) =>
					filename.split(/[/\\]/).includes('node_modules') ? undefined : true
			},

			// Embed the HTML, fonts and hashed assets in the Rust service.
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
