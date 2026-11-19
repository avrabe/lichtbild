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
    if (e.key === "ArrowLeft") go("a.prev");
    else if (e.key === "ArrowRight") go("a.next");
    else if (e.key === "Escape") go("a.back");
    else if (e.key === "i") {
      var d = viewer.querySelector("details.info");
      if (d) d.open = !d.open;
    } else if (e.key === "t") {
      var tr = viewer.querySelector("details.transcript");
      if (tr) tr.open = !tr.open;
    }
  });
  var x0 = null, y0 = null, t0 = 0;
  var stage = viewer.querySelector(".stage") || viewer;
  stage.addEventListener("touchstart", function (e) {
    if (e.touches.length !== 1 || onVideo(e.target) || viewer.classList.contains("zoomed")) { x0 = null; return; }
    x0 = e.touches[0].clientX; y0 = e.touches[0].clientY; t0 = Date.now();
  }, { passive: true });
  stage.addEventListener("touchend", function (e) {
    if (x0 === null || viewer.classList.contains("zoomed")) { x0 = null; return; }
    var dx = e.changedTouches[0].clientX - x0;
    var dy = e.changedTouches[0].clientY - y0;
    x0 = null;
    // A deliberate, mostly horizontal swipe; pinch-zoom and scrolling pass.
    if (Math.abs(dx) > 50 && Math.abs(dx) > 2 * Math.abs(dy) && Date.now() - t0 < 800) {
      go(dx < 0 ? "a.next" : "a.prev");
    }
  }, { passive: true });
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
  // Text in the picture (REQ-TEXT-001): line boxes over the photo, shown
  // while the text panel is open; they follow the photo's zoom.
  var layer = viewer.querySelector(".textlayer");
  var textp = viewer.querySelector("details.textp");
  function placeLayer() {
    if (!layer) return;
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
  function showLayer() {
    if (!layer) return;
    layer.hidden = !(viewer.classList.contains("below") || (textp && textp.open));
    placeLayer();
  }
  if (textp) textp.addEventListener("toggle", showLayer);
  viewer.addEventListener("lb-layout", showLayer);
  if (img.complete) showLayer(); else img.addEventListener("load", showLayer);
  window.addEventListener("resize", placeLayer);
  function apply() {
    img.style.transform = "translate(" + tx + "px," + ty + "px) scale(" + scale + ")";
    viewer.classList.toggle("zoomed", scale > 1.001);
    if (layer && !layer.hidden) layer.style.transform = img.style.transform;
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
    if (current) current.classList.remove("now");
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

