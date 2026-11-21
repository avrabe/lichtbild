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

  // The person's day or night choice (REQ-WEB-024), else the system's.
  var chosen = document.documentElement.getAttribute("data-theme");
  var dark = chosen ? chosen === "night" : window.matchMedia && window.matchMedia("(prefers-color-scheme: dark)").matches;
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
  // Place mode (REQ-PLACE-002): a tap sets the picture's position through
  // the page's form, so the server's same-origin check applies.
  var placeForm = document.getElementById("placeform");
  if (placeForm) {
    var pin = null;
    map.getCanvas().style.cursor = "crosshair";
    map.on("click", function (e) {
      placeForm.elements.lat.value = e.lngLat.lat.toFixed(6);
      placeForm.elements.lon.value = e.lngLat.lng.toFixed(6);
      if (pin) pin.remove();
      pin = new maplibregl.Marker({ color: "#c8452d" }).setLngLat(e.lngLat).addTo(map);
      placeForm.elements.save.disabled = false;
    });
  }
  var markers = [];
  var pending = null;

  function estimated(it) {
    return it.est_m ? " · estimated, within " + (it.est_m >= 1000 ? (it.est_m / 1000).toFixed(1) + " km" : it.est_m + " m") : "";
  }

  function viewerHref(it) {
    return "v/" + encodeURIComponent(it.who) + "/" + it.id;
  }

  function showStrip(c) {
    strip.textContent = "";
    c.items.forEach(function (it) {
      var a = document.createElement("a");
      a.href = viewerHref(it);
      a.title = it.taken.replace("T", " ") + estimated(it);
      if (it.est_m) a.classList.add("est");
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
      node.className = "mk one" + (one.video ? " video" : "") + (one.est_m ? " est" : "");
      var img = document.createElement("img");
      img.src = one.thumb;
      img.alt = "";
      node.appendChild(img);
      node.title = one.taken.replace("T", " ") + estimated(one);
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
    var from = attr("data-from"), to = attr("data-to");
    if (from) q += "&from=" + encodeURIComponent(from);
    if (to) q += "&to=" + encodeURIComponent(to);
    var country = attr("data-country");
    if (country) q += "&country=" + encodeURIComponent(country);
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

  // When (REQ-MAP-002): a trip reloads the page, which fits the map to it;
  // the slider narrows the months shown in place.
  var when = document.querySelector("form.mapwhen");
  if (when) {
    when.elements.trip.addEventListener("change", function () { when.submit(); });
    var box = when.querySelector(".mapslider");
    if (box) {
      var first = box.getAttribute("data-first").split("-").map(Number);
      var last = box.getAttribute("data-last").split("-").map(Number);
      var n = (last[0] - first[0]) * 12 + (last[1] - first[1]);
      var lo = box.querySelector(".lo"), hi = box.querySelector(".hi"), out = box.querySelector("output");
      var NAMES = ["Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec"];
      var month = function (i) {
        var m = first[1] - 1 + i;
        return [first[0] + Math.floor(m / 12), (m % 12) + 1];
      };
      var index = function (day) {
        if (!day) return null;
        var p = day.split("-").map(Number);
        return Math.max(0, Math.min(n, (p[0] - first[0]) * 12 + (p[1] - first[1])));
      };
      var pad = function (v) { return (v < 10 ? "0" : "") + v; };
      [lo, hi].forEach(function (r) { r.min = 0; r.max = n; });
      lo.value = index(attr("data-from")) || 0;
      var h = index(attr("data-to"));
      hi.value = h === null ? n : h;
      var label = function () {
        var a = month(+lo.value), b = month(+hi.value);
        out.textContent = NAMES[a[1] - 1] + " " + a[0] + " – " + NAMES[b[1] - 1] + " " + b[0];
      };
      var apply = function () {
        var a = month(+lo.value), b = month(+hi.value);
        var end = new Date(b[0], b[1], 0).getDate();
        var f = +lo.value === 0 ? "" : a[0] + "-" + pad(a[1]) + "-01";
        var t = +hi.value === n ? "" : b[0] + "-" + pad(b[1]) + "-" + pad(end);
        if (f) el.setAttribute("data-from", f); else el.removeAttribute("data-from");
        if (t) el.setAttribute("data-to", t); else el.removeAttribute("data-to");
        when.elements.trip.value = "";
        var u = new URL(location.href);
        u.searchParams.delete("trip");
        if (f) u.searchParams.set("from", f); else u.searchParams.delete("from");
        if (t) u.searchParams.set("to", t); else u.searchParams.delete("to");
        history.replaceState(null, "", u);
        load();
      };
      lo.addEventListener("input", function () { if (+lo.value > +hi.value) hi.value = lo.value; label(); });
      hi.addEventListener("input", function () { if (+hi.value < +lo.value) lo.value = hi.value; label(); });
      lo.addEventListener("change", apply);
      hi.addEventListener("change", apply);
      label();
      box.hidden = false;
    }
  }
})();
