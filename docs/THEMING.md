# Theming

hyprgallery is styled with GTK4 CSS. The built-in stylesheet is
[`crates/hyprgallery-ui/src/default.css`](../crates/hyprgallery-ui/src/default.css).
Anything in `~/.config/hyprgallery/style.css` is loaded after it and reloaded
whenever you save the file. There is no need to restart the daemon.

Both sit above GTK's user priority, so a desktop-wide theme in
`~/.config/gtk-4.0/gtk.css` does not restyle the gallery. That is deliberate:
setups like HyDE put a complete theme there, and its button and entry rules
would otherwise fight the gallery's own. Change the gallery in its own
`style.css`.

The selectors below are public API. They will not be renamed without a note in
the changelog.

## Colours

| Name | What it is |
| --- | --- |
| `@hg_accent` | Taken from the focused wallpaper and replaced as focus moves. Used for the active album, the selection ring and busy text. |
| `@hg_accent_fg` | Text drawn on top of `@hg_accent`. |
| `@hg_text` | Primary text. |
| `@hg_dim` | Secondary text and inactive controls. |
| `@hg_glass` | The popover panel's background. |

`@hg_accent` is defined by the gallery at runtime, above the built-in
stylesheet and below yours. Redefining it in `style.css` does not stop it
changing; to keep a fixed accent, set colours on the selectors that use it,
for example:

```css
.hg-album.active { background: #89b4fa; color: #11111b; }
gridview.hg-grid > child:selected hg-tile { outline-color: #89b4fa; }
```

## The window

| Selector | What it is |
| --- | --- |
| `window#hyprgallery` | The layer surface. Transparent; keep it that way. |
| `hg-backdrop` | The full-screen wash behind the overlay layout. Drawn in code. |
| `.hg-content-box` | The overlay layout's content: header, grid, footer. |
| `.hg-panel` | The popover layout's panel. |
| `.hg-catcher` | The transparent area around the popover panel that closes it when clicked. |

## Albums

| Selector | What it is |
| --- | --- |
| `.hg-header` | The row holding the albums and the search box. |
| `.hg-album` | An album in the overlay layout. `.active` for the one shown, `.smart` for Favourites, Recent, By colour and Animated. |
| `.hg-album-name`, `.hg-album-count` | Its two labels. |
| `.hg-separator` | The line between your folders and the smart albums. |
| `.hg-shelf`, `.hg-shelf-item` | The album covers in the popover layout. `.active` as above. |
| `hg-cover` | The fanned stack of thumbnails on a shelf item. Drawn in code. |
| `searchentry.hg-search` | The search box. |
| `.hg-hues`, `.hg-hue` | The colour filters under By colour. `.active` for the chosen one. |

## The grid

| Selector | What it is |
| --- | --- |
| `gridview.hg-grid` | The grid. |
| `gridview.hg-grid > child` | A grid cell. Its padding is the gap between tiles. `:selected` for the focused one. |
| `hg-tile` | One wallpaper. `border-radius`, `outline`, `box-shadow` and `background-color` (shown before the thumbnail arrives) apply. The picture, name, badges and spinner inside are drawn in code. |
| `.hg-empty`, `.hg-empty-title`, `.hg-empty-hint` | What shows when an album or a search has nothing in it. |

The tile's contents are drawn rather than built from widgets, so their text
and badge colours are not themeable. Corner radius, the selection ring and the
shadow are.

## Footer and messages

| Selector | What it is |
| --- | --- |
| `.hg-footer` | The bottom row. |
| `.hg-key`, `.hg-hint` | A key and its description in the shortcut hints. |
| `.hg-status` | The name of the focused wallpaper, or what the gallery is busy with. `.busy` while thumbnailing or applying. |
| `menubutton.hg-rotate` | The rotation menu. `.active` while a rotation is running. |
| `.hg-toast` | A message at the bottom. `.error` for problems. |
| `.hg-drop` | The outline shown while files are dragged over the gallery. |

## The Waybar module

The module is a Waybar custom module, styled in Waybar's own stylesheet as
`#custom-hyprgallery`. Its state is a class: `idle`, `rotating`, `applying`,
`error` or `offline`, plus `open` while the gallery is on screen. A starting
point is in [`contrib/waybar/hyprgallery.css`](../contrib/waybar/hyprgallery.css).
