# douane dashboard

A Svelte 5 dashboard for subscription accounts, quotas and request activity. The Rust service embeds the static output and serves the JSON API under `/admin/api`.

```sh
pnpm install --frozen-lockfile
pnpm check
pnpm build
```

Commit `build/` together with changes to `src/`. Production does not need Node.js.

The dashboard opens on quotas and reads the current service state. Account imports, OAuth connections, account toggles, model availability and routing settings call the service directly. Provider failures and unknown quotas remain visible; the shipped app contains no sample accounts.
