# Hosting the invite website

`website/` is four static files. Its job is one URL:
`https://<your-domain>/join/<code>` — the link people paste to their friends. The
page decodes the token and hands it to the Farder app.

## Why it runs on your own box

Whoever serves this page sees the IP of everyone who clicks an invite. That is
precisely the thing Farder exists not to hand to a third party, and invite
clickers are not even users yet — they are people deciding whether to become
one. On your VPS those logs are yours. On a hosting platform they are the
platform's.

There is a second, duller reason: the page reads the token out of
`window.location.pathname`, so the server has to **rewrite** `/join/*` to the
invite page while keeping the URL intact. A redirect would change the path and
take the token with it. Static hosts that cannot rewrite (GitHub Pages) need a
404-page trick to fake it; one line of Caddy does it properly.

## Running it

It is the `web` profile of the relay's compose file — the same box, one command:

```bash
FARDER_SITE_DOMAIN=farder.xyz \
  docker compose -f deploy/relay/docker-compose.yml --profile web up -d
```

Or let `bootstrap.sh` do everything at once:

```bash
FARDER_SITE_DOMAIN=farder.xyz bash deploy/relay/bootstrap.sh
```

Caddy obtains and renews a Let's Encrypt certificate by itself. For that to
work, **DNS must already point at this box and TCP 80 must be reachable** —
port 80 is how the certificate is issued, even though the site ends up on 443.

## DNS records

Both records point at the same machine, because the relay and the site live
there together. Replace the IP with your VPS's.

| Type | Host    | Value          | TTL       | What it does                          |
|------|---------|----------------|-----------|---------------------------------------|
| A    | `@`     | `203.0.113.42` | Automatic | `farder.xyz` — the site + invite links |
| A    | `relay` | `203.0.113.42` | Automatic | `relay.farder.xyz` — a stable name for the relay |

On Namecheap: **Domain List → Manage → Advanced DNS → Add New Record**. Delete
the parking-page records it ships with (a CNAME on `www` pointing at
`parkingpage.namecheap.com`, and any URL-redirect record on `@`), or they will
fight with yours.

`relay.farder.xyz` is **documentation, not configuration**, until the client
learns to resolve hostnames: `DEFAULT_RELAY.addr` is parsed as a literal
`SocketAddr`, so putting a hostname there silently leaves the build with no
default relay. Add the record anyway — it costs nothing and it is what a
hostname-resolving client would later use.

## Checking it

```bash
# From anywhere, once DNS has propagated:
curl -sI https://farder.xyz/ | head -1
curl -s https://farder.xyz/join/ZmFyZGVyOi8vcmVsYXlkLzAwL0FiQw | grep -o "<title>.*</title>"
```

The second one must return the INVITE page's title, not the landing page's —
that is the rewrite working. If it returns the landing page, the `rewrite` line
in `deploy/website/Caddyfile` is not being applied.
