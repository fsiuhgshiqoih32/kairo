// Kairo's clip pages: the link Kairo copies when you share a clip.
//
// Kairo uploads the clip to a free file host (litterbox.catbox.moe, pomf2.lain.la or
// catbox.moe), then asks this Worker for a page: POST /api/clips with where the video is and
// what it is. The page lives in KV for two days and KV deletes it then; the link is /c/<id>.
//
// A page plays the clip in the browser, and carries the Open Graph tags that make Discord play
// it right in the chat (the same tags FxEmbed uses: og:video with its type and size, a player
// card and a poster).

import { LIFETIME, SITE, ICON, create, expired, html, now, render, json } from './page.js';

const ID = /^[A-Za-z0-9]{8}$/;

export default {
  async fetch(request, env) {
    const url = new URL(request.url);
    if (url.pathname === '/api/clips') {
      if (request.method !== 'POST') return json({ error: 'Use POST.' }, 405);
      return create(request, env, url);
    }
    const page = url.pathname.match(/^\/c\/([^/]+)\/?$/);
    if (page && (request.method === 'GET' || request.method === 'HEAD')) {
      if (!ID.test(page[1])) return expired(url);
      const clip = await env.CLIPS.get(page[1], 'json');
      if (!clip || now() >= clip.created + LIFETIME) return expired(url);
      return html(render(clip, url), 200);
    }
    if (url.pathname === '/' || url.pathname === '') return Response.redirect(SITE, 302);
    if (url.pathname === '/favicon.ico') return Response.redirect(ICON, 301);
    return expired(url);
  },
};
