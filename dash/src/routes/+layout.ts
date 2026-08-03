// A single-page app served from a Rust binary: nothing is rendered ahead of
// time, because every page needs the admin token the browser holds and the
// server deliberately does not put in a cookie.
export const ssr = false;
export const prerender = false;
