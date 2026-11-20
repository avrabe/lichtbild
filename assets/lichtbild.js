// lichtbild timeline: size the date rail's month ticks by item count and
// mark the month in view. Served from lichtbild itself (same-origin CSP);
// the rail works as plain links without it.
(function () {
  "use strict";

  function sizeTicks(rail) {
    rail.querySelectorAll("a.m").forEach(function (t) {
      var n = parseInt(t.getAttribute("data-n"), 10) || 1;
      // Square root keeps busy months from crowding out quiet ones.
      t.style.flexGrow = String(Math.max(1, Math.sqrt(n)));
    });
  }

  function markActive(rail, ym) {
    var current = rail.querySelector("a.m.on");
    if (current && current.getAttribute("data-ym") === ym) return;
    if (current) current.classList.remove("on");
    var next = rail.querySelector('a.m[data-ym="' + ym + '"]');
    if (next) next.classList.add("on");
  }

  // The chooser's button names the month in view; its year opens.
  var MONTHS = ["Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec"];
  function markChooser(menu, ym) {
    var now = menu.querySelector(".jnow");
    var m = parseInt(ym.slice(5, 7), 10);
    if (now && m >= 1 && m <= 12) now.textContent = MONTHS[m - 1] + " " + ym.slice(0, 4);
    var cur = menu.querySelector(".months a.on");
    if (cur) cur.classList.remove("on");
    var next = menu.querySelector('.months a[data-ym="' + ym + '"]');
    if (next) next.classList.add("on");
    menu.setAttribute("data-ym", ym);
  }

  function init() {
    var rail = document.querySelector("nav.rail");
    var menu = document.querySelector("details.jumpmenu");
    if (!rail && !menu) return;
    if (rail) sizeTicks(rail);
    if (menu) {
      // Opening the chooser opens the year in view, closes the others.
      menu.addEventListener("toggle", function () {
        if (!menu.open) return;
        var y = (menu.getAttribute("data-ym") || "").slice(0, 4);
        if (!y) return;
        menu.querySelectorAll("details.jyear").forEach(function (d) {
          d.open = d.getAttribute("data-y") === y;
          if (d.open) d.scrollIntoView({ block: "nearest" });
        });
      });
    }
    if (!("IntersectionObserver" in window)) return;
    var io = new IntersectionObserver(
      function (entries) {
        entries.forEach(function (e) {
          if (!e.isIntersecting) return;
          var ym = e.target.textContent.slice(0, 7);
          if (rail) markActive(rail, ym);
          if (menu) markChooser(menu, ym);
        });
      },
      { rootMargin: "0px 0px -80% 0px" }
    );
    function observe(root) {
      root.querySelectorAll("h4.day:not([data-seen])").forEach(function (h) {
        h.setAttribute("data-seen", "");
        io.observe(h);
      });
    }
    observe(document);
    // Days loaded later by htmx's infinite scroll.
    document.body.addEventListener("htmx:afterSwap", function () {
      observe(document);
    });
  }

  if (document.readyState === "loading") {
    document.addEventListener("DOMContentLoaded", init);
  } else {
    init();
  }
})();

// Viewer: arrow keys and horizontal swipes follow the prev/next links,
// Escape goes back to the timeline. Swipe left = next (older) photo, swipe
// right = previous (newer), as in phone galleries.
(function () {
  "use strict";
  var viewer = document.querySelector(".viewer");
  if (!viewer) return;
  function go(sel) {
    var a = viewer.querySelector(sel);
    if (a && a.href) window.location.href = a.href;
  }
  // Keys and swipes that start on a video belong to its controls (seek,
  // volume, scrubbing), not to the photo navigation.
  function onVideo(t) { return t && t.closest && t.closest("video"); }
  document.addEventListener("keydown", function (e) {
    if (e.altKey || e.ctrlKey || e.metaKey) return;
    if (onVideo(e.target) && e.key !== "Escape") return;
    // Typing in the search box is not navigation.
    if (e.target && e.target.closest && e.target.closest("input, textarea, .suggest")) return;
    if (e.key === "ArrowLeft") go("a.prev");
    else if (e.key === "ArrowRight") go("a.next");
    else if (e.key === "Escape") go("a.back");
    else if (e.key === "i") {
      var d = viewer.querySelector("details.info");
      if (d) d.open = !d.open;
    } else if (e.key === "t") {
      var tr = viewer.querySelector("details.transcript");
      if (tr) tr.open = !tr.open;
    } else if (e.key === "s") {
      go("a.slideshow");
    } else if (e.key === ".") {
      var h = viewer.querySelector("form.favf button");
      if (h) h.click();
    }
  });
  var x0 = null, y0 = null, t0 = 0, top0 = false;
  var stage = viewer.querySelector(".stage") || viewer;
  stage.addEventListener("touchstart", function (e) {
    if (e.touches.length !== 1 || onVideo(e.target) || viewer.classList.contains("zoomed")) { x0 = null; return; }
    x0 = e.touches[0].clientX; y0 = e.touches[0].clientY; t0 = Date.now();
    top0 = window.scrollY <= 0;
  }, { passive: true });
  stage.addEventListener("touchend", function (e) {
    if (x0 === null || viewer.classList.contains("zoomed")) { x0 = null; return; }
    var dx = e.changedTouches[0].clientX - x0;
    var dy = e.changedTouches[0].clientY - y0;
    x0 = null;
    // A deliberate, mostly horizontal swipe; pinch-zoom and scrolling pass.
    if (Math.abs(dx) > 50 && Math.abs(dx) > 2 * Math.abs(dy) && Date.now() - t0 < 800) {
      go(dx < 0 ? "a.next" : "a.prev");
    } else if (top0 && dy > 90 && dy > 2 * Math.abs(dx) && Date.now() - t0 < 800) {
      // Pulled down from the top of the page: back to the timeline.
      go("a.back");
    }
  }, { passive: true });
})();

// Favourite (REQ-WEB-016): the heart toggles in place; without
// JavaScript the form posts and the page comes back.
(function () {
  "use strict";
  var form = document.querySelector(".viewer form.favf");
  if (!form || !window.fetch) return;
  form.addEventListener("submit", function (e) {
    e.preventDefault();
    var b = form.querySelector("button");
    var on = b.classList.contains("on");
    b.disabled = true;
    fetch(form.action, {
      method: "POST",
      credentials: "same-origin",
      redirect: "manual",
      headers: { "content-type": "application/x-www-form-urlencoded" },
      body: new URLSearchParams(new FormData(form)).toString()
    }).then(function (r) {
      if (!r.ok && r.type !== "opaqueredirect") throw new Error(r.status);
      on = !on;
      b.classList.toggle("on", on);
      b.setAttribute("aria-pressed", on ? "true" : "false");
      b.textContent = on ? "\u2665" : "\u2661";
      form.action = form.action.replace(on ? /\/fav\/(\d+)$/ : /\/unfav\/(\d+)$/, on ? "/unfav/$1" : "/fav/$1");
    }).catch(function () {}).then(function () { b.disabled = false; });
  });
})();

// Add to album in the viewer's details: the name field only for a new one.
(function () {
  "use strict";
  var f = document.querySelector(".viewer form.albumf");
  if (!f) return;
  var sel = f.querySelector("select"), name = f.querySelector("input[name=name]");
  function sync() {
    name.hidden = sel.value !== "new";
    name.required = sel.value === "new";
  }
  sel.addEventListener("change", sync);
  sync();
})();

// Share (iPad, iPhone, some desktops): the original goes to the system
// share sheet, so it can be saved to Photos or sent. Fetching it may use
// up the tap that allows sharing; then a second tap shares.
(function () {
  "use strict";
  var b = document.querySelector(".viewer button.share");
  if (!b || !navigator.canShare || !window.fetch) return;
  var file = null;
  b.hidden = false;
  function share() {
    navigator.share({ files: [file] }).then(function () { b.textContent = "Share\u2026"; }, function (err) {
      b.textContent = err && err.name === "NotAllowedError" ? "Tap again to share" : "Share\u2026";
    });
  }
  b.addEventListener("click", function () {
    if (file) { share(); return; }
    b.textContent = "Preparing\u2026";
    fetch(b.getAttribute("data-src"), { credentials: "same-origin" }).then(function (r) {
      if (!r.ok) throw new Error(r.status);
      return r.blob();
    }).then(function (blob) {
      var f = new File([blob], b.getAttribute("data-name") || "picture", { type: blob.type });
      if (!navigator.canShare({ files: [f] })) { b.hidden = true; return; }
      file = f;
      share();
    }).catch(function () { b.textContent = "Could not share"; });
  });
})();

// Transcript: a click on a time seeks the video instead of reloading.
(function () {
  "use strict";
  var viewer = document.querySelector(".viewer");
  if (!viewer) return;
  viewer.addEventListener("click", function (e) {
    var a = e.target.closest && e.target.closest("a.ts");
    var v = viewer.querySelector("video");
    if (!a || !v) return;
    e.preventDefault();
    v.currentTime = parseFloat(a.getAttribute("data-t")) || 0;
    v.play();
  });
})();

// Viewer: full screen (button or f) for photos and videos, and zoom for
// photos: wheel or trackpad pinch at the pointer, double click or double
// tap between fit and 100 %, drag to pan, two-finger pinch on touch, keys
// + - 0. The original is shown at its full resolution when the browser can
// display it (HEIC: the 2048 px preview).
(function () {
  "use strict";
  var viewer = document.querySelector(".viewer");
  if (!viewer) return;
  var fs = viewer.querySelector("button.fs");
  if (fs && document.fullscreenEnabled) {
    fs.hidden = false;
    fs.addEventListener("click", toggleFull);
  }
  function toggleFull() {
    if (document.fullscreenElement) document.exitFullscreen();
    else if (viewer.requestFullscreen) viewer.requestFullscreen();
  }
  var img = viewer.querySelector("img.photo");
  document.addEventListener("keydown", function (e) {
    if (e.altKey || e.ctrlKey || e.metaKey) return;
    if (e.target && e.target.closest && e.target.closest("video, input")) return;
    if (e.key === "f" && document.fullscreenEnabled) { toggleFull(); return; }
    if (!img) return;
    var r = img.getBoundingClientRect();
    var cx = r.left + r.width / 2, cy = r.top + r.height / 2;
    if (e.key === "+" || e.key === "=") zoomAt(scale * 1.5, cx, cy);
    else if (e.key === "-") zoomAt(scale / 1.5, cx, cy);
    else if (e.key === "0") reset();
  });
  if (!img) return;

  var scale = 1, tx = 0, ty = 0;
  // Boxes over the photo (REQ-TEXT-001, REQ-SEE-001): lines of text and
  // things found, each layer shown while its panel is open (text also
  // while the panels sit below the picture); they follow the zoom.
  var layers = Array.prototype.map.call(viewer.querySelectorAll(".boxlayer"), function (layer) {
    return { el: layer, panel: viewer.querySelector("details." + layer.getAttribute("data-for")) };
  });
  function placeLayer(layer) {
    var W = img.offsetWidth, H = img.offsetHeight;
    layer.style.left = img.offsetLeft + "px";
    layer.style.top = img.offsetTop + "px";
    layer.style.width = W + "px";
    layer.style.height = H + "px";
    layer.style.transform = img.style.transform;
    layer.querySelectorAll("a[data-quad]").forEach(function (a) {
      var q = a.getAttribute("data-quad").split(",").map(Number);
      // Corners: top left, top right, bottom right, bottom left.
      var x0 = q[0] * W, y0 = q[1] * H, x1 = q[2] * W, y1 = q[3] * H, x3 = q[6] * W, y3 = q[7] * H;
      var w = Math.hypot(x1 - x0, y1 - y0), h = Math.hypot(x3 - x0, y3 - y0);
      a.style.left = x0 + "px";
      a.style.top = y0 + "px";
      a.style.width = w + "px";
      a.style.height = h + "px";
      a.style.transform = "rotate(" + Math.atan2(y1 - y0, x1 - x0) + "rad)";
    });
  }
  function showLayers() {
    layers.forEach(function (l) {
      var below = viewer.classList.contains("below") && l.el.getAttribute("data-for") === "textp";
      l.el.hidden = !(below || (l.panel && l.panel.open));
      if (!l.el.hidden) placeLayer(l.el);
    });
  }
  layers.forEach(function (l) {
    if (l.panel) l.panel.addEventListener("toggle", showLayers);
  });
  viewer.addEventListener("lb-layout", showLayers);
  // A find in "Something wrong?" lights up its box (REQ-FIX-001).
  viewer.querySelectorAll("details.things li[data-n]").forEach(function (li) {
    var box = viewer.querySelector(".thinglayer a[data-n=\"" + li.getAttribute("data-n") + "\"]");
    if (!box) return;
    function on() { box.classList.add("hot"); }
    function off() { box.classList.remove("hot"); }
    li.addEventListener("mouseenter", on);
    li.addEventListener("mouseleave", off);
    li.addEventListener("focusin", on);
    li.addEventListener("focusout", off);
  });
  if (img.complete) showLayers(); else img.addEventListener("load", showLayers);
  window.addEventListener("resize", showLayers);
  function apply() {
    img.style.transform = "translate(" + tx + "px," + ty + "px) scale(" + scale + ")";
    viewer.classList.toggle("zoomed", scale > 1.001);
    layers.forEach(function (l) {
      if (!l.el.hidden) l.el.style.transform = img.style.transform;
    });
  }
  function maxScale() {
    // At least 4x, and enough to show every pixel of the original.
    var w = img.getBoundingClientRect().width / scale;
    return Math.max(4, (img.naturalWidth || 0) / Math.max(w, 1));
  }
  // Keep the image point under (cx, cy) in place while the scale changes.
  // The transform origin is the image's top left corner (CSS).
  function zoomAt(s, cx, cy) {
    s = Math.min(Math.max(s, 1), maxScale());
    var r = img.getBoundingClientRect();
    var lx = r.left - tx, ly = r.top - ty;
    var qx = (cx - lx - tx) / scale, qy = (cy - ly - ty) / scale;
    scale = s;
    tx = cx - lx - s * qx;
    ty = cy - ly - s * qy;
    if (scale <= 1.001) { scale = 1; tx = 0; ty = 0; }
    apply();
  }
  function reset() { scale = 1; tx = 0; ty = 0; apply(); }

  img.addEventListener("wheel", function (e) {
    e.preventDefault();
    zoomAt(scale * Math.exp(-e.deltaY * 0.0025), e.clientX, e.clientY);
  }, { passive: false });
  img.addEventListener("dblclick", function (e) {
    if (scale > 1.001) reset();
    // 100 %: one image pixel per screen pixel, at least twice the fit.
    else zoomAt(Math.max(2, (img.naturalWidth || 0) / img.getBoundingClientRect().width), e.clientX, e.clientY);
  });

  // Pointers: one drags when zoomed, two pinch.
  var pts = {}, last = null, pinch = null, tapAt = 0;
  img.addEventListener("pointerdown", function (e) {
    pts[e.pointerId] = { x: e.clientX, y: e.clientY };
    img.setPointerCapture(e.pointerId);
    var ids = Object.keys(pts);
    if (ids.length === 2) {
      var a = pts[ids[0]], b = pts[ids[1]];
      pinch = { d: Math.hypot(a.x - b.x, a.y - b.y), s: scale };
      last = null;
    } else if (ids.length === 1) {
      last = { x: e.clientX, y: e.clientY };
      // Double tap on touch screens (dblclick does not fire there).
      if (e.pointerType === "touch") {
        var now = Date.now();
        if (now - tapAt < 300) {
          if (scale > 1.001) reset(); else zoomAt(2.5, e.clientX, e.clientY);
          tapAt = 0;
        } else tapAt = now;
      }
    }
  });
  img.addEventListener("pointermove", function (e) {
    if (!pts[e.pointerId]) return;
    pts[e.pointerId] = { x: e.clientX, y: e.clientY };
    var ids = Object.keys(pts);
    if (pinch && ids.length === 2) {
      var a = pts[ids[0]], b = pts[ids[1]];
      var d = Math.hypot(a.x - b.x, a.y - b.y);
      zoomAt(pinch.s * d / Math.max(pinch.d, 1), (a.x + b.x) / 2, (a.y + b.y) / 2);
    } else if (last && scale > 1.001) {
      tx += e.clientX - last.x; ty += e.clientY - last.y;
      last = { x: e.clientX, y: e.clientY };
      apply();
    }
  });
  function up(e) {
    delete pts[e.pointerId];
    if (Object.keys(pts).length < 2) pinch = null;
    if (!Object.keys(pts).length) last = null;
  }
  img.addEventListener("pointerup", up);
  img.addEventListener("pointercancel", up);
  img.addEventListener("dragstart", function (e) { e.preventDefault(); });
})();

// A thumbnail that fails to load because the session ended (the server
// restarted for an upgrade) is a redirect to the login: reload the page,
// which goes through the login, instead of showing empty tiles.
(function () {
  "use strict";
  var checked = false;
  document.addEventListener("error", function (e) {
    var img = e.target;
    if (checked || !img || img.tagName !== "IMG" || !img.closest(".item, .mapstrip, .result")) return;
    checked = true;
    fetch(img.src, { redirect: "manual", credentials: "same-origin" }).then(function (r) {
      if (r.type === "opaqueredirect" || r.status === 401) window.location.reload();
      else checked = false;
    }, function () { checked = false; });
  }, true);
})();

// Transcript and text in the picture: copy them, and show them below the
// picture instead of over it (REQ-TEXT-001). The choice is kept in this
// browser only.
(function () {
  "use strict";
  var viewer = document.querySelector(".viewer");
  var under = viewer && viewer.querySelector("section.under");
  if (!under) return;
  var panels = Array.prototype.slice.call(viewer.querySelectorAll(".tpanel, .xpanel"));
  if (!panels.length) return;
  var homes = panels.map(function (p) { return p.parentNode; });
  var KEY = "lichtbild.below";
  function stored() {
    try { return window.localStorage.getItem(KEY) === "1"; } catch (e) { return false; }
  }
  function store(v) {
    try { window.localStorage.setItem(KEY, v ? "1" : "0"); } catch (e) { /* private mode */ }
  }
  function textOf(p) {
    if (p.classList.contains("tpanel")) {
      return Array.prototype.map.call(p.querySelectorAll("li"), function (li) {
        return li.textContent.trim();
      }).join("\n");
    }
    return Array.prototype.map.call(p.querySelectorAll(":scope > p:not(.dim), section > p"), function (para) {
      return Array.prototype.map.call(para.querySelectorAll("span"), function (s) { return s.textContent; }).join("\n");
    }).join("\n\n");
  }
  function copy(text, button) {
    function done(ok) {
      var was = "Copy";
      button.textContent = ok ? "Copied" : "Select and copy by hand";
      setTimeout(function () { button.textContent = was; }, 1600);
    }
    // The clipboard API needs a secure context; plain http (a house
    // address) falls back to a selected text area.
    if (navigator.clipboard && window.isSecureContext) {
      navigator.clipboard.writeText(text).then(function () { done(true); }, function () { done(false); });
      return;
    }
    var ta = document.createElement("textarea");
    ta.value = text;
    ta.setAttribute("readonly", "");
    ta.style.position = "fixed";
    ta.style.opacity = "0";
    document.body.appendChild(ta);
    ta.select();
    ta.setSelectionRange(0, text.length);
    var ok = false;
    try { ok = document.execCommand("copy"); } catch (e) { ok = false; }
    document.body.removeChild(ta);
    done(ok);
  }
  function setBelow(below) {
    viewer.classList.toggle("below", below);
    under.hidden = !below;
    panels.forEach(function (p, i) {
      var label = p.querySelector("button.place");
      if (below) {
        var box = document.createElement("div");
        box.className = "ublock";
        var h = document.createElement("h2");
        h.textContent = p.getAttribute("data-title") || "";
        box.appendChild(h);
        box.appendChild(p);
        under.appendChild(box);
        if (label) label.textContent = p.getAttribute("data-over") || "Show over";
      } else {
        homes[i].appendChild(p);
        if (label) label.textContent = p.getAttribute("data-below") || "Show below";
      }
    });
    if (!below) under.textContent = "";
    viewer.dispatchEvent(new Event("lb-layout"));
  }
  panels.forEach(function (p) {
    var tools = p.querySelector(".ptools");
    if (!tools) return;
    tools.hidden = false;
    tools.querySelector("button.copy").addEventListener("click", function (e) {
      copy(textOf(p), e.currentTarget);
    });
    tools.querySelector("button.place").addEventListener("click", function () {
      var below = !viewer.classList.contains("below");
      store(below);
      setBelow(below);
    });
  });
  if (stored()) setBelow(true);
})();

// Transcript: while the video plays, mark the line being said; in the
// floating panel, keep it in view (below the video the page is not moved).
(function () {
  "use strict";
  var viewer = document.querySelector(".viewer");
  var video = viewer && viewer.querySelector("video");
  var panel = viewer && viewer.querySelector(".tpanel");
  if (!video || !panel) return;
  var stamps = Array.prototype.map.call(panel.querySelectorAll("a.ts[data-t]"), function (a) {
    return { t: parseFloat(a.getAttribute("data-t")) || 0, li: a.closest("li") };
  });
  if (!stamps.length) return;
  var current = null;
  function mark() {
    var now = video.currentTime, found = null;
    for (var i = 0; i < stamps.length && stamps[i].t <= now + 0.25; i++) found = stamps[i].li;
    if (found === current) return;
    panel.querySelectorAll("li.now").forEach(function (li) { li.classList.remove("now"); });
    current = found;
    if (!current) return;
    current.classList.add("now");
    var floating = !viewer.classList.contains("below") && panel.scrollHeight > panel.clientHeight;
    if (floating) {
      var top = current.offsetTop - panel.clientHeight / 3;
      panel.scrollTo({ top: Math.max(0, top), behavior: "smooth" });
    }
  }
  video.addEventListener("timeupdate", mark);
  video.addEventListener("seeked", mark);
})();

// A search hit's time (?t=): start the video there. The #t= media fragment
// on the source is not enough on iPad Safari, which loads nothing before
// the first tap on play; so seek as soon as the video allows, and at the
// latest when it starts. The transcript opens at that line.
(function () {
  "use strict";
  var viewer = document.querySelector(".viewer");
  var video = viewer && viewer.querySelector("video");
  if (!video) return;
  var t = parseFloat(new URLSearchParams(window.location.search).get("t"));
  if (!(t > 0)) return;
  var pending = true;
  function seek() {
    if (!pending) return;
    if (Math.abs(video.currentTime - t) > 0.5) {
      try { video.currentTime = t; } catch (e) { return; }
    }
    pending = false;
  }
  ["loadedmetadata", "canplay", "play"].forEach(function (ev) {
    video.addEventListener(ev, seek);
  });
  if (video.readyState >= 1) seek();
  var details = viewer.querySelector("details.transcript");
  if (details && !viewer.classList.contains("below")) details.open = true;
  // Mark the line now, before the video has loaded.
  var best = null;
  viewer.querySelectorAll(".tpanel a.ts[data-t]").forEach(function (a) {
    if ((parseFloat(a.getAttribute("data-t")) || 0) <= t + 0.25) best = a.closest("li");
  });
  if (best) {
    best.classList.add("now");
    var panel = best.closest(".tpanel");
    if (panel && panel.scrollHeight > panel.clientHeight) panel.scrollTop = Math.max(0, best.offsetTop - panel.clientHeight / 3);
  }
})();


// Search suggestions (REQ-SEARCH-002): the list under the search box comes
// from the server as it is typed; arrow keys move through it, Escape or a
// click elsewhere closes it. On the results page the box keeps the query,
// so it can be refined instead of typed again.
(function () {
  "use strict";
  var input = document.querySelector("form.search input[name=q]");
  var box = document.getElementById("suggest");
  if (!input || !box) return;
  if (/\/search$/.test(location.pathname) && !input.value) {
    var q = new URLSearchParams(location.search).get("q");
    if (q) input.value = q;
  }
  function opts() { return Array.prototype.slice.call(box.querySelectorAll("a")); }
  function close() { box.innerHTML = ""; }
  input.addEventListener("keydown", function (e) {
    var o = opts();
    if (e.key === "ArrowDown" && o.length) { e.preventDefault(); o[0].focus(); }
    else if (e.key === "Escape") close();
  });
  box.addEventListener("keydown", function (e) {
    var o = opts(), i = o.indexOf(document.activeElement);
    if (e.key === "ArrowDown") { e.preventDefault(); (o[i + 1] || o[i]).focus(); }
    else if (e.key === "ArrowUp") { e.preventDefault(); if (i <= 0) input.focus(); else o[i - 1].focus(); }
    else if (e.key === "Escape") { close(); input.focus(); }
  });
  document.addEventListener("click", function (e) {
    if (!(e.target.closest && e.target.closest("form.search"))) close();
  });
  // The form is sent as typed; a stale list must not cover the results.
  input.form.addEventListener("submit", close);
})();

// Series (REQ-WEB-012): the count on a tile opens the rest of its series
// in place, and closes it again; the tile itself still opens the viewer.
(function () {
  "use strict";
  function toggle(stack) {
    var series = stack.closest(".series");
    if (!series) return;
    var open = series.classList.toggle("open");
    stack.setAttribute("aria-expanded", open ? "true" : "false");
  }
  document.addEventListener("click", function (e) {
    var stack = e.target.closest && e.target.closest(".stack");
    if (!stack) return;
    e.preventDefault();
    e.stopPropagation();
    toggle(stack);
  }, true);
  document.addEventListener("keydown", function (e) {
    if (e.key !== "Enter" && e.key !== " ") return;
    var stack = e.target.closest && e.target.closest(".stack");
    if (!stack) return;
    e.preventDefault();
    toggle(stack);
  });
})();

// Slideshow (REQ-WEB-013): the photos listed in the page, one every few
// seconds, cross-faded, the next one loaded ahead. A tap or the space bar
// pauses and plays, arrows and swipes step, Escape closes; the screen stays
// on while it plays where the browser allows it.
(function () {
  "use strict";
  var show = document.querySelector(".show");
  if (!show) return;
  var slides = Array.prototype.slice.call(show.querySelectorAll(".slides li"));
  var imgs = show.querySelectorAll("img.slide");
  var cap = show.querySelector(".cap");
  var play = show.querySelector(".play");
  var close = show.querySelector("a.close");
  var fs = show.querySelector(".sfs");
  var ms = parseInt(show.getAttribute("data-interval"), 10) || 5000;
  var i = -1, cur = 0, playing = true, timer = null, lock = null;
  if (!slides.length) { show.hidden = true; return; }
  document.documentElement.classList.add("showing");
  function wake(on) {
    try {
      if (on && !lock && navigator.wakeLock) {
        navigator.wakeLock.request("screen").then(function (l) { lock = l; }, function () {});
      } else if (!on && lock) { lock.release(); lock = null; }
    } catch (e) { /* not offered */ }
  }
  function schedule() {
    clearTimeout(timer);
    if (playing) timer = setTimeout(function () { go(i + 1); }, ms);
  }
  function setPlaying(p) {
    playing = p;
    show.classList.toggle("paused", !p);
    play.textContent = p ? "❚❚" : "▶";
    play.setAttribute("aria-label", p ? "Pause" : "Play");
    wake(p);
    schedule();
  }
  function go(k) {
    // Playing again after the end starts over.
    if (k >= slides.length && playing && !show.querySelector(".send").hidden) k = 0;
    if (k >= slides.length) {
      show.querySelector(".send").hidden = false;
      setPlaying(false);
      return;
    }
    if (k < 0 || (k === i && show.querySelector(".send").hidden)) return;
    show.querySelector(".send").hidden = true;
    i = k;
    var next = imgs[1 - cur], li = slides[k];
    next.onload = function () {
      next.classList.add("on");
      imgs[cur].classList.remove("on");
      cur = 1 - cur;
    };
    // A photo that does not load is passed over while playing.
    next.onerror = function () { if (i === k && playing) go(k + 1); };
    next.src = li.getAttribute("data-src");
    cap.textContent = li.getAttribute("data-when");
    close.href = li.getAttribute("data-href");
    if (slides[k + 1]) new Image().src = slides[k + 1].getAttribute("data-src");
    schedule();
  }
  play.addEventListener("click", function (e) {
    e.stopPropagation();
    if (!playing && !show.querySelector(".send").hidden) { setPlaying(true); go(0); return; }
    setPlaying(!playing);
  });
  show.querySelector(".snext").addEventListener("click", function (e) { e.stopPropagation(); go(i + 1); });
  show.querySelector(".sprev").addEventListener("click", function (e) { e.stopPropagation(); go(i - 1); });
  show.addEventListener("click", function (e) {
    if (e.target.closest("a, button")) return;
    setPlaying(!playing);
  });
  document.addEventListener("keydown", function (e) {
    if (e.altKey || e.ctrlKey || e.metaKey) return;
    if (e.target && e.target.closest && e.target.closest("input, textarea")) return;
    if (e.key === " ") { e.preventDefault(); setPlaying(!playing); }
    else if (e.key === "ArrowRight") go(i + 1);
    else if (e.key === "ArrowLeft") go(i - 1);
    else if (e.key === "Escape" && !document.fullscreenElement) window.location.href = close.href;
    else if (e.key === "f" && fs && !fs.hidden) fs.click();
  });
  var x0 = null, y0 = 0;
  show.addEventListener("touchstart", function (e) {
    if (e.touches.length !== 1) { x0 = null; return; }
    x0 = e.touches[0].clientX; y0 = e.touches[0].clientY;
  }, { passive: true });
  show.addEventListener("touchend", function (e) {
    if (x0 === null) return;
    var dx = e.changedTouches[0].clientX - x0, dy = e.changedTouches[0].clientY - y0;
    x0 = null;
    if (Math.abs(dx) > 50 && Math.abs(dx) > 2 * Math.abs(dy)) {
      e.preventDefault();
      go(dx < 0 ? i + 1 : i - 1);
    }
  });
  var full = show.requestFullscreen || show.webkitRequestFullscreen;
  if (fs && full) {
    fs.hidden = false;
    fs.addEventListener("click", function (e) {
      e.stopPropagation();
      if (document.fullscreenElement || document.webkitFullscreenElement) {
        (document.exitFullscreen || document.webkitExitFullscreen).call(document);
      } else {
        full.call(show);
      }
    });
  }
  document.addEventListener("visibilitychange", function () {
    if (document.visibilityState === "visible" && playing) { lock = null; wake(true); }
  });
  go(0);
  setPlaying(true);
})();

// Selecting in the timeline (REQ-WEB-014): "Select" turns taps on tiles
// into choosing them (a series counts whole); the bar hides the chosen
// ones for this person, with an undo.
(function () {
  "use strict";
  var btn = document.querySelector(".thead .select");
  var tl = document.querySelector(".timeline");
  if (!btn || !tl || !window.fetch) return;
  btn.hidden = false;
  var bar = document.createElement("div");
  bar.className = "selbar";
  bar.setAttribute("role", "toolbar");
  bar.hidden = true;
  // On the favourites page the heart takes them out again; in an album,
  // "Remove" takes them out of it.
  var favPage = tl.classList.contains("favs");
  var album = tl.getAttribute("data-album");
  var opts = document.getElementById("albumopts");
  bar.innerHTML = '<span class="seln" aria-live="polite"></span>' +
    '<button type="button" class="selfav">' + (favPage ? "Not a favourite" : "\u2665 Favourite") + '</button>' +
    (album ? '<button type="button" class="selout">Remove from album</button>' : '') +
    (opts ? '<select class="selalbum" aria-label="Add to album"><option value="">Add to album\u2026</option>' +
      opts.innerHTML + '<option value="new">New album\u2026</option></select>' +
      '<input type="text" class="selname" maxlength="80" placeholder="Album name" aria-label="Name of the new album" hidden>' +
      '<button type="button" class="seladd" hidden>Add</button>' : '') +
    '<button type="button" class="selhide">Hide for me</button>' +
    '<button type="button" class="selcancel">Cancel</button>';
  document.body.appendChild(bar);
  var count = bar.querySelector(".seln"), hideBtn = bar.querySelector(".selhide");
  var favBtn = bar.querySelector(".selfav");
  var pick = bar.querySelector(".selalbum"), nameIn = bar.querySelector(".selname");
  var addBtn = bar.querySelector(".seladd"), outBtn = bar.querySelector(".selout");
  function ids(tile) {
    var series = tile.closest(".series");
    if (series && !series.classList.contains("open") && !tile.classList.contains("later")) {
      return Array.prototype.map.call(series.querySelectorAll(".item"), function (a) { return a.id.slice(1); });
    }
    return [tile.id.slice(1)];
  }
  // What a tile stands for is fixed when it is chosen: a closed series
  // whole, so opening it afterwards changes nothing.
  function chosen() {
    var seen = {}, out = [];
    tl.querySelectorAll(".item.sel").forEach(function (t) {
      (t.getAttribute("data-ids") || t.id.slice(1)).split(",").forEach(function (id) {
        if (!seen[id]) { seen[id] = true; out.push(id); }
      });
    });
    return out;
  }
  function update() {
    var n = chosen().length;
    count.textContent = n === 1 ? "1 chosen" : n + " chosen";
    hideBtn.disabled = n === 0;
    favBtn.disabled = n === 0;
    if (pick) pick.disabled = n === 0;
    if (outBtn) outBtn.disabled = n === 0;
  }
  function setMode(on) {
    document.body.classList.toggle("selecting", on);
    btn.setAttribute("aria-pressed", on ? "true" : "false");
    btn.textContent = on ? "Done" : "Select";
    bar.hidden = !on;
    if (!on) tl.querySelectorAll(".item.sel").forEach(function (t) {
      t.classList.remove("sel");
      t.removeAttribute("aria-pressed");
      t.removeAttribute("data-ids");
    });
    update();
  }
  btn.addEventListener("click", function () { setMode(!document.body.classList.contains("selecting")); });
  bar.querySelector(".selcancel").addEventListener("click", function () { setMode(false); });
  tl.addEventListener("click", function (e) {
    if (!document.body.classList.contains("selecting")) return;
    var tile = e.target.closest && e.target.closest("a.item");
    if (!tile || e.target.closest(".stack")) return;
    e.preventDefault();
    var on = tile.classList.toggle("sel");
    tile.setAttribute("aria-pressed", on ? "true" : "false");
    if (on) tile.setAttribute("data-ids", ids(tile).join(","));
    else tile.removeAttribute("data-ids");
    update();
  });
  function post(url, list, extra) {
    return fetch(url, {
      method: "POST",
      credentials: "same-origin",
      headers: { "content-type": "application/x-www-form-urlencoded" },
      body: "ids=" + encodeURIComponent(list.join(",")) + (extra || "")
    }).then(function (r) { if (!r.ok) throw new Error(r.status); return r.text(); });
  }
  // Albums (REQ-WEB-017): into one, or a new one named in place.
  function addTo(albumId, name) {
    var list = chosen();
    if (!list.length) return;
    var extra = "&album=" + encodeURIComponent(albumId) +
      (name ? "&name=" + encodeURIComponent(name) : "");
    post("album-add", list, extra).then(function (id) {
      var n = list.length === 1 ? "1 item" : list.length + " items";
      setMode(false);
      var t = document.createElement("div");
      t.className = "toast";
      t.setAttribute("role", "status");
      t.innerHTML = '<span></span><a>Open</a>';
      t.querySelector("span").textContent = n + " added to \u201c" + (name || pick.options[pick.selectedIndex].text) + "\u201d.";
      t.querySelector("a").href = "album/" + String(id).replace(/\D/g, "");
      document.body.appendChild(t);
      setTimeout(function () { t.remove(); }, 8000);
      if (name && opts) {
        var o = document.createElement("option");
        o.value = String(id).replace(/\D/g, "");
        o.textContent = name;
        pick.insertBefore(o, pick.options[1]);
      }
      pick.value = "";
      nameIn.hidden = addBtn.hidden = true;
      nameIn.value = "";
    }, function () { count.textContent = "Could not add, try again"; });
  }
  if (pick) {
    pick.addEventListener("change", function () {
      if (pick.value === "new") {
        nameIn.hidden = addBtn.hidden = false;
        nameIn.focus();
      } else if (pick.value) {
        addTo(pick.value);
      }
    });
    function addNew() {
      // As the server keeps it: spaces folded, at most 80 characters.
      var name = nameIn.value.replace(/\s+/g, " ").trim().slice(0, 80);
      if (name) addTo("new", name);
    }
    addBtn.addEventListener("click", addNew);
    nameIn.addEventListener("keydown", function (e) { if (e.key === "Enter") { e.preventDefault(); addNew(); } });
  }
  if (outBtn) outBtn.addEventListener("click", function () {
    var list = chosen();
    if (!list.length) return;
    outBtn.disabled = true;
    post("album/" + album + "/remove", list).then(function () {
      list.forEach(function (id) {
        var t = document.getElementById("i" + id);
        if (t) t.remove();
      });
      setMode(false);
      toast(list, (list.length === 1 ? "1 item" : list.length + " items") + " taken out of the album.",
        "album-add", false, "&album=" + encodeURIComponent(album));
    }, function () { outBtn.disabled = false; count.textContent = "Could not remove, try again"; });
  });
  hideBtn.addEventListener("click", function () {
    var list = chosen();
    if (!list.length) return;
    hideBtn.disabled = true;
    post("hide-many", list).then(function () {
      list.forEach(function (id) {
        var t = document.getElementById("i" + id);
        if (t) t.remove();
      });
      // A series that lost its first tile shows what is left as tiles.
      tl.querySelectorAll(".series").forEach(function (s) {
        if (s.querySelector(".stack")) return;
        s.querySelectorAll(".item.later").forEach(function (t) { t.classList.remove("later"); });
        while (s.firstChild) s.parentNode.insertBefore(s.firstChild, s);
        s.remove();
      });
      setMode(false);
      toast(list);
    }, function () { hideBtn.disabled = false; count.textContent = "Could not hide, try again"; });
  });
  function hearts(list, on) {
    list.forEach(function (id) {
      var t = document.getElementById("i" + id);
      if (!t) return;
      var h = t.querySelector(".heart");
      if (on && !h) {
        h = document.createElement("span");
        h.className = "heart";
        h.title = "Favourite";
        h.textContent = "\u2665";
        t.appendChild(h);
      } else if (!on && h) h.remove();
    });
  }
  favBtn.addEventListener("click", function () {
    var list = chosen();
    if (!list.length) return;
    favBtn.disabled = true;
    post(favPage ? "unfav-many" : "fav-many", list).then(function () {
      if (favPage) list.forEach(function (id) {
        var t = document.getElementById("i" + id);
        if (t) t.remove();
      });
      else hearts(list, true);
      setMode(false);
      var n = list.length === 1 ? "1 item" : list.length + " items";
      toast(list, favPage ? n + " no longer favourites." : n + " made favourites.",
        favPage ? "fav-many" : "unfav-many", !favPage);
    }, function () { favBtn.disabled = false; count.textContent = "Could not save, try again"; });
  });
  // Undo posts `undo`; it reloads unless only hearts changed in place.
  function toast(list, text, undo, inPlace, extra) {
    var t = document.createElement("div");
    t.className = "toast";
    t.setAttribute("role", "status");
    t.innerHTML = '<span></span><button type="button">Undo</button>';
    t.querySelector("span").textContent = text ||
      (list.length === 1 ? "1 item" : list.length + " items") + " hidden for you.";
    document.body.appendChild(t);
    var gone = setTimeout(function () { t.remove(); }, 10000);
    t.querySelector("button").addEventListener("click", function () {
      clearTimeout(gone);
      post(undo || "unhide-many", list, extra).then(function () {
        if (inPlace) { hearts(list, false); t.remove(); } else window.location.reload();
      });
    });
  }
})();

// Row height in the timeline: − and + (buttons or keys), a pinch on the
// iPad or a trackpad. Remembered in this browser only.
(function () {
  "use strict";
  var tl = document.querySelector(".timeline");
  var head = document.querySelector(".thead");
  if (!tl || !head) return;
  var KEY = "lichtbild.row", MAX = 4;
  var small = window.matchMedia && window.matchMedia("(max-width: 600px)").matches;
  var row = small ? 1 : 2;
  try {
    var v = parseInt(localStorage.getItem(KEY), 10);
    if (v >= 0 && v <= MAX) row = v;
  } catch (e) { /* private window: the default */ }
  var zoom = document.createElement("span");
  zoom.className = "zoom";
  zoom.innerHTML = '<button type="button" class="tbtn" aria-label="Smaller pictures" title="Smaller (\u2212)">\u2212</button>' +
    '<button type="button" class="tbtn" aria-label="Larger pictures" title="Larger (+)">+</button>';
  head.appendChild(zoom);
  var minus = zoom.children[0], plus = zoom.children[1];
  function set(r) {
    row = Math.max(0, Math.min(MAX, r));
    for (var i = 0; i <= MAX; i++) tl.classList.toggle("row" + i, i === row);
    minus.disabled = row === 0;
    plus.disabled = row === MAX;
    try { localStorage.setItem(KEY, String(row)); } catch (e) { /* not kept */ }
  }
  set(row);
  minus.addEventListener("click", function () { set(row - 1); });
  plus.addEventListener("click", function () { set(row + 1); });
  document.addEventListener("keydown", function (e) {
    if (e.altKey || e.ctrlKey || e.metaKey) return;
    if (e.target && e.target.closest && e.target.closest("input, textarea, select")) return;
    if (e.key === "-") set(row - 1);
    else if (e.key === "+" || e.key === "=") set(row + 1);
  });
  // Safari's pinch (iPad), and a trackpad pinch (wheel with ctrl).
  var start = row;
  tl.addEventListener("gesturestart", function (e) { e.preventDefault(); start = row; });
  tl.addEventListener("gesturechange", function (e) {
    e.preventDefault();
    var step = e.scale > 1.35 ? 1 : e.scale < 0.74 ? -1 : 0;
    if (e.scale > 2.2) step = 2;
    if (e.scale < 0.45) step = -2;
    if (start + step !== row) set(start + step);
  });
  var acc = 0;
  tl.addEventListener("wheel", function (e) {
    if (!e.ctrlKey) return;
    e.preventDefault();
    acc += e.deltaY;
    if (Math.abs(acc) > 40) { set(row + (acc < 0 ? 1 : -1)); acc = 0; }
  }, { passive: false });
})();

// Keys and gestures (REQ-WEB-015): "?" or the menu entry shows them.
(function () {
  "use strict";
  var rows = [
    ["Photo", "← →", "previous, next (or swipe)"],
    ["Photo", "Esc", "back to the timeline (or swipe down)"],
    ["Photo", "+ − 0", "zoom in, out, fit (or pinch, double tap)"],
    ["Photo", "i  t", "details, transcript"],
    ["Photo", "f", "full screen"],
    ["Photo", "s", "slideshow from here"],
    ["Photo", ".", "favourite, or not any more"],
    ["Slideshow", "space", "pause, play (or tap)"],
    ["Slideshow", "← →", "previous, next (or swipe)"],
    ["Slideshow", "Esc", "close"],
    ["Search", "↓ ↑", "move through the suggestions"],
    ["Timeline", "\u2212 +", "smaller, larger pictures (or pinch)"],
    ["Timeline", "Select", "choose several: favourites, an album, or hide them"],
    ["Anywhere", "?", "this list"]
  ];
  var dlg = null;
  function open() {
    if (!dlg) {
      dlg = document.createElement("dialog");
      dlg.className = "keys";
      var h = '<h2>Keys and gestures</h2><table>';
      rows.forEach(function (r) {
        h += "<tr><th>" + r[0] + "</th><td><kbd>" + r[1] + "</kbd></td><td>" + r[2] + "</td></tr>";
      });
      dlg.innerHTML = h + '</table><form method="dialog"><button>Close</button></form>';
      document.body.appendChild(dlg);
      dlg.addEventListener("click", function (e) { if (e.target === dlg) dlg.close(); });
    }
    if (dlg.showModal) dlg.showModal(); else dlg.setAttribute("open", "");
  }
  document.addEventListener("keydown", function (e) {
    if (e.key !== "?" || e.ctrlKey || e.metaKey || e.altKey) return;
    if (e.target && e.target.closest && e.target.closest("input, textarea")) return;
    e.preventDefault();
    open();
  });
  document.addEventListener("click", function (e) {
    var b = e.target.closest && e.target.closest(".keyhelp");
    if (!b) return;
    var menu = b.closest("details");
    if (menu) menu.open = false;
    open();
  });
})();

// Days (REQ-WEB-019): picking a date goes there; ←/→ step a day.
(function () {
  const pick = document.querySelector('form.daypick input[type=date]');
  if (!pick) return;
  pick.addEventListener('change', () => { if (pick.value) pick.form.submit(); });
  document.addEventListener('keydown', (e) => {
    if (e.target.closest('input, textarea, select') || e.metaKey || e.ctrlKey || e.altKey) return;
    const a = document.querySelector(e.key === 'ArrowLeft' ? '.daynav a[rel=prev]' : e.key === 'ArrowRight' ? '.daynav a[rel=next]' : null);
    if (a) { e.preventDefault(); location.href = a.href; }
  });
})();
