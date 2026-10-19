// Kairo's site: picks the download for this computer, fills in the newest release, and runs the
// page's live parts (the hero's replay panel, F8, the app tour). Every link works without it.
(() => {
  "use strict";
  const $ = (s, el = document) => el.querySelector(s);
  const $$ = (s, el = document) => [...el.querySelectorAll(s)];
  const reduced = matchMedia("(prefers-reduced-motion: reduce)").matches;
  const esc = (s) => String(s).replace(/[&<>"]/g, (c) => ({ "&": "&amp;", "<": "&lt;", ">": "&gt;", '"': "&quot;" })[c]);

  // ---------- Downloads ----------
  const REPO = "fsiuhgshiqoih32/kairo";
  const release = {
    version: "0.3.0",
    date: null,
    files: {
      windows: { name: "kairo-v0.3.0-windows-x64-setup.exe", size: 31979762 },
      portable: { name: "kairo-v0.3.0-windows-x64-portable.zip", size: 15609058 },
      mac: { name: "kairo-v0.3.0-macos-universal.zip", size: 25437353 },
      linux: { name: "kairo-v0.3.0-linux-x86_64-portable.tar.gz", size: 15796739 },
      sums: { name: "SHA256SUMS.txt", size: 0 },
    },
  };
  const SUFFIX = {
    windows: "-windows-x64-setup.exe",
    portable: "-windows-x64-portable.zip",
    mac: "-macos-universal.zip",
    linux: "-linux-x86_64-portable.tar.gz",
  };
  const OS = {
    windows: {
      name: "Windows",
      for: "For Windows 10 and 11, 64‑bit",
      label: "Download the installer",
      meta: "Windows 10 and 11",
      note: "Installs for you without administrator rights and uninstalls from Settings › Apps. It isn’t code-signed yet, so Windows may say it protected your PC: choose More info, then Run anyway.",
    },
    mac: {
      name: "macOS",
      for: "For macOS 11 or newer, Apple silicon and Intel",
      label: "Download for Mac",
      meta: "macOS 11 or newer",
      note: "Unzip it and move Kairo to Applications. It isn’t notarized yet: the first time, right-click it and choose Open. Then allow Screen Recording in System Settings › Privacy & Security. Desktop sound needs macOS 13.",
    },
    linux: {
      name: "Linux",
      for: "For Linux on x86_64, with X11",
      label: "Download for Linux",
      meta: "x86_64, X11",
      note: "Unpack it and run ./kairo. Desktop sound needs parec (PulseAudio or PipeWire); copying clips needs xclip or wl-copy. On Wayland, bind kairo save to a key.",
    },
  };
  const url = (f) => `https://github.com/${REPO}/releases/download/v${release.version}/${f.name}`;
  const mb = (bytes) => `${Math.max(1, Math.round(bytes / 1e6))} MB`;

  function detect() {
    const p = (navigator.userAgentData && navigator.userAgentData.platform) || navigator.platform || "";
    const ua = navigator.userAgent || "";
    const touchMac = /Mac/.test(p) && navigator.maxTouchPoints > 1;
    if (/android|iphone|ipad|ipod/i.test(ua) || touchMac || (navigator.userAgentData && navigator.userAgentData.mobile)) {
      return { os: /iphone|ipad|ipod/i.test(ua) || touchMac ? "mac" : "windows", phone: true };
    }
    if (/mac/i.test(p) || /Mac OS X/.test(ua)) return { os: "mac", phone: false };
    if (/linux|x11/i.test(p) || /Linux|X11/.test(ua)) return { os: "linux", phone: false };
    return { os: "windows", phone: false };
  }
  const here = detect();
  let chosen = here.os;

  function paint() {
    const f = release.files[chosen];
    const o = OS[chosen];
    $("#pick-for").textContent = o.for;
    $("#go-label").textContent = o.label;
    $("#go").href = url(f);
    $("#go-file").textContent = f.name;
    $("#go-size").textContent = mb(f.size);
    $("#pick-note").textContent = o.note;
    $$(".os-switch [role=radio]").forEach((b) => {
      const on = b.dataset.os === chosen;
      b.setAttribute("aria-checked", String(on));
      b.tabIndex = on ? 0 : -1;
    });
    $("#alt-portable").href = url(release.files.portable);
    $("#alt-mac").href = url(release.files.mac);
    $("#alt-linux").href = url(release.files.linux);
    $("#sums").href = url(release.files.sums);
    $$("[data-size]").forEach((el) => (el.textContent = mb(release.files[el.dataset.size].size)));
    $$("[data-version]").forEach((el) => (el.textContent = release.version));
    if (release.date) {
      const d = new Intl.DateTimeFormat("en-GB", { day: "numeric", month: "long", year: "numeric" }).format(release.date);
      $("#release-date").textContent = `, released ${d}`;
    }

    const hero = $("#hero-download");
    const meta = $("#hero-meta");
    if (here.phone) {
      $("#hero-download-label").textContent = "Get Kairo for your computer";
      hero.href = "#download";
      meta.innerHTML = `Version ${esc(release.version)} · for Windows, macOS and Linux`;
    } else {
      const mine = release.files[here.os];
      $("#hero-download-label").textContent = `Download for ${OS[here.os].name}`;
      hero.href = url(mine);
      const others = here.os === "windows" ? "macOS and Linux" : here.os === "mac" ? "Windows and Linux" : "Windows and macOS";
      meta.innerHTML = `Version ${esc(release.version)} · ${mb(mine.size)} · ${esc(OS[here.os].meta)} · <a href="#download">${others}</a>`;
    }
  }

  const radios = $$(".os-switch [role=radio]");
  radios.forEach((b, i) => {
    b.addEventListener("click", () => {
      chosen = b.dataset.os;
      paint();
    });
    b.addEventListener("keydown", (e) => {
      const step = { ArrowRight: 1, ArrowDown: 1, ArrowLeft: -1, ArrowUp: -1 }[e.key];
      if (!step) return;
      e.preventDefault();
      const next = radios[(i + step + radios.length) % radios.length];
      chosen = next.dataset.os;
      paint();
      next.focus();
    });
  });

  if (here.phone) {
    $("#phone-note").hidden = false;
    $("#send-link").addEventListener("click", async () => {
      const page = { title: "Kairo: free instant replay", url: location.origin + location.pathname };
      try {
        if (navigator.share) await navigator.share(page);
        else {
          await navigator.clipboard.writeText(page.url);
          $("#send-link").textContent = "Link copied";
        }
      } catch {
        /* the visitor closed the share sheet */
      }
    });
  }
  paint();

  // The newest release, if GitHub answers; the links above stand otherwise.
  fetch(`https://api.github.com/repos/${REPO}/releases/latest`, { headers: { Accept: "application/vnd.github+json" } })
    .then((r) => (r.ok ? r.json() : Promise.reject(r.status)))
    .then((rel) => {
      const files = {};
      for (const [key, suffix] of Object.entries(SUFFIX)) {
        const a = rel.assets.find((x) => x.name.startsWith("kairo-") && x.name.endsWith(suffix));
        if (!a) return;
        files[key] = { name: a.name, size: a.size };
      }
      files.sums = { name: "SHA256SUMS.txt", size: 0 };
      release.version = String(rel.tag_name).replace(/^v/, "");
      release.date = rel.published_at ? new Date(rel.published_at) : null;
      release.files = files;
      paint();
    })
    .catch(() => {});

  // ---------- Navigation ----------
  const nav = $("#nav");
  const onScroll = () => nav.classList.toggle("scrolled", scrollY > 24);
  addEventListener("scroll", onScroll, { passive: true });
  onScroll();

  // ---------- Hero video and Kairo's recording panel ----------
  const video = $("#hero-video");
  const toggle = $("#vid-toggle");
  const hud = $("#hud");
  const fill = $("#hud-fill");
  const PLAY = 6.2; // seconds of play before the loop rewinds to its first frame
  let userPaused = reduced;
  let inView = true;

  function setPaused(paused) {
    toggle.setAttribute("aria-pressed", String(paused));
    $("#vid-toggle-label").textContent = paused ? "Play the video" : "Pause video";
  }
  function sync() {
    if (userPaused || !inView) video.pause();
    else video.play().catch(() => {});
  }
  toggle.addEventListener("click", () => {
    userPaused = !userPaused;
    setPaused(userPaused);
    sync();
  });
  if (reduced) {
    video.removeAttribute("autoplay");
    video.pause();
  }
  setPaused(userPaused);
  new IntersectionObserver(([e]) => {
    inView = e.isIntersecting;
    sync();
  }).observe($(".hero"));

  let rewinding = null;
  function tick() {
    const t = video.currentTime || 0;
    const back = t > PLAY;
    fill.style.transform = `scaleX(${back ? Math.max(0, 1 - (t - PLAY) / (video.duration - PLAY || 1.5)) : Math.min(1, t / PLAY)})`;
    if (back !== rewinding) {
      rewinding = back;
      hud.classList.toggle("rewind", back);
      $("#hud-state").textContent = back ? "Rewinding" : "Recording";
    }
    requestAnimationFrame(tick);
  }
  requestAnimationFrame(tick);

  // ---------- F8: the save ----------
  const toast = $("#toast");
  const film = $("#film");
  const keycap = $("#keycap");
  const sprite = new Image();
  sprite.src = "media/site/film.jpg";
  let hideTimer = null;
  let filmTimer = null;

  function drawShot() {
    const c = $("#toast-shot");
    const ctx = c.getContext("2d");
    try {
      if (video.readyState >= 2 && inView) {
        ctx.drawImage(video, 0, 0, c.width, c.height);
        return;
      }
      if (sprite.complete && sprite.naturalWidth) {
        ctx.drawImage(sprite, sprite.naturalWidth - 320, 0, 320, 180, 0, 0, c.width, c.height);
        return;
      }
    } catch {
      /* fall through to a plain tile */
    }
    ctx.fillStyle = "#23262d";
    ctx.fillRect(0, 0, c.width, c.height);
  }

  function save() {
    if (inView && !reduced) {
      const f = $("#hero-flash");
      f.classList.remove("go");
      void f.offsetWidth;
      f.classList.add("go");
    }
    film.classList.add("kept");
    clearTimeout(filmTimer);
    filmTimer = setTimeout(() => film.classList.remove("kept"), 2600);
    drawShot();
    toast.hidden = false;
    hud.classList.add("under");
    requestAnimationFrame(() => toast.classList.add("show"));
    clearTimeout(hideTimer);
    hideTimer = setTimeout(close, 7000);
  }
  function close() {
    toast.classList.remove("show");
    hud.classList.remove("under");
    setTimeout(() => {
      if (!toast.classList.contains("show")) toast.hidden = true;
    }, 480);
  }
  function pressKey() {
    keycap.classList.add("down");
    setTimeout(() => keycap.classList.remove("down"), 140);
  }

  addEventListener("keydown", (e) => {
    if (e.key !== "F8" || e.repeat) return;
    e.preventDefault();
    pressKey();
    save();
  });
  keycap.addEventListener("click", save);
  $("#hud-key").addEventListener("click", save);
  $("#toast-close").addEventListener("click", close);
  if (matchMedia("(pointer: coarse)").matches) $("#press-hint").textContent = "Tap it to save the last 30 seconds.";

  // ---------- Replays: each plays while it's on screen (or when clicked, with reduced motion) ----------
  const replays = $$(".replay video");
  const load = (v) => {
    if (!v.src && v.dataset.src) v.src = v.dataset.src;
  };
  if (!reduced && "IntersectionObserver" in window) {
    const seenReplays = new IntersectionObserver(
      (entries) =>
        entries.forEach((e) => {
          const v = e.target;
          if (e.isIntersecting) {
            load(v);
            v.play().catch(() => {});
          } else {
            v.pause();
          }
        }),
      { threshold: 0.5 },
    );
    replays.forEach((v) => seenReplays.observe(v));
  }
  $$(".replay-media").forEach((b) =>
    b.addEventListener("click", () => {
      const v = $("video", b);
      load(v);
      if (v.paused) v.play().catch(() => {});
      else v.pause();
    }),
  );

  // ---------- The app, tab by tab ----------
  const tabs = $$('[role="tab"]');
  function select(tab, focus) {
    tabs.forEach((t) => {
      const on = t === tab;
      t.setAttribute("aria-selected", String(on));
      t.tabIndex = on ? 0 : -1;
      const panel = document.getElementById(t.getAttribute("aria-controls"));
      panel.hidden = !on;
      if (on && !reduced) {
        panel.classList.remove("enter");
        void panel.offsetWidth;
        panel.classList.add("enter");
      }
    });
    if (focus) tab.focus();
  }
  tabs.forEach((t, i) => {
    t.addEventListener("click", () => select(t, false));
    t.addEventListener("keydown", (e) => {
      const to = { ArrowRight: i + 1, ArrowLeft: i - 1, Home: 0, End: tabs.length - 1 }[e.key];
      if (to === undefined) return;
      e.preventDefault();
      select(tabs[(to + tabs.length) % tabs.length], true);
    });
  });

  // ---------- The hero's footage goes soft as you scroll past it ----------
  const heroVideo = $("#hero-video");
  if (!reduced && heroVideo) {
    let ticking = false;
    let last = -1;
    const soften = () => {
      ticking = false;
      const p = Math.min(1, Math.max(0, scrollY / (heroVideo.offsetHeight || innerHeight)));
      const step = Math.round(p * 100);
      if (step === last) return;
      last = step;
      if (step === 0) {
        heroVideo.style.filter = "";
        heroVideo.style.transform = "";
      } else {
        heroVideo.style.filter = `blur(${(p * 14).toFixed(1)}px) brightness(${(1 - p * 0.35).toFixed(3)})`;
        heroVideo.style.transform = `scale(${(1 + p * 0.05).toFixed(4)})`;
      }
    };
    addEventListener(
      "scroll",
      () => {
        if (!ticking) {
          ticking = true;
          requestAnimationFrame(soften);
        }
      },
      { passive: true },
    );
    soften();
  }

  // ---------- Entrances, once, in reading order ----------
  if (!reduced && "IntersectionObserver" in window) {
    const groups = [
      ".facts li", ".press-key", ".press-copy > *", ".film", ".tour-head > *", ".tabs", ".panels",
      ".replays-head > *", ".replay", ".duo-item", ".features .h2", ".feat li", ".get-head > *", ".pick", ".others",
      ".faq h2", ".qs details", ".invite", ".cols", ".fine",
    ];
    const seen = new Map();
    $$(groups.join(",")).forEach((el) => {
      const box = el.closest("section, footer");
      const n = seen.get(box) || 0;
      seen.set(box, n + 1);
      el.classList.add("reveal");
      el.style.setProperty("--d", `${Math.min(n, 8) * 70}ms`);
    });
    const io = new IntersectionObserver(
      (entries) =>
        entries.forEach((e) => {
          if (!e.isIntersecting) return;
          e.target.classList.add("in");
          io.unobserve(e.target);
        }),
      { rootMargin: "0px 0px -10% 0px" },
    );
    $$(".reveal, .close").forEach((el) => io.observe(el));
  }
})();
