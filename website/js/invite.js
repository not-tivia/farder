/**
 * invite.js — Parse invite URL and handle deep link launch for Farder
 *
 * URL format:  farder.gg/join/ENCODED_TOKEN
 * Example:     farder.gg/join/cGxheS5mYXJkZXIuZ2c6NDQzNS9BYkMxMjN4WQ
 *
 * ENCODED_TOKEN is URL-safe base64 of "server_address/invite_code".
 * Deep link:   farder://play.farder.gg:4435/AbC123xY
 */

(function () {
  "use strict";

  /**
   * Parse the URL to extract server address and invite code.
   *
   * Primary format: /join/ENCODED_TOKEN
   *   where ENCODED_TOKEN is URL-safe base64 of "server_address/invite_code".
   *
   * Fallback: ?server=...&code=... query string form.
   *
   * Returns { server, code } or null if the URL doesn't match.
   */
  function parseInviteUrl() {
    var path = window.location.pathname;

    // /join/ENCODED_TOKEN
    var joinMatch = path.match(/\/join\/([A-Za-z0-9_-]+)/);
    if (joinMatch) {
      try {
        var encoded = joinMatch[1].replace(/-/g, '+').replace(/_/g, '/');
        var decoded = atob(encoded);
        var slashIdx = decoded.indexOf('/');
        if (slashIdx > 0) {
          return {
            server: decoded.substring(0, slashIdx),
            code: decoded.substring(slashIdx + 1)
          };
        }
      } catch(e) {}
    }

    // Fallback: query params ?server=&code=
    var params = new URLSearchParams(window.location.search);
    var server = params.get('server');
    var code = params.get('code');
    if (server && code) {
      return { server: server, code: code };
    }

    return null;
  }

  /**
   * Build the deep link URI for the Farder desktop client.
   * farder://SERVER_ADDRESS/INVITE_CODE
   */
  function buildDeepLink(server, code) {
    return "farder://" + server + "/" + encodeURIComponent(code);
  }

  /**
   * Attempt to open the deep link, then fall back gracefully.
   * Browsers don't give us a reliable error callback when a protocol handler
   * is missing, so we use a short timeout heuristic: if the page is still
   * focused after ~1.5 s the app probably didn't open.
   */
  function tryOpenDeepLink(deepLink, statusEl) {
    statusEl.textContent = "Opening Farder\u2026";
    statusEl.className = "invite-status";

    var iframe = document.createElement("iframe");
    iframe.style.display = "none";
    document.body.appendChild(iframe);

    // The iframe trick works on most browsers without triggering a navigation.
    // If that fails (e.g. Firefox blocks it), fall back to location.href.
    try {
      iframe.src = deepLink;
    } catch (e) {
      window.location.href = deepLink;
    }

    var opened = false;

    var blurHandler = function () {
      opened = true;
      statusEl.textContent = "Farder opened! Check your taskbar.";
      statusEl.className = "invite-status success";
      window.removeEventListener("blur", blurHandler);
    };

    window.addEventListener("blur", blurHandler);

    setTimeout(function () {
      window.removeEventListener("blur", blurHandler);
      document.body.removeChild(iframe);

      if (!opened) {
        statusEl.textContent =
          "Farder does not appear to be installed. Download it below.";
        statusEl.className = "invite-status error";
        // Scroll to / highlight the download button
        var dlBtn = document.getElementById("btn-download");
        if (dlBtn) {
          dlBtn.classList.add("btn-blue");
          dlBtn.classList.remove("btn-primary");
          dlBtn.scrollIntoView({ behavior: "smooth", block: "center" });
        }
      }
    }, 1800);
  }

  /**
   * Render the "valid invite" UI into #invite-content.
   */
  function renderValidInvite(container, server, code) {
    var deepLink = buildDeepLink(server, code);

    container.innerHTML =
      '<div class="invite-header">' +
        '<div class="invite-server-icon">&#x1F310;</div>' +
        '<div class="invite-heading">You\u2019ve been invited to a Farder server!</div>' +
        '<div class="invite-subheading">Server: <strong>' + escapeHtml(server) + "</strong></div>" +
      "</div>" +

      '<div class="invite-code-display">' +
        '<div class="invite-code-label">Invite code</div>' +
        '<div class="invite-code-value">' + escapeHtml(code) + "</div>" +
      "</div>" +

      '<hr class="invite-separator">' +

      '<div class="invite-actions">' +
        '<button class="btn btn-blue" id="btn-open-farder">&#x1F680;&nbsp; Open in Farder</button>' +
        '<a href="../index.html#download" class="btn btn-primary" id="btn-download">&#x1F4BE;&nbsp; Download Farder</a>' +
        '<a href="../index.html" class="btn btn-primary btn-sm">&#x2753;&nbsp; What is Farder?</a>' +
      "</div>" +

      '<p class="invite-status" id="invite-status"></p>';

    // Wire up the "Open in Farder" button
    var openBtn = document.getElementById("btn-open-farder");
    var statusEl = document.getElementById("invite-status");

    openBtn.addEventListener("click", function () {
      tryOpenDeepLink(deepLink, statusEl);
    });

    // Auto-attempt on page load (only if there is an invite in the URL —
    // avoids confusing blank-page attempts)
    // Give the page a moment to finish rendering first.
    setTimeout(function () {
      tryOpenDeepLink(deepLink, statusEl);
    }, 600);
  }

  /**
   * Render the "invalid / missing invite" UI into #invite-content.
   */
  function renderInvalidInvite(container) {
    container.innerHTML =
      '<div class="invite-invalid">' +
        '<div class="invite-invalid-icon">&#x274C;</div>' +
        '<div class="invite-invalid-title">Invalid invite link</div>' +
        '<div class="invite-invalid-desc">' +
          "This invite link is missing a server address or invite code.<br>" +
          "Ask the server admin to generate a new invite." +
        "</div>" +
        '<a href="../index.html" class="btn btn-primary">&#x1F3E0;&nbsp; Go to Farder homepage</a>' +
      "</div>";
  }

  /** Minimal HTML escaping to prevent XSS from URL params. */
  function escapeHtml(str) {
    return String(str)
      .replace(/&/g, "&amp;")
      .replace(/</g, "&lt;")
      .replace(/>/g, "&gt;")
      .replace(/"/g, "&quot;")
      .replace(/'/g, "&#39;");
  }

  // ── Entry point ───────────────────────────────────────────

  document.addEventListener("DOMContentLoaded", function () {
    var container = document.getElementById("invite-content");
    if (!container) return;

    var parsed = parseInviteUrl();

    if (parsed && parsed.server && parsed.code) {
      renderValidInvite(container, parsed.server, parsed.code);
    } else {
      renderInvalidInvite(container);
    }
  });
})();
