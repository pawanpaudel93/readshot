# Readshot Site

Static site for `https://readshot.pawanpaudel.com.np`.

Deploy the `site/` directory as the web root. The page is plain HTML/CSS and
does not require a build step.

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
