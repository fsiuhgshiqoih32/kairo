// Kairo's site: picks the download for this computer, fills in the latest release, and runs the
// page's small live pieces. Everything works as plain links without it.
(() => {
  "use strict";
  const $ = (s, el = document) => el.querySelector(s);
  const $$ = (s, el = document) => [...el.querySelectorAll(s)];
  const reduced = matchMedia("(prefers-reduced-motion: reduce)").matches;
  const esc = (s) => String(s).replace(/[&<>"]/g, (c) => ({ "&": "&amp;", "<": "&lt;", ">": "&gt;", '"': "&quot;" })[c]);

  // ---------- Downloads ----------
  const REPO = "fsiuhgshiqoih32/kairo";
  const release = {
    version: "0.2.0",
    files: {
      windows: { name: "kairo-v0.2.0-windows-x64-setup.exe", size: 31914499 },
      portable: { name: "kairo-v0.2.0-windows-x64-portable.zip", size: 15543795 },
      mac: { name: "kairo-v0.2.0-macos-universal.zip", size: 25339367 },
      linux: { name: "kairo-v0.2.0-linux-x86_64-portable.tar.gz", size: 15746029 },
    },
  };
  const SUFFIX = {
    windows: "-windows-x64-setup.exe",
    portable: "-windows-x64-portable.zip",
    mac: "-macos-universal.zip",
    linux: "-linux-x86_64-portable.tar.gz",
  };
  const NAME = { windows: "Windows", mac: "macOS", linux: "Linux" };
  const url = (f) => `https://github.com/${REPO}/releases/download/v${release.version}/${f.name}`;
  const mb = (bytes) => `${Math.round(bytes / 1e6)} MB`;

  function detect() {
    const p = (navigator.userAgentData && navigator.userAgentData.platform) || navigator.platform || "";
    const ua = navigator.userAgent || "";
    if (/android|iphone|ipad|ipod/i.test(ua) || (/Mac/.test(p) && navigator.maxTouchPoints > 1)) {
      return { os: "windows", phone: true };
    }
    if (/mac/i.test(p) || /Mac OS X/.test(ua)) return { os: "mac", phone: false };
    if (/linux|x11/i.test(p) || /Linux|X11/.test(ua)) return { os: "linux", phone: false };
    return { os: "windows", phone: false };
  }
  const here = detect();

  function note(os) {
    const f = release.files[os];
    const file = `<span class="file">${esc(f.name)}</span> (${mb(f.size)})`;
    const lead = here.phone ? "Kairo runs on computers, so send yourself this page and open it there. " : "";
    if (os === "mac") {
      return `${lead}${file} holds Kairo.app for Apple silicon and Intel, macOS 11 or newer. Move it to Applications; the first time, right-click it and choose Open, then allow Screen Recording.`;
    }
    if (os === "linux") {
      return `${lead}${file} is for x86_64 with X11. Unpack it and run ./kairo; desktop sound needs parec from PulseAudio or PipeWire.`;
    }
    return `${lead}The installer, ${file}, sets Kairo up for you without administrator rights. It isn’t code-signed yet, so Windows may ask: choose More info, then Run anyway.`;
  }

  const os = $("#os");
  function choose(which) {
    os.value = which;
    $("#go").href = url(release.files[which]);
    $("#get-note").innerHTML = note(which);
    const hero = $("#hero-download");
    if (here.phone) {
      hero.textContent = "Get Kairo for your computer";
      hero.href = "#download";
    } else {
      hero.textContent = `Download for ${NAME[here.os]}`;
      hero.href = url(release.files[here.os]);
    }
    $("#alt-portable").href = url(release.files.portable);
    $("#alt-mac").href = url(release.files.mac);
    $("#alt-linux").href = url(release.files.linux);
    $$("[data-version]").forEach((el) => (el.textContent = release.version));
  }
  os.addEventListener("change", () => choose(os.value));
  choose(here.os);

  // The newest release, if GitHub answers; the 0.2.0 links above stand otherwise.
  fetch(`https://api.github.com/repos/${REPO}/releases/latest`, { headers: { Accept: "application/vnd.github+json" } })
    .then((r) => (r.ok ? r.json() : Promise.reject(r.status)))
    .then((rel) => {
      const files = {};
      for (const [key, suffix] of Object.entries(SUFFIX)) {
        const a = rel.assets.find((x) => x.name.startsWith("kairo-") && x.name.endsWith(suffix));
        if (!a) return;
        files[key] = { name: a.name, size: a.size };
      }
      release.version = rel.tag_name.replace(/^v/, "");
      release.files = files;
      choose(os.value);
    })
    .catch(() => {});

  // ---------- Searching the sample clips ----------
  const CLIPS = [
    { img: "clip3", title: "Axe through the roof", meta: "Minecraft · 0:30 · today", size: "72 MB", tags: ["clutch", "build"], alt: "an axe breaking through a wooden roof at night" },
    { img: "clip6", title: "Down the mine in four hits", meta: "Minecraft · 0:15 · yesterday", size: "31 MB", tags: ["clutch"], alt: "a diamond axe at a log beside a stone wall" },
    { img: "clip2", title: "Last log before sunrise", meta: "Minecraft · 0:13 · trimmed", size: "26 MB", tags: ["clutch", "trimmed"], alt: "a log breaking apart against the night sky" },
    { img: "clip1", title: "Greenhouse frame, done", meta: "Minecraft · 0:30 · today", size: "64 MB", tags: ["build"], alt: "a wooden frame around a greenhouse at dusk" },
    { img: "clip4", title: "Night over the lake", meta: "Minecraft · 0:30 · yesterday", size: "63 MB", tags: ["lake"], alt: "beams of a half-built house over hills at night" },
    { img: "clip5", title: "Deck down to the water", meta: "Minecraft · 0:21 · trimmed", size: "44 MB", tags: ["build", "lake", "trimmed"], alt: "a wooden deck on grass" },
  ];
  function preview(c) {
    if (!c) return;
    const img = $("#preview-img");
    img.src = `media/site/${c.img}.jpg`;
    img.alt = `A frame from the clip: ${c.alt}`;
    $("#preview-title").textContent = c.title;
    $("#preview-meta").textContent = `${c.meta} · ${c.size}`;
    $$("#rows li").forEach((li) => li.classList.toggle("sel", li.dataset.img === c.img));
  }
  const q = $("#q");
  const rows = $("#rows");
  function search() {
    const words = q.value.toLowerCase().split(/\s+/).filter(Boolean);
    const hits = CLIPS.filter((c) => {
      const hay = `${c.title} ${c.meta} ${c.tags.join(" ")}`.toLowerCase();
      return words.every((w) => hay.includes(w));
    }).slice(0, 3);
    rows.innerHTML = hits.length
      ? hits.map((c) => `<li tabindex="0" data-img="${c.img}"><img src="media/site/${c.img}.jpg" alt="" width="128" height="72"><div><b>${esc(c.title)}</b><span>${esc(c.meta)}</span></div><em>${esc(c.tags[0])}</em></li>`).join("")
      : `<li class="none">No clips match “${esc(q.value)}”. Kairo searches names, games and tags.</li>`;
    const n = CLIPS.filter((c) => words.every((w) => `${c.title} ${c.meta} ${c.tags.join(" ")}`.toLowerCase().includes(w))).length;
    preview(hits[0]);
    $("#answer-note").textContent = words.length
      ? `${n} ${n === 1 ? "clip" : "clips"}. Kairo names each clip after the game and keeps its tags, so search finds them.`
      : "Type a game, a name or a tag.";
  }
  q.addEventListener("input", search);
  // Pointing at a result, or moving to it with Tab, shows it larger, like Kairo's hover preview.
  const pick = (e) => {
    const li = e.target.closest("li[data-img]");
    if (li) preview(CLIPS.find((c) => c.img === li.dataset.img));
  };
  rows.addEventListener("mouseover", pick);
  rows.addEventListener("focusin", pick);
  preview(CLIPS[0]);
  $$(".examples button").forEach((b) =>
    b.addEventListener("click", () => {
      q.value = b.dataset.q;
      search();
      q.focus();
    }),
  );

  // ---------- Switches ----------
  $$(".band .switch").forEach((sw) =>
    sw.addEventListener("click", () => {
      const band = sw.closest(".band");
      const on = band.dataset.on !== "true";
      band.dataset.on = String(on);
      sw.setAttribute("aria-checked", String(on));
    }),
  );

  // ---------- Stages ----------
  const STAGES = [
    ["It’s already recording.", "The replay runs on your graphics card’s own encoder and lives in memory, so games keep their frames."],
    ["Then you press F8.", "Any time, even in full-screen games. Up to three more shortcuts can save other lengths, up to 20 minutes."],
    ["Saved in a second.", "The clip is cut from what’s already encoded, named after the game, with game, mic and voice chat on their own tracks."],
    ["Trim it, then share it.", "Trim it, slow it down, add captions or music, then make a link anyone can watch, with a QR code for phones."],
  ];
  const stage = $("#stage");
  const frags = $$(".frag");
  const stops = $$(".stops li");
  const fill = $(".slider .fill");
  let touched = false;
  function show(i) {
    stage.value = i;
    $("#how-title").textContent = STAGES[i][0];
    $("#clause-note").textContent = STAGES[i][1];
    frags.forEach((f) => f.classList.toggle("on", Number(f.dataset.stage) === i));
    stops.forEach((s, k) => s.classList.toggle("on", k === i));
    if (fill) fill.style.transform = `scaleX(${i / 3})`;
  }
  stage.addEventListener("input", () => {
    touched = true;
    show(Number(stage.value));
  });
  stops.forEach((s, k) =>
    s.addEventListener("click", () => {
      touched = true;
      show(k);
    }),
  );
  show(0);
  // It steps through on its own while in view, until someone takes the slider.
  let timer = null;
  const how = $("#how");
  new IntersectionObserver(([e]) => {
    clearInterval(timer);
    if (e.isIntersecting && !reduced) {
      timer = setInterval(() => {
        if (touched) return clearInterval(timer);
        show((Number(stage.value) + 1) % 4);
      }, 3600);
    }
  }, { threshold: 0.4 }).observe(how);

  // ---------- The live part: F8 saves "this page" ----------
  const started = performance.now();
  const toast = $("#toast");
  let hide = null;
  function save() {
    const secs = Math.min(30, Math.max(1, Math.round((performance.now() - started) / 1000)));
    $("#toast-len").textContent = `0:${String(secs).padStart(2, "0")}`;
    const shot = $("#toast-shot");
    const video = $("#strip");
    const ctx = shot.getContext("2d");
    try {
      ctx.drawImage(video, 0, 0, shot.width, shot.height);
    } catch {
      ctx.fillStyle = "#262930";
      ctx.fillRect(0, 0, shot.width, shot.height);
    }
    toast.hidden = false;
    requestAnimationFrame(() => toast.classList.add("show"));
    clearTimeout(hide);
    hide = setTimeout(close, 7000);
  }
  function close() {
    toast.classList.remove("show");
    setTimeout(() => {
      if (!toast.classList.contains("show")) toast.hidden = true;
    }, 450);
  }
  addEventListener("keydown", (e) => {
    if (e.key === "F8") {
      e.preventDefault();
      save();
    }
  });
  $("#try").addEventListener("click", save);
  $("#toast-close").addEventListener("click", close);
  if (here.phone || matchMedia("(pointer: coarse)").matches) {
    $("#try").innerHTML = "Try it: tap for <kbd>F8</kbd>";
  }

  // ---------- Entrances, once, in reading order ----------
  const groups = new Map();
  $$(".reveal").forEach((el) => {
    const sec = el.closest("section, footer");
    const list = groups.get(sec) || [];
    el.style.setProperty("--d", `${list.length * 80}ms`);
    list.push(el);
    groups.set(sec, list);
  });
  const io = new IntersectionObserver(
    (entries) =>
      entries.forEach((e) => {
        if (!e.isIntersecting) return;
        e.target.classList.add("in");
        // The end of the entrance, set on the element so the transition runs from the CSS start.
        if (!reduced && e.target.classList.contains("reveal")) {
          e.target.style.opacity = "1";
          e.target.style.transform = "none";
        }
        io.unobserve(e.target);
      }),
    { rootMargin: "0px 0px -12% 0px" },
  );
  $$(".reveal, .close").forEach((el) => io.observe(el));
})();
