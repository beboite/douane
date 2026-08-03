const STORAGE_KEY = 'douane-admin';

/**
 * The admin token, for as long as this tab is open.
 *
 * `sessionStorage` and not `localStorage`: a token that survives closing the
 * browser is a token left lying on the machine, and re-pasting it once a day is
 * a small price. The broker deliberately sets no cookie, so this is the only
 * place it lives on the client.
 */
class Session {
	token = $state('');

	constructor() {
		try {
			this.token = sessionStorage.getItem(STORAGE_KEY) ?? '';
		} catch {
			/* storage refused: the user pastes it again */
		}
	}

	get unlocked() {
		return this.token.length > 0;
	}

	unlock(token: string) {
		this.token = token.trim();
		try {
			sessionStorage.setItem(STORAGE_KEY, this.token);
		} catch {
			/* the token still works for this page load */
		}
	}

	lock() {
		this.token = '';
		try {
			sessionStorage.removeItem(STORAGE_KEY);
		} catch {
			/* nothing to forget */
		}
	}
}

export const session = new Session();
