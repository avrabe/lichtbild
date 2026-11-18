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

  function init() {
    var rail = document.querySelector("nav.rail");
    if (!rail) return;
    sizeTicks(rail);
    if (!("IntersectionObserver" in window)) return;
    var io = new IntersectionObserver(
      function (entries) {
        entries.forEach(function (e) {
          if (e.isIntersecting) markActive(rail, e.target.textContent.slice(0, 7));
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
    }
  });
  var x0 = null, y0 = null, t0 = 0;
  var stage = viewer.querySelector(".stage") || viewer;
  stage.addEventListener("touchstart", function (e) {
    if (e.touches.length !== 1 || onVideo(e.target)) { x0 = null; return; }
    x0 = e.touches[0].clientX; y0 = e.touches[0].clientY; t0 = Date.now();
  }, { passive: true });
  stage.addEventListener("touchend", function (e) {
    if (x0 === null) return;
    var dx = e.changedTouches[0].clientX - x0;
    var dy = e.changedTouches[0].clientY - y0;
    x0 = null;
    // A deliberate, mostly horizontal swipe; pinch-zoom and scrolling pass.
    if (Math.abs(dx) > 50 && Math.abs(dx) > 2 * Math.abs(dy) && Date.now() - t0 < 800) {
      go(dx < 0 ? "a.next" : "a.prev");
    }
  }, { passive: true });
})();
