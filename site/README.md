# Readshot Site

Static site for `https://readshot.pawanpaudel.com.np`.

Deploy the `site/` directory as the web root. The site is plain HTML/CSS/JS and
does not require a build step.

Pages:

- `index.html` — product page and installer.
- `cli.html` — command-line documentation.
- `mcp.html` — MCP setup and tool reference.

Shared assets:

- `styles.css` — site styling.
- `script.js` — copy-button behavior. When the browser blocks the
  clipboard, the button turns into an error and a line under it says to
  select the text and press ⌘C (macOS) or Ctrl+C, or just to select and
  copy it on a touch-only device. The `.copy-status` live region
  announces the same message.
- `favicon.svg`, `favicon.ico`, `apple-touch-icon.png`, `icon-192.png`,
  `icon-512.png`, `site.webmanifest` — the app icon for browsers and home
  screens.
- `og-image.png` — the 1200×630 link-preview image (`og:image`,
  `twitter:card`).

Every icon image is generated from `packaging/macos/icon.svg` (and
`icon-16.svg` for the 16 px favicon frame, `icon-small.svg` for the 32 px
one) by
`packaging/render-icons.sh`; edit the SVG and rerun it rather than editing
the images.

## Install Redirect

`/install.sh` should return a temporary redirect to:

```text
https://github.com/pawanpaudel93/readshot/releases/latest/download/install.sh
```

Included configs:

- `_redirects` for Netlify and Cloudflare Pages.
- `vercel.json` for Vercel. It also sets `cleanUrls`: Vercel serves
  `cli.html` at `/cli` and `mcp.html` at `/mcp`, and redirects the
  `.html` addresses there.

The public install command is:

```bash
curl -fsSL https://readshot.pawanpaudel.com.np/install.sh | bash
```
