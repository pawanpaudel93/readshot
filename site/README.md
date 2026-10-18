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
- `script.js` — copy-button behavior.
- `favicon.svg`, `favicon.ico`, `apple-touch-icon.png`, `icon-192.png`,
  `icon-512.png`, `site.webmanifest` — the app icon for browsers and home
  screens.
- `og-image.png` — the 1200×630 link-preview image (`og:image`,
  `twitter:card`).

Every icon image is generated from `packaging/macos/icon.svg` (and
`icon-small.svg` for the 16/32 px favicon frames) by
`packaging/render-icons.sh`; edit the SVG and rerun it rather than editing
the images.

## Install Redirect

`/install.sh` should return a temporary redirect to:

```text
https://github.com/pawanpaudel93/readshot/releases/latest/download/install.sh
```

Included configs:

- `_redirects` for Netlify and Cloudflare Pages.
- `vercel.json` for Vercel.

The public install command is:

```bash
curl -fsSL https://readshot.pawanpaudel.com.np/install.sh | bash
```
