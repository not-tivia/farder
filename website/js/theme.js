/**
 * theme.js — the page wears the client's themes.
 *
 * The point of the switcher is not decoration: it is the one claim on the page
 * a visitor can verify in a second. "The client is themeable" is a sentence;
 * watching the site repaint is proof.
 *
 * Everything here is local. The choice goes to localStorage on that device and
 * is sent nowhere, which is the least a page making these claims can do.
 */
(function () {
  "use strict";

  var KEY = "farder.theme";
  var THEMES = ["xp", "discord", "kitty"];

  function currentTheme() {
    var t = document.documentElement.getAttribute("data-theme");
    return THEMES.indexOf(t) >= 0 ? t : "xp";
  }

  function apply(theme) {
    if (THEMES.indexOf(theme) < 0) return;
    document.documentElement.setAttribute("data-theme", theme);
    try {
      localStorage.setItem(KEY, theme);
    } catch (e) {
      // Private mode, or storage blocked. The theme still applies for this
      // visit; only the memory of it is lost, which is not worth an error.
    }
    syncButtons(theme);
  }

  function syncButtons(theme) {
    var buttons = document.querySelectorAll("[data-set-theme]");
    for (var i = 0; i < buttons.length; i++) {
      var pressed = buttons[i].getAttribute("data-set-theme") === theme;
      // aria-pressed is what a screen reader announces; the CSS reads the same
      // attribute, so the visual state cannot drift from the announced one.
      buttons[i].setAttribute("aria-pressed", pressed ? "true" : "false");
    }
  }

  function init() {
    syncButtons(currentTheme());

    var buttons = document.querySelectorAll("[data-set-theme]");
    for (var i = 0; i < buttons.length; i++) {
      buttons[i].addEventListener("click", function (ev) {
        apply(ev.currentTarget.getAttribute("data-set-theme"));
      });
    }

    // Reveal-on-scroll, and only as an enhancement: without
    // IntersectionObserver (or with motion reduced) everything is simply
    // visible. A page that needs JavaScript to show its text is a broken page.
    var reveals = document.querySelectorAll(".reveal");
    var reduced = window.matchMedia && window.matchMedia("(prefers-reduced-motion: reduce)").matches;

    if (!("IntersectionObserver" in window) || reduced) {
      for (var j = 0; j < reveals.length; j++) reveals[j].classList.add("is-visible");
      return;
    }

    var io = new IntersectionObserver(function (entries) {
      entries.forEach(function (entry) {
        if (!entry.isIntersecting) return;
        entry.target.classList.add("is-visible");
        io.unobserve(entry.target);
      });
    }, { rootMargin: "0px 0px -8% 0px", threshold: 0.05 });

    for (var k = 0; k < reveals.length; k++) io.observe(reveals[k]);
  }

  if (document.readyState === "loading") {
    document.addEventListener("DOMContentLoaded", init);
  } else {
    init();
  }
})();
