// Tests for the clip pages: `node clips/test.mjs`. Uses a fake KV; no Cloudflare needed.
import assert from 'node:assert/strict';
import worker from './src/index.js';
import { check, render, duration, LIFETIME } from './src/page.js';

class FakeKV {
  constructor() {
    this.items = new Map();
  }
  async get(key, type) {
    const v = this.items.get(key);
    if (v === undefined) return null;
    return type === 'json' ? JSON.parse(v.value) : v.value;
  }
  async put(key, value, options) {
    this.items.set(key, { value, ttl: options?.expirationTtl });
  }
}

const env = { CLIPS: new FakeKV() };
const call = (path, init) => worker.fetch(new Request(`https://kairo-clips.example.workers.dev${path}`, init), env);
const post = (body) =>
  call('/api/clips', { method: 'POST', headers: { 'Content-Type': 'application/json' }, body: JSON.stringify(body) });

let passed = 0;
async function test(name, f) {
  await f();
  passed++;
  console.log(`ok - ${name}`);
}

await test('a clip on litterbox gets a page that lasts two days', async () => {
  const r = await post({
    video: 'https://litter.catbox.moe/nc5mz6.mp4',
    thumb: 'https://litter.catbox.moe/ab12cd.jpg',
    title: 'Ace on Ascent',
    game: 'VALORANT',
    seconds: 30.4,
    width: 1920,
    height: 1080,
  });
  assert.equal(r.status, 201);
  const { id, url, expires } = await r.json();
  assert.match(id, /^[A-Za-z0-9]{8}$/);
  assert.equal(url, `https://kairo-clips.example.workers.dev/c/${id}`);
  assert.ok(Math.abs(expires - (Date.now() / 1000 + LIFETIME)) < 5);
  assert.equal(env.CLIPS.items.get(id).ttl, LIFETIME);

  const page = await call(`/c/${id}`, { headers: { 'User-Agent': 'Mozilla/5.0 (compatible; Discordbot/2.0)' } });
  assert.equal(page.status, 200);
  assert.match(page.headers.get('Content-Type'), /text\/html/);
  const text = await page.text();
  for (const tag of [
    '<meta property="og:video" content="https://litter.catbox.moe/nc5mz6.mp4">',
    '<meta property="og:video:type" content="video/mp4">',
    '<meta property="og:video:width" content="1920">',
    '<meta property="og:video:height" content="1080">',
    '<meta property="twitter:card" content="player">',
    '<meta property="og:image" content="https://litter.catbox.moe/ab12cd.jpg">',
    '<meta property="og:title" content="Ace on Ascent">',
    '<meta property="og:description" content="VALORANT · 0:30 · Shared with Kairo">',
  ]) {
    assert.ok(text.includes(tag), `missing ${tag}`);
  }
  assert.ok(text.includes('<video controls playsinline'));
});

await test('pomf2 and catbox links are taken too; screenshots get an image card', async () => {
  for (const video of ['https://pomf2.lain.la/f/8m87cizz.mp4', 'https://files.catbox.moe/d4fknf.mp4']) {
    assert.equal((await post({ video, title: 'x' })).status, 201, video);
  }
  const r = await post({ video: 'https://files.catbox.moe/abc123.png', title: 'Screenshot' });
  const { id } = await r.json();
  const text = await (await call(`/c/${id}`)).text();
  assert.ok(text.includes('<meta property="og:image" content="https://files.catbox.moe/abc123.png">'));
  assert.ok(text.includes('summary_large_image'));
  assert.ok(!text.includes('og:video'));
});

await test('anything else is refused', async () => {
  for (const body of [
    { video: 'https://evil.example/x.mp4' },
    { video: 'https://litter.catbox.moe/nc5mz6.mp4?x=1' },
    { video: 'http://litter.catbox.moe/nc5mz6.mp4' },
    { video: 'https://litter.catbox.moe/../x.mp4' },
    { video: 'https://litter.catbox.moe/nc5mz6.exe' },
    { video: 'https://litter.catbox.moe/nc5mz6.mp4', thumb: 'https://evil.example/t.jpg' },
    { video: 'https://litter.catbox.moe/nc5mz6.mp4', thumb: 'https://litter.catbox.moe/nc5mz6.mp4' },
    null,
  ]) {
    assert.equal((await post(body)).status, 400, JSON.stringify(body));
  }
  assert.equal((await call('/api/clips', { method: 'POST', body: 'not json' })).status, 400);
  assert.equal((await call('/api/clips')).status, 405);
});

await test('titles are escaped, trimmed and cleaned', async () => {
  const clip = check({
    video: 'https://litter.catbox.moe/nc5mz6.mp4',
    title: '  <script>alert("x")</script>\u0007' + 'a'.repeat(200),
    game: 'G&G',
    seconds: -4,
    width: 1e9,
  });
  assert.equal(clip.title.length, 120);
  assert.equal(clip.seconds, null);
  assert.equal(clip.width, null);
  clip.created = Math.floor(Date.now() / 1000);
  const text = render(clip, new URL('https://k.example/c/AAAAAAAA'));
  assert.ok(!text.includes('<script>alert'), 'escaped');
  assert.ok(text.includes('&lt;script&gt;alert(&quot;x&quot;)'));
  assert.ok(text.includes('G&amp;G'));
});

await test('an expired or unknown clip says so', async () => {
  const r = await call('/c/AAAAAAAA');
  assert.equal(r.status, 410);
  assert.ok((await r.text()).includes('This clip has expired'));
  assert.equal((await call('/c/../../etc')).status, 404);
  // Still in KV a moment past two days (KV deletes lazily): the page is gone anyway.
  env.CLIPS.items.set('BBBBBBBB', {
    value: JSON.stringify({ video: 'https://litter.catbox.moe/nc5mz6.mp4', title: 'old', created: Math.floor(Date.now() / 1000) - LIFETIME - 1 }),
  });
  assert.equal((await call('/c/BBBBBBBB')).status, 410);
  const home = await call('/');
  assert.equal(home.status, 302);
  assert.equal(home.headers.get('Location'), 'https://fsiuhgshiqoih32.github.io/kairo/');
});

await test('a busy limiter turns people away', async () => {
  const limited = { CLIPS: new FakeKV(), LIMITER: { limit: async () => ({ success: false }) } };
  const r = await worker.fetch(
    new Request('https://k.example/api/clips', { method: 'POST', body: JSON.stringify({ video: 'https://litter.catbox.moe/nc5mz6.mp4' }) }),
    limited
  );
  assert.equal(r.status, 429);
});

await test('lengths read like a player', async () => {
  assert.equal(duration(30.4), '0:30');
  assert.equal(duration(185), '3:05');
  assert.equal(duration(3729), '1:02:09');
});

console.log(`\n${passed} passed`);
