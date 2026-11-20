// Day or night (REQ-WEB-024): the person's choice, kept in this browser,
// applied before the page is drawn so it does not flash. "auto" follows
// the system.
(function () {
  "use strict";
  var t = null;
  try { t = window.localStorage.getItem("lichtbild.theme"); } catch (e) { /* private mode */ }
  if (t === "day" || t === "night") document.documentElement.setAttribute("data-theme", t);
})();
