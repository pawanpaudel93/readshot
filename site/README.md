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
- `favicon.svg` — app-icon aligned favicon.

## Install Redirect

`/install.sh` should return a temporary redirect to:

```text
https://raw.githubusercontent.com/pawanpaudel93/readshot/main/install.sh
```

Included configs:

- `_redirects` for Netlify and Cloudflare Pages.
- `vercel.json` for Vercel.

The public install command is:

```bash
curl -fsSL https://readshot.pawanpaudel.com.np/install.sh | bash
```
