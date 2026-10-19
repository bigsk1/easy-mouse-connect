# Repository images

`desktop-preview.png` shows the unpaired browser settings preview. It is illustrative UI documentation, not a capture of a connected native session.

`social-preview.png` is the 1280 × 640 GitHub social card, with a solid background and a file size below 1 MB. The stylized PCs use the settings interface's colors and mark; the illustration contains no real device details or input. The card has no release number so it can accompany later releases.

Upload the PNG in the repository's **Settings → General → Social preview → Edit → Upload an image**. GitHub's [social preview documentation](https://docs.github.com/en/repositories/managing-your-repositorys-settings-and-features/customizing-your-repository/customizing-your-repositorys-social-media-preview) describes the size and format requirements. Committing the file alone does not configure the repository's social preview.

Edit `social-preview.svg` to change the artwork, then render from the repository root:

```sh
npm ci
npx playwright install chromium
node scripts/render-social-preview.mjs
```

The renderer uses the existing Playwright development dependency, blocks remote requests, and checks the PNG dimensions and size. Set `PLAYWRIGHT_CHROMIUM_EXECUTABLE_PATH` to use an existing Chromium or Chrome executable. The SVG uses installed Segoe UI on Windows, with Ubuntu Sans and sans-serif fallbacks; text metrics can differ slightly across systems. No fonts or additional runtime dependencies are bundled. The PNG is the upload asset; the SVG is its editable source.
