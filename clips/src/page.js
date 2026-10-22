// Kairo's clip pages: checking what Kairo sends, and the pages themselves.
// The Worker (index.js) stores and serves them.

export const LIFETIME = 2 * 24 * 3600;
export const SITE = 'https://fsiuhgshiqoih32.github.io/kairo/';
export const ICON = SITE + 'media/site/favicon.png';
const FONTS = SITE + 'fonts/';

// The hosts Kairo uploads to, and the files it puts there.
const HOSTED =
  /^https:\/\/(litter\.catbox\.moe\/[A-Za-z0-9]{4,32}|files\.catbox\.moe\/[A-Za-z0-9]{4,32}|pomf2\.lain\.la\/f\/[A-Za-z0-9]{4,32})\.(mp4|jpg|jpeg|png|gif|webp)$/;
const ALPHABET = 'ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789';


export function now() {
  return Math.floor(Date.now() / 1000);
}

/** POST /api/clips: checks what Kairo sent, keeps it for two days, answers with the link. */
export async function create(request, env, url) {
  if (env.LIMITER) {
    const ip = request.headers.get('CF-Connecting-IP') || 'unknown';
    const { success } = await env.LIMITER.limit({ key: ip });
    if (!success) return json({ error: 'Too many links at once. Wait a minute.' }, 429);
  }
  let body;
  try {
    body = await request.json();
  } catch {
    return json({ error: 'Send JSON.' }, 400);
  }
  const clip = check(body);
  if (typeof clip === 'string') return json({ error: clip }, 400);
  clip.created = now();
  let id;
  for (let i = 0; i < 3 && !id; i++) {
    const candidate = randomId();
    if ((await env.CLIPS.get(candidate)) === null) id = candidate;
  }
  if (!id) return json({ error: 'Try again.' }, 503);
  await env.CLIPS.put(id, JSON.stringify(clip), { expirationTtl: LIFETIME });
  return json(
    { id, url: `${url.origin}/c/${id}`, expires: clip.created + LIFETIME },
    201
  );
}

/** What a page needs, from what Kairo sent, or why it won't do. */
export function check(body) {
  if (!body || typeof body !== 'object') return 'Send a clip.';
  const { video, thumb } = body;
  if (typeof video !== 'string' || !HOSTED.test(video)) {
    return 'The clip has to be on litterbox.catbox.moe, pomf2.lain.la or catbox.moe.';
  }
  if (thumb != null && (typeof thumb !== 'string' || !HOSTED.test(thumb) || thumb.endsWith('.mp4'))) {
    return 'The picture has to be an image on the same hosts.';
  }
  const text = (v, max) =>
    typeof v === 'string' ? v.replace(/[\u0000-\u001f\u007f]/g, ' ').trim().slice(0, max) : '';
  const number = (v, max) =>
    typeof v === 'number' && Number.isFinite(v) && v > 0 && v <= max ? v : null;
  return {
    video,
    thumb: thumb || null,
    title: text(body.title, 120) || 'A Kairo clip',
    game: text(body.game, 60),
    seconds: number(body.seconds, 24 * 3600),
    width: number(body.width, 8192) && Math.round(body.width),
    height: number(body.height, 8192) && Math.round(body.height),
  };
}

function randomId() {
  const bytes = new Uint8Array(16);
  let id = '';
  while (id.length < 8) {
    crypto.getRandomValues(bytes);
    for (const b of bytes) {
      // 248 = 4 * 62: anything above would make some letters likelier.
      if (b < 248 && id.length < 8) id += ALPHABET[b % 62];
    }
  }
  return id;
}

export function json(value, status) {
  return new Response(JSON.stringify(value), {
    status,
    headers: { 'Content-Type': 'application/json; charset=utf-8', 'Cache-Control': 'no-store' },
  });
}

export function html(body, status) {
  return new Response(body, {
    status,
    headers: {
      'Content-Type': 'text/html; charset=utf-8',
      'Cache-Control': 'public, max-age=60',
      'Content-Security-Policy':
        "default-src 'none'; media-src https://litter.catbox.moe https://files.catbox.moe https://pomf2.lain.la; " +
        "img-src https://litter.catbox.moe https://files.catbox.moe https://pomf2.lain.la https://fsiuhgshiqoih32.github.io; " +
        "font-src https://fsiuhgshiqoih32.github.io; style-src 'unsafe-inline'; script-src 'unsafe-inline'; " +
        "base-uri 'none'; form-action 'none'; frame-ancestors 'none'",
      'Referrer-Policy': 'no-referrer',
      'X-Content-Type-Options': 'nosniff',
    },
  });
}

const escape = (s) =>
  String(s).replace(/[&<>"']/g, (c) => ({ '&': '&amp;', '<': '&lt;', '>': '&gt;', '"': '&quot;', "'": '&#39;' })[c]);

/** 0:42, 3:05, 1:02:09 */
export function duration(seconds) {
  const s = Math.round(seconds);
  const h = Math.floor(s / 3600);
  const m = Math.floor((s % 3600) / 60);
  const two = (n) => String(n).padStart(2, '0');
  return h ? `${h}:${two(m)}:${two(s % 60)}` : `${m}:${two(s % 60)}`;
}

/** The page for a clip. */
export function render(clip, url) {
  const page = `${url.origin}${url.pathname}`;
  const video = clip.video.endsWith('.mp4');
  const facts = [clip.game, clip.seconds && duration(clip.seconds)].filter(Boolean).join(' · ');
  const description = facts ? `${facts} · Shared with Kairo` : 'Shared with Kairo';
  const w = clip.width || 1280;
  const h = clip.height || 720;
  const e = escape;
  const meta = [
    `<meta property="og:site_name" content="Kairo">`,
    `<meta property="og:title" content="${e(clip.title)}">`,
    `<meta property="og:description" content="${e(description)}">`,
    `<meta property="og:url" content="${e(page)}">`,
    `<meta name="theme-color" content="#ff7a1a">`,
  ];
  if (video) {
    meta.push(
      `<meta property="og:type" content="video.other">`,
      `<meta property="og:video" content="${e(clip.video)}">`,
      `<meta property="og:video:secure_url" content="${e(clip.video)}">`,
      `<meta property="og:video:type" content="video/mp4">`,
      `<meta property="og:video:width" content="${w}">`,
      `<meta property="og:video:height" content="${h}">`,
      `<meta property="twitter:card" content="player">`,
      `<meta property="twitter:player:stream" content="${e(clip.video)}">`,
      `<meta property="twitter:player:stream:content_type" content="video/mp4">`,
      `<meta property="twitter:player:width" content="${w}">`,
      `<meta property="twitter:player:height" content="${h}">`
    );
    if (clip.thumb) {
      meta.push(`<meta property="og:image" content="${e(clip.thumb)}">`, `<meta property="twitter:image" content="0">`);
    }
  } else {
    meta.push(
      `<meta property="og:type" content="website">`,
      `<meta property="og:image" content="${e(clip.video)}">`,
      `<meta property="twitter:card" content="summary_large_image">`
    );
  }
  const media = video
    ? `<video controls playsinline preload="metadata" width="${w}" height="${h}"${clip.thumb ? ` poster="${e(clip.thumb)}"` : ''} src="${e(clip.video)}"></video>`
    : `<img src="${e(clip.video)}" alt="${e(clip.title)}">`;
  return `<!doctype html>
<html lang="en">
<head>
<meta charset="utf-8">
<meta name="viewport" content="width=device-width, initial-scale=1">
<title>${e(clip.title)} · Kairo</title>
<meta name="description" content="${e(description)}">
${meta.join('\n')}
<link rel="icon" href="${ICON}">
<style>${STYLE}</style>
</head>
<body>
<main>
  <a class="brand" href="${SITE}"><img src="${ICON}" alt="" width="26" height="26">Kairo</a>
  <div class="frame" style="aspect-ratio:${w} / ${h}">${media}</div>
  <h1>${e(clip.title)}</h1>
  <p class="facts">${e(facts || 'A clip')}<span class="dot"> · </span><span id="left" data-ends="${clip.created + LIFETIME}">Link works for 2 days</span></p>
  <div class="actions">
    <a class="button primary" href="${e(clip.video)}" download>Download</a>
    <button class="button" id="copy" type="button" data-link="${e(page)}">Copy link</button>
  </div>
  <section class="get">
    <p><b>Made with Kairo</b>, the free clip recorder for Windows. Press one key to save the last 30 seconds of your game, then share it like this.</p>
    <a class="button primary" href="${SITE}">Get Kairo, free</a>
  </section>
</main>
<script>${SCRIPT}</script>
</body>
</html>`;
}

/** The page for a clip that's gone (or never was). */
export function expired(url) {
  const body = `<!doctype html>
<html lang="en">
<head>
<meta charset="utf-8">
<meta name="viewport" content="width=device-width, initial-scale=1">
<title>Clip expired · Kairo</title>
<meta property="og:site_name" content="Kairo">
<meta property="og:title" content="This clip has expired">
<meta property="og:description" content="Kairo links last 2 days.">
<meta name="theme-color" content="#ff7a1a">
<link rel="icon" href="${ICON}">
<style>${STYLE}</style>
</head>
<body>
<main class="gone">
  <a class="brand" href="${SITE}"><img src="${ICON}" alt="" width="26" height="26">Kairo</a>
  <h1>This clip has expired</h1>
  <p class="facts">Links from Kairo last 2 days. Ask whoever sent it to share it again.</p>
  <section class="get">
    <p><b>Kairo</b> is a free clip recorder for Windows. Press one key to save the last 30 seconds of your game and share it with a link.</p>
    <a class="button primary" href="${SITE}">Get Kairo, free</a>
  </section>
</main>
</body>
</html>`;
  return html(body, url.pathname.startsWith('/c/') ? 410 : 404);
}

const STYLE = `
@font-face{font-family:Inter;font-weight:400;font-display:swap;src:url(${FONTS}Inter-Regular.ttf)}
@font-face{font-family:Inter;font-weight:600;font-display:swap;src:url(${FONTS}Inter-SemiBold.ttf)}
@font-face{font-family:Inter;font-weight:700;font-display:swap;src:url(${FONTS}Inter-Bold.ttf)}
:root{--bg:#141519;--raised:#1b1d22;--raised-2:#23262d;--line:#2c2f37;--ink:#eef0f3;--soft:#c3c7ce;--muted:#8f949e;--accent:#ff7a1a;--accent-hi:#ff8f3d;--on-accent:#141519}
*{box-sizing:border-box}
html{color-scheme:dark}
body{margin:0;background:var(--bg);color:var(--ink);font:16px/1.5 Inter,system-ui,-apple-system,"Segoe UI",sans-serif;-webkit-font-smoothing:antialiased}
main{max-width:1100px;margin:0 auto;padding:20px 16px 48px}
.brand{display:inline-flex;align-items:center;gap:10px;color:var(--ink);text-decoration:none;font-weight:700;font-size:19px;margin-bottom:18px}
.brand img{border-radius:7px}
.frame{width:100%;max-height:72vh;background:#000;border-radius:14px;overflow:hidden;border:1px solid var(--line);display:flex;align-items:center;justify-content:center}
.frame video,.frame img{width:100%;height:100%;max-height:72vh;display:block;object-fit:contain;background:#000}
h1{font-size:clamp(22px,3vw,32px);line-height:1.2;margin:20px 0 6px;font-weight:700;overflow-wrap:anywhere}
.facts{color:var(--muted);margin:0 0 18px}
.actions{display:flex;flex-wrap:wrap;gap:10px}
.button{appearance:none;border:1px solid var(--line);background:var(--raised-2);color:var(--ink);font:600 15px/1 Inter,system-ui,sans-serif;padding:12px 18px;border-radius:10px;text-decoration:none;cursor:pointer;display:inline-flex;align-items:center;min-height:44px}
.button:hover{background:var(--line)}
.button.primary{background:var(--accent);border-color:var(--accent);color:var(--on-accent)}
.button.primary:hover{background:var(--accent-hi)}
.button:focus-visible{outline:2px solid var(--accent-hi);outline-offset:2px}
.get{margin-top:40px;padding:20px;border:1px solid var(--line);border-radius:14px;background:var(--raised);display:flex;flex-wrap:wrap;gap:16px;align-items:center;justify-content:space-between}
.get p{margin:0;color:var(--soft);max-width:60ch}
.gone h1{margin-top:12vh}
@media (max-width:560px){.actions .button{flex:1;justify-content:center}.dot{display:none}#left{display:block}}
`;

const SCRIPT = `
(function(){
  var left=document.getElementById('left');
  if(left){
    var ends=+left.dataset.ends, s=ends-Math.floor(Date.now()/1000);
    if(s<=0){left.textContent='Link has expired';}
    else{var h=Math.ceil(s/3600);left.textContent='Link works for '+(h>36?Math.ceil(h/24)+' more days':h+' more hour'+(h==1?'':'s'));}
  }
  var copy=document.getElementById('copy');
  if(copy){copy.addEventListener('click',function(){
    var done=function(){copy.textContent='Copied';setTimeout(function(){copy.textContent='Copy link';},1600);};
    if(navigator.clipboard){navigator.clipboard.writeText(copy.dataset.link).then(done,function(){prompt('Copy this link:',copy.dataset.link);});}
    else{prompt('Copy this link:',copy.dataset.link);}
  });}
})();
`;
