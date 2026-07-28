/* theme-mode.js — the website's light / dark switch.

   Dark is the design's home key and stays the default: a visitor who has never
   chosen sees the black editorial page, exactly as before. A choice is stored
   in localStorage and applied on the very next page, before first paint —
   which is why this file is loaded synchronously in <head>, not deferred.

   The terminal replicas are deliberately NOT touched: they carry their own
   `--t-*` palette from the real builtin themes, so a window keeps looking like
   a window on either page background. */
(function () {
  var KEY = "aiterminal.mode";
  var root = document.documentElement;

  function read() {
    try { return localStorage.getItem(KEY); } catch (e) { return null; }
  }
  function write(mode) {
    try { localStorage.setItem(KEY, mode); } catch (e) { /* private mode — session only */ }
  }

  function label() {
    var light = root.getAttribute("data-theme") === "light";
    var buttons = document.querySelectorAll(".mode-toggle");
    for (var i = 0; i < buttons.length; i++) {
      buttons[i].setAttribute("aria-label", light ? "Switch to dark mode" : "Switch to light mode");
      buttons[i].setAttribute("title", light ? "Dark mode" : "Light mode");
      buttons[i].setAttribute("aria-pressed", light ? "true" : "false");
    }
  }

  function apply(mode) {
    root.setAttribute("data-theme", mode === "light" ? "light" : "dark");
    label();
  }

  /* runs before <body> exists — the attribute lands ahead of the first paint */
  apply(read() === "light" ? "light" : "dark");

  document.addEventListener("DOMContentLoaded", function () {
    label();
    var buttons = document.querySelectorAll(".mode-toggle");
    for (var i = 0; i < buttons.length; i++) {
      buttons[i].addEventListener("click", function () {
        var next = root.getAttribute("data-theme") === "light" ? "dark" : "light";
        apply(next);
        write(next);
      });
    }
  });
})();
