# herdmux website

Documentation site for [herdmux](https://github.com/irhtml/herdmux), built with [Astro Starlight](https://starlight.astro.build/).

## Local development

```sh
npm install
npm run dev
```

Open <http://localhost:4321/herdmux/> to view.

## Build

```sh
npm run build     # generates dist/
npm run preview   # serves built site locally
```

Deployed to GitHub Pages by `.github/workflows/deploy-website.yml` on every push to `main` that touches `website/**` or the workflow file itself.
