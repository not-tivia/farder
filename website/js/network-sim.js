/**
 * network-sim.js — the "what happens when..." chart on the How it works page.
 *
 * Every scenario is a short list of steps, and every step is a complete
 * picture: which boxes are up, which lines are carrying traffic, and one
 * sentence of cause and one of effect. Stepping backwards is then just
 * drawing an earlier picture; nothing has to be undone.
 *
 * The behaviour described here is the real behaviour, checked against the
 * code: the app retries every 3 s (AppShell.tsx), a relayed server retries the
 * relay with 0.5 s -> 30 s backoff (farder-server relay.rs), the relay holds no
 * messages, and the app refuses a relay whose certificate fingerprint does not
 * match before it sends anything. Change those and this file is wrong.
 */
(function () {
  "use strict";

  var root = document.getElementById("net-sim");
  if (!root) return;

  // ── The pictures ─────────────────────────────────────────────────────────
  // n = nodes: you, friend, relay, host, fake  -> [state, short label]
  //     state is ok | warn | down | off (off = not drawn)
  // l = links: yr, fr, hr (via relay), yh, fh (direct), yf (to the fake)
  //     state is up | down | blocked | off

  var ALL_OK_RELAYED = {
    n: { you: ["ok", "online"], friend: ["ok", "online"], relay: ["ok", "forwarding"], host: ["ok", "running"], fake: ["off", ""] },
    l: { yr: "up", fr: "up", hr: "up", yh: "off", fh: "off", yf: "off" }
  };
  var ALL_OK_DIRECT = {
    n: { you: ["ok", "online"], friend: ["ok", "online"], relay: ["off", ""], host: ["ok", "running"], fake: ["off", ""] },
    l: { yr: "off", fr: "off", hr: "off", yh: "up", fh: "up", yf: "off" }
  };

  // Build a picture from a base with a few changes, so each step only states
  // what is different about it.
  function pic(base, nodes, links) {
    var out = { n: {}, l: {} };
    var k;
    for (k in base.n) out.n[k] = (nodes && nodes[k]) || base.n[k];
    for (k in base.l) out.l[k] = (links && links[k]) || base.l[k];
    return out;
  }

  var R = ALL_OK_RELAYED;
  var D = ALL_OK_DIRECT;

  var SCENARIOS = {
    relayed: {
      normal: [
        { p: R,
          cause: "Everything is up.",
          effect: "Your message goes from your app to the relay, down the host's line to the server, which saves it and sends it back out through the relay to your friend. The server only ever sees the relay's address, and so do you." }
      ],
      you: [
        { p: pic(R, { you: ["down", "no internet"] }, { yr: "down" }),
          cause: "Your internet cuts out.",
          effect: "Your line to the relay breaks. Nothing else is touched: the relay, the host and your friend carry on." },
        { p: pic(R, { you: ["warn", "reconnecting"], friend: ["ok", "sees you leave"] }, { yr: "down" }),
          cause: "Your app notices the line is dead.",
          effect: "It shows “Connection lost. Reconnecting…” and tries again every 3 seconds. If you were in a voice call it ends it, so your mic isn't left live. The server tells everyone else you've gone offline." },
        { p: pic(R, { you: ["warn", "reconnecting"], host: ["ok", "saving messages"] }, { yr: "down" }),
          cause: "Your friend keeps chatting.",
          effect: "Their messages still reach the server and are saved on the host's machine. You aren't there to receive them, but nothing is lost." },
        { p: pic(R, { you: ["ok", "back"] }),
          cause: "Your internet comes back.",
          effect: "The next retry gets through. Your app signs in again on its own, and what you missed is still there to read, because it was saved on the host, not in transit." }
      ],
      relay: [
        { p: pic(R, { relay: ["down", "offline"] }, { yr: "down", fr: "down", hr: "down" }),
          cause: "The relay goes offline.",
          effect: "Every line through it breaks at once: yours, your friend's and the host's." },
        { p: pic(R, { relay: ["down", "offline"], you: ["warn", "reconnecting"], friend: ["warn", "reconnecting"], host: ["warn", "retrying relay"] }, { yr: "down", fr: "down", hr: "down" }),
          cause: "Everyone's app notices.",
          effect: "Both apps show “Reconnecting…” and retry every 3 seconds; voice calls end. The server is still running on the host's machine and keeps calling the relay, waiting longer between tries, from half a second up to 30 seconds." },
        { p: pic(R, { relay: ["down", "offline"], you: ["warn", "reconnecting"], friend: ["warn", "reconnecting"], host: ["warn", "history safe"] }, { yr: "down", fr: "down", hr: "down" }),
          cause: "What was lost?",
          effect: "Nothing. The relay never stored a message, so there is nothing on it to lose. All history is on the host's disk. Nobody can chat until a line is back, but nothing is gone." },
        { p: pic(R, { relay: ["ok", "back"], you: ["warn", "reconnecting"], friend: ["warn", "reconnecting"], host: ["ok", "checked in"] }, { yr: "blocked", fr: "blocked" }),
          cause: "The relay comes back.",
          effect: "The host's server calls in and registers again. If your app gets through before the host does, the relay answers “destination not connected” and the app simply tries again 3 seconds later." },
        { p: pic(R, { relay: ["ok", "back"], you: ["ok", "back"], friend: ["ok", "back"] }),
          cause: "Everyone's retry lands.",
          effect: "Both apps reconnect and sign in again on their own. Nobody had to press anything." }
      ],
      host: [
        { p: pic(R, { host: ["down", "PC off"] }, { hr: "down" }),
          cause: "The host turns their PC off.",
          effect: "The server stops, and its line to the relay goes with it. The relay itself is fine." },
        { p: pic(R, { host: ["down", "PC off"], you: ["warn", "reconnecting"], friend: ["warn", "reconnecting"], relay: ["ok", "server not in"] }, { hr: "down", yr: "blocked", fr: "blocked" }),
          cause: "Your apps lose the server.",
          effect: "Both show “Reconnecting…”. Each retry reaches the relay, which answers “destination not connected”, because there is no server checked in to put you through to." },
        { p: pic(R, { host: ["down", "history on disk"], you: ["warn", "reconnecting"], friend: ["warn", "reconnecting"], relay: ["ok", "server not in"] }, { hr: "down", yr: "blocked", fr: "blocked" }),
          cause: "This is the real weak point today.",
          effect: "The history is safe on the host's disk, but while that one machine is off, the community is closed. Removing this single point of failure is what mesh hosting is for." },
        { p: pic(R, { host: ["ok", "running"], you: ["ok", "back"], friend: ["ok", "back"] }),
          cause: "The host opens Farder again.",
          effect: "Their server starts, calls the relay and checks in under the same ID. The next retry from each app gets put through, and everyone is back." }
      ],
      fake: [
        { p: pic(R, { fake: ["warn", "impostor"], you: ["ok", "calling…"] }, { yr: "off", yf: "up" }),
          cause: "Someone on the network intercepts your call to the relay.",
          effect: "A machine pretending to be the relay answers instead. It can copy the relay's address, but not the relay's private key." },
        { p: pic(R, { fake: ["down", "rejected"], you: ["warn", "refused"] }, { yr: "off", yf: "blocked" }),
          cause: "Your app asks it to prove who it is.",
          effect: "During the encrypted handshake the impostor has to present the relay's certificate, and its fingerprint doesn't match the one your app expects. The app hangs up before sending anything of yours." },
        { p: pic(R, { fake: ["off", ""], you: ["ok", "safe"] }),
          cause: "What did the impostor get?",
          effect: "Nothing it can use: no messages, no sign-in, and no key. Your app keeps retrying until it reaches the real relay." }
      ]
    },
    direct: {
      normal: [
        { p: D,
          cause: "Everything is up.",
          effect: "Your app connects straight to the host's computer. It's simpler, but the host sees every member's real IP address, and every member needs the host's." }
      ],
      you: [
        { p: pic(D, { you: ["down", "no internet"] }, { yh: "down" }),
          cause: "Your internet cuts out.",
          effect: "Your line to the host breaks. Your friend and the server carry on." },
        { p: pic(D, { you: ["warn", "reconnecting"], friend: ["ok", "sees you leave"], host: ["ok", "saving messages"] }, { yh: "down" }),
          cause: "Your app notices.",
          effect: "“Reconnecting…” every 3 seconds, voice call ended, and the server tells everyone you've gone offline. Messages sent meanwhile are saved on the host." },
        { p: pic(D, { you: ["ok", "back"] }),
          cause: "Your internet comes back.",
          effect: "The app reconnects and signs in on its own, and what you missed is waiting on the host." }
      ],
      relay: [
        { p: D,
          cause: "A relay goes down somewhere.",
          effect: "Nothing happens here. A direct server doesn't use a relay, so a relay outage can't touch it." }
      ],
      host: [
        { p: pic(D, { host: ["down", "PC off"] }, { yh: "down", fh: "down" }),
          cause: "The host turns their PC off.",
          effect: "Both lines break: there is nothing at the other end to answer." },
        { p: pic(D, { host: ["down", "history on disk"], you: ["warn", "reconnecting"], friend: ["warn", "reconnecting"] }, { yh: "down", fh: "down" }),
          cause: "Your apps keep trying.",
          effect: "Both retry every 3 seconds and get no answer. The history is safe on the host's disk; the server is simply closed until it comes back." },
        { p: pic(D, { you: ["ok", "back"], friend: ["ok", "back"] }),
          cause: "The host opens Farder again.",
          effect: "The server starts, the next retries get through, and everyone is back." }
      ],
      fake: [
        { p: D,
          cause: "Fake relays only matter with a relay.",
          effect: "A direct server doesn't use one. Switch to “Relayed” to see how the app spots an impostor." }
      ]
    }
  };

  // ── Drawing ──────────────────────────────────────────────────────────────

  var state = { mode: "relayed", scenario: "normal", step: 0 };

  var nodeEls = {};
  var linkEls = {};
  ["you", "friend", "relay", "host", "fake"].forEach(function (id) {
    nodeEls[id] = root.querySelector('[data-node="' + id + '"]');
  });
  ["yr", "fr", "hr", "yh", "fh", "yf"].forEach(function (id) {
    linkEls[id] = root.querySelector('[data-link="' + id + '"]');
  });

  var causeEl = root.querySelector("[data-sim-cause]");
  var effectEl = root.querySelector("[data-sim-effect]");
  var counterEl = root.querySelector("[data-sim-counter]");
  var backBtn = root.querySelector("[data-sim-back]");
  var nextBtn = root.querySelector("[data-sim-next]");
  var dotsEl = root.querySelector("[data-sim-dots]");

  function steps() {
    return SCENARIOS[state.mode][state.scenario];
  }

  function draw() {
    var list = steps();
    var s = list[state.step];
    var id;

    for (id in nodeEls) {
      var n = s.p.n[id];
      var el = nodeEls[id];
      el.setAttribute("class", "sim-node sim-" + n[0]);
      el.querySelector(".sim-status").textContent = n[1];
    }
    for (id in linkEls) {
      linkEls[id].setAttribute("class", "sim-link sim-" + s.p.l[id]);
    }

    causeEl.textContent = s.cause;
    effectEl.textContent = s.effect;
    counterEl.textContent = list.length > 1
      ? "Step " + (state.step + 1) + " of " + list.length
      : "";

    backBtn.disabled = state.step === 0;
    nextBtn.disabled = state.step === list.length - 1;
    backBtn.hidden = nextBtn.hidden = list.length === 1;

    dotsEl.innerHTML = "";
    if (list.length > 1) {
      for (var i = 0; i < list.length; i++) {
        var dot = document.createElement("button");
        dot.type = "button";
        dot.className = "sim-dot";
        dot.setAttribute("aria-label", "Go to step " + (i + 1));
        if (i === state.step) dot.setAttribute("aria-current", "step");
        dot.setAttribute("data-step", String(i));
        dotsEl.appendChild(dot);
      }
    }

    syncPressed("[data-sim-mode]", "data-sim-mode", state.mode);
    syncPressed("[data-sim-scenario]", "data-sim-scenario", state.scenario);
  }

  function syncPressed(selector, attr, value) {
    var els = root.querySelectorAll(selector);
    for (var i = 0; i < els.length; i++) {
      els[i].setAttribute("aria-pressed", els[i].getAttribute(attr) === value ? "true" : "false");
    }
  }

  // ── Controls ─────────────────────────────────────────────────────────────

  root.addEventListener("click", function (ev) {
    var t = ev.target.closest("button");
    if (!t || !root.contains(t)) return;

    if (t.hasAttribute("data-sim-mode")) {
      state.mode = t.getAttribute("data-sim-mode");
      state.step = 0;
    } else if (t.hasAttribute("data-sim-scenario")) {
      state.scenario = t.getAttribute("data-sim-scenario");
      state.step = 0;
    } else if (t.hasAttribute("data-sim-next")) {
      state.step = Math.min(state.step + 1, steps().length - 1);
    } else if (t.hasAttribute("data-sim-back")) {
      state.step = Math.max(state.step - 1, 0);
    } else if (t.hasAttribute("data-step")) {
      state.step = parseInt(t.getAttribute("data-step"), 10) || 0;
    } else {
      return;
    }
    draw();
  });

  root.classList.add("sim-ready");
  draw();
})();
