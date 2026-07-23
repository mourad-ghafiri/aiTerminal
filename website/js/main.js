/* main.js — site behaviors: reveal-on-scroll, count-up stats, nav, docs scroll-spy. */

/* refresh always lands at the top: the browser restores scroll position and
   re-jumps to any #anchor left in the URL, so opt out of both on reload.
   Anchor clicks still scroll normally — the hash just never sticks in the
   address bar — and deep links opened fresh (not reloaded) still work. */
if ("scrollRestoration" in history) history.scrollRestoration = "manual";
window.addEventListener("hashchange", () =>
  history.replaceState(null, "", location.pathname + location.search));
const navEntry = performance.getEntriesByType("navigation")[0];
if (navEntry && navEntry.type === "reload") {
  if (location.hash) history.replaceState(null, "", location.pathname + location.search);
  window.addEventListener("load", () => window.scrollTo(0, 0));
}

document.addEventListener("DOMContentLoaded", () => {
  /* reveal on scroll */
  const io = new IntersectionObserver(
    (es) => es.forEach((e) => { if (e.isIntersecting) { e.target.classList.add("in"); io.unobserve(e.target); } }),
    { rootMargin: "-40px" }
  );
  document.querySelectorAll(".reveal").forEach((el) => io.observe(el));

  /* count-up stats */
  const countUp = (el) => {
    const target = parseFloat(el.dataset.count);
    const suffix = el.dataset.suffix || "";
    const dur = 1200, t0 = performance.now();
    const tick = (t) => {
      const k = Math.min(1, (t - t0) / dur);
      const eased = 1 - Math.pow(1 - k, 3);
      el.firstChild.textContent = Math.round(target * eased).toLocaleString("en-US");
      if (k < 1) requestAnimationFrame(tick);
      else el.firstChild.textContent = Math.round(target).toLocaleString("en-US");
    };
    el.innerHTML = "0<span class='u'>" + suffix + "</span>";
    requestAnimationFrame(tick);
  };
  const cio = new IntersectionObserver((es) => es.forEach((e) => {
    if (e.isIntersecting) { countUp(e.target); cio.unobserve(e.target); }
  }));
  document.querySelectorAll("[data-count]").forEach((el) => cio.observe(el));

  /* mobile nav */
  const toggle = document.querySelector(".nav-toggle");
  if (toggle) toggle.addEventListener("click", () =>
    document.querySelector(".nav-links").classList.toggle("open"));

  /* docs scroll-spy */
  const docLinks = [...document.querySelectorAll(".docs-nav a[href^='#']")];
  if (docLinks.length) {
    const targets = docLinks
      .map((a) => document.getElementById(a.getAttribute("href").slice(1)))
      .filter(Boolean);
    const spy = new IntersectionObserver((es) => {
      es.forEach((e) => {
        if (e.isIntersecting) {
          docLinks.forEach((a) => a.classList.toggle("active", a.getAttribute("href") === "#" + e.target.id));
        }
      });
    }, { rootMargin: "-15% 0px -75% 0px" });
    targets.forEach((t) => spy.observe(t));
  }
});
