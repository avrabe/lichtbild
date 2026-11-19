// lichtbild world map (REQ-MAP-001): MapLibre over the deployment's
// PMTiles file, with photo clusters from map/clusters. Every URL here is
// relative to the page's <base>, so it is lichtbild's own origin and base
// path; the same-origin CSP would block anything else. The libraries are
// vendored (assets/VENDORED.md).
(function () {
  "use strict";
  var el = document.getElementById("map");
  if (!el || !window.maplibregl || !window.pmtiles || !window.basemaps) return;

  function abs(p) { return new URL(p, document.baseURI).href; }
  function attr(n) { return el.getAttribute(n); }

  // The CSP build starts its worker from this same-origin file, not from a
  // blob: URL.
  maplibregl.setWorkerUrl(abs(attr("data-worker")));
  var protocol = new pmtiles.Protocol();
  maplibregl.addProtocol("pmtiles", protocol.tile);

  var dark = window.matchMedia && window.matchMedia("(prefers-color-scheme: dark)").matches;
  var flavor = dark ? "dark" : "light";
  var glyphs = attr("data-glyphs");
  var sprite = attr("data-sprite");
  // Without glyphs there are no labels: the base layers have none, and
  // MapLibre would refuse a text layer without a glyphs URL.
  var lang = (navigator.language || "en").slice(0, 2).toLowerCase();
  var layers = basemaps.layers("protomaps", basemaps.namedFlavor(flavor), glyphs ? { lang: lang } : {});
  layers = layers.filter(function (l) {
    var lay = l.layout || {};
    if (!glyphs && lay["text-field"] !== undefined) return false;
    if (!sprite && lay["icon-image"] !== undefined) return false;
    return true;
  });
  var style = {
    version: 8,
    sources: {
      protomaps: {
        type: "vector",
        url: "pmtiles://" + abs(attr("data-pmtiles")),
        attribution: "© OpenStreetMap"
      }
    },
    layers: layers
  };
  if (glyphs) style.glyphs = abs(glyphs) + "{fontstack}/{range}.pbf";
  if (sprite) style.sprite = abs(sprite) + flavor;

  var opts = {
    container: el,
    style: style,
    center: [0, 20],
    zoom: 1,
    maxZoom: 19,
    // Own options replace MapLibre's default attribution, which links out.
    attributionControl: { compact: false }
  };
  var b = (attr("data-bounds") || "").split(",").map(Number);
  if (b.length === 4 && b.every(isFinite)) {
    opts.bounds = [[b[0], b[1]], [b[2], b[3]]];
    opts.fitBoundsOptions = { padding: 48, maxZoom: 12 };
  }
  var map = new maplibregl.Map(opts);
  map.addControl(new maplibregl.NavigationControl({ showCompass: false }), "top-right");

  var who = attr("data-who");
  var kind = attr("data-kind");
  var strip = document.getElementById("mapstrip");
  var markers = [];
  var pending = null;

  function viewerHref(it) {
    return "v/" + encodeURIComponent(it.who) + "/" + it.id;
  }

  function showStrip(c) {
    strip.textContent = "";
    c.items.forEach(function (it) {
      var a = document.createElement("a");
      a.href = viewerHref(it);
      a.title = it.taken.replace("T", " ");
      if (it.thumb) {
        var img = document.createElement("img");
        img.src = it.thumb;
        img.alt = "";
        img.loading = "lazy";
        a.appendChild(img);
      } else {
        a.className = "nothumb";
        a.textContent = it.video ? "video" : "photo";
      }
      strip.appendChild(a);
    });
    if (c.n > c.items.length) {
      var more = document.createElement("span");
      more.className = "more";
      more.textContent = "and " + (c.n - c.items.length) + " more";
      strip.appendChild(more);
    }
    var close = document.createElement("button");
    close.type = "button";
    close.className = "close";
    close.textContent = "×";
    close.setAttribute("aria-label", "Close");
    close.addEventListener("click", function () { strip.hidden = true; });
    strip.appendChild(close);
    strip.hidden = false;
  }

  function open(c) {
    if (c.n === 1 && c.items && c.items.length === 1) {
      window.location.href = viewerHref(c.items[0]);
    } else if (c.items && c.items.length === c.n) {
      showStrip(c);
    } else if (map.getZoom() < 18) {
      map.easeTo({ center: [c.lon, c.lat], zoom: Math.min(map.getZoom() + 2, 19) });
    } else if (c.items && c.items.length) {
      showStrip(c);
    }
  }

  function marker(c) {
    var node = document.createElement("button");
    node.type = "button";
    var one = c.n === 1 && c.items && c.items[0];
    if (one && one.thumb) {
      node.className = "mk one" + (one.video ? " video" : "");
      var img = document.createElement("img");
      img.src = one.thumb;
      img.alt = "";
      node.appendChild(img);
      node.title = one.taken.replace("T", " ");
    } else {
      node.className = "mk " + (c.n < 10 ? "s" : c.n < 100 ? "m" : "l");
      node.textContent = c.n < 10000 ? String(c.n) : Math.round(c.n / 1000) + "k";
      node.title = c.n + (c.n === 1 ? " item" : " items");
    }
    node.addEventListener("click", function (e) {
      e.stopPropagation();
      open(c);
    });
    return new maplibregl.Marker({ element: node }).setLngLat([c.lon, c.lat]).addTo(map);
  }

  function show(data) {
    markers.forEach(function (m) { m.remove(); });
    markers = data.clusters.map(marker);
  }

  function load() {
    if (pending) pending.abort();
    pending = new AbortController();
    var bb = map.getBounds();
    var q = "map/clusters?z=" + Math.max(0, Math.floor(map.getZoom())) + "&bbox=" +
      [bb.getWest(), bb.getSouth(), bb.getEast(), bb.getNorth()]
        .map(function (v) { return v.toFixed(5); }).join(",");
    if (who) q += "&who=" + encodeURIComponent(who);
    if (kind) q += "&kind=" + encodeURIComponent(kind);
    fetch(q, { signal: pending.signal, headers: { accept: "application/json" } })
      .then(function (r) {
        if (!r.ok) throw new Error("clusters: " + r.status);
        return r.json();
      })
      .then(show)
      .catch(function () {});
  }

  map.on("load", load);
  map.on("moveend", load);
})();
