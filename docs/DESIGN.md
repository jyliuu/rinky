# rinky design system

The look of every screen comes from `crates/ink-ui`: the tokens in `theme.rs` and the
widgets built on them. This file lists the tokens, says when to use each widget, and
lists the e-ink rules every screen must keep. Screen agents restyle their screens with
these pieces; they do not invent new grays, sizes or line widths.

![Design tab of the widget gallery, day](design/gallery-design-day.png)
![The same tab in night mode](design/gallery-design-night.png)

The pictures are the "Design" tab of the widget gallery (`rinky demo`, tab 6),
rendered headless at 1272 x 1696 and scaled to 40 %:

```sh
rinky demo --headless --script crates/ink-app/src/screens/demo/gallery.txt --out DIR
```

## Screen and units

- Panel: 1272 x 1696 pixels at 300 ppi (Kindle Paperwhite 12). 8 px = 0.68 mm.
- Touch targets: at least 96 px (8.1 mm) in both directions; `Theme::touch_px` is 108 and
  `Theme::control_h()` (104) is the height of buttons, chips and segmented controls.
- All sizes below are pixels at this density.

## Tokens (`ink_ui::Theme`, `ink_ui::theme`)

### Palette

One palette; night mode is the panel inverting it (`Theme::night()` sets only the
`night` flag, the device shows `255 - value`). Every gray is an exact step of the
16-level panel (a multiple of 0x11), so nothing dithers.

| Token | Value | Use | Night shows |
| --- | --- | --- | --- |
| `fg` (`gray::INK`) | 0x00 | text, icons, outlines, filled buttons, rules | white |
| `muted` (`gray::MUTED`) | 0x55 | secondary text: authors, details, captions | 0xAA |
| `subtle` (`gray::SUBTLE`) | 0xAA | disabled outlines, grab handles, popup shadow | 0x55 |
| `light` (`gray::LIGHT`) | 0xDD | fills: progress tracks, placeholder covers, filled cards, keyboard special keys, list dividers | 0x22 |
| `bg` (`gray::PAPER`) | 0xFF | background | black |

Only these five values. Pictures (covers, book pages) keep their own grays.

### Type scale (`TextRole { px, style }`)

The only face is Noto Serif (regular, bold, italic). Bold carries emphasis; size steps
are large enough to tell apart on the panel.

| Role | Size | Weight | Use |
| --- | --- | --- | --- |
| `title` | 56 | bold | title of a top-level screen (app bar without back) |
| `heading` | 46 | bold | sub-screen titles (app bar with back), dialog and sheet titles, card titles |
| `body` (= `font_px`) | 40 | regular | list rows, menus, settings, dialog text |
| `label` | 36 | regular | button and segment captions (bold when filled or selected) |
| `caption` (= `small_px`) | 28 | regular | secondary lines, details, status, nav bar captions, progress labels |

`font_px` and `small_px` stay as they were, so code that uses them keeps its look.

### Spacing

An 8 px grid: `ink_ui::theme::space(n)` = `8 * n` (also `Theme::space`). Use
`space(1)` (8) inside controls, `space(2)` (16) between related items, `space(3)` (24)
for padding in cards and sheets, `space(4)` (32) for screen margins. Rows and bars use
multiples of 8 as well (`APP_BAR_H` 136, `NAV_BAR_H` 144, list rows 120-160).

### Shape and lines

| Token | Value | Use |
| --- | --- | --- |
| `radius` | 12 | keys, small controls, cover placeholders (8) |
| `radius_lg` | 24 | cards, dialogs, popups, menus, sheets |
| buttons, chips, segmented | height / 2 | pills |
| `border_px` | 3 | outlines of buttons, cards, dialogs, toggles, thick progress |
| `rule_px` | 3 | dividers and bar rules (black under bars, `light` between rows) |
| `focus_px` | 6 | outline of a focused or selected element |

### Motion

No multi-frame animation (see "Motion" below). `anim_frames` and `anim_frame_ms` are
still in `Theme` but nothing reads them.

## Widgets

All widgets work in a `WidgetTree` and also painted directly with
`Widget::paint(&mut cx, rect)` by screens that draw themselves (library, reader footer,
statistics): outside a tree there is no clip, so give the widget its full rectangle.
Every widget keeps its ink inside its bounds (`crates/ink-ui/tests/verify_ui_cycle2.rs`
checks each with long texts). Hit rectangles are public (`cells`, `action_rects`,
`back_rect`, `rects`, `switch_rect`, `Slider::parts`) for screens that hit-test
themselves.

### Buttons: `Button`, `ButtonKind`, `paint_button`

Pills in three kinds:

- `Button::filled` (`ButtonKind::Filled`): black, bold white caption. One per screen or
  dialog: the main action (Read, OK, Save, Full view).
- `Button::new` (`Outlined`, the default): white with a 3 px black outline. Secondary
  actions (Details, Cancel in a dialog, filters).
- `Button::text` (`Text`): bold caption, no frame. Low-emphasis actions and icon-only
  buttons in bars.
- `.disabled()`: gray caption, `subtle` outline. `inverted = true` still means filled.
- Pressed: inverted, DU. `paint_button(cx, rect, label, icon, kind, enabled, pressed)`
  draws the same face for screens that draw directly (replaces
  `library::view::button`).
- Dialogs make the last of several buttons filled; popups keep all buttons outlined
  (their tools have equal weight).

### App bar: `AppBar`, `BarAction`

The top of every screen, `APP_BAR_H` (136) tall: optional back chevron (reports `back`),
title (title role; heading role with a back button), optional subtitle (muted caption),
trailing actions (`BarAction::icon` 104 px round buttons, `BarAction::text` outlined
pills), black 3 px rule along the bottom. Use it instead of hand-drawn "Exit"/"‹"
buttons. Actions that do not fit are dropped from the right end of the list.

### Bottom navigation: `NavBar`, `NavItem`

Top-level destinations (Home, Library, Collections, Stats...), `NAV_BAR_H` (144) tall:
black rule on top, equal cells with icon and caption; the selected destination's icon
sits on a black pill and its caption is bold. A tap reports the item's action and
selects it; only the two changed cells are refreshed. 3 to 5 items.

### Segmented control: `Segmented`

One pill split into 2-4 mutually exclusive options (List / Grid / Covers, Day / Week /
Month). Selected segment black with bold white text. Reports `action:index`. Use for
view modes; use `Tabs` for switching whole pages of content.

### Chips: `Chips`, `Chip`

Wrapping row of pill-shaped filters (status, format, collection). Off: outlined; on:
black with a check mark. Multi-select by default, `.single()` for one-of-many. Reports
`action:index:on|off`. Chip rectangles come from the last paint (`rects()`).

### Tabs: `Tabs`

Text tabs with a bold, underlined selection and a 3 px black rule. For pages of one
screen (Contents / Bookmarks, Overview / Books).

### Lists: `List`, `ListItem`, `Cover`, `paint_cover`

Paged rows (no scrolling), touch-sized (at least 110 px; 150-180 with covers). A row
can have a leading icon or a book cover (`ListItem::cover`, 2:3, picture scaled with a
black frame, or a light placeholder with the title), title, muted italic subtitle,
right-aligned detail, check mark, and `ListItem::progress` (a thin bar with the
percentage along the bottom third). Dividers are 3 px `light`. Pressed row: inverted,
DU, only that row refreshed. Page footer with arrows and "n / m".

### Scroll bar: `ScrollBar`

A thin (14 px) vertical bar for lists that move a screenful at a time (`ink_ui::scrollbar`):
`Theme::track()` track, black thumb for the shown part, nothing when everything fits. A
value type (`total`, `first`, `shown`; `ScrollBar::paged(total, per, page)`); the
screen gives it the scrolling region including a 48 px column on the right
(`ScrollBar::content` is the rest, `ScrollBar::track` the bar, `ScrollBar::hit_rect` a
96 px wide touch zone). `item_at(track, y)` maps a tap or drag to an item. While a finger
drags, repaint only the track with `paint_drag` (paper track, black thumb) as DU; the lift
repaints rows and bar once with GL16. `ScrollView` draws its bar with it.

### Cards: `Card`, `CardKind`

A `radius_lg` panel for one object: "Continue reading", a book in a grid, a statistic.
Outlined (default) or filled (`light`). Optional cover on the left, title (heading),
subtitle, wrapped body, thin progress along the bottom; a tap reports its action
(pressed: black, DU). Other widgets (a `RingProgress` badge) can be added as children.

### Progress: `ProgressBar`, `ProgressKind`, `RingProgress`

- `ProgressBar::thin(v)`: 8 px `light` track, black fill. Reader footer, list rows,
  cards. Chapter starts (`with_marks`) are 4 px black ticks reaching above and below
  the track. `with_label("42%")` on the right (bold caption), `with_caption("Act II")`
  above on the left (muted).
- `ProgressBar::new(v)` (thick): 16-24 px rounded bar with a 3 px outline; chapter
  starts are white notches. Downloads, imports, the go-to page.
- `RingProgress::new(v)`: a ring on a paper disc with a black edge, so it reads on any
  cover; the percentage in the middle (`.bare()` for none). Place it with
  `RingProgress::badge_rect(cover, side)` in the cover's bottom-right corner (side about
  a third of the cover width, at least 64).
- Values outside 0..=1 and NaN are clamped (NaN = 0).

### Toggles and sliders: `Toggle`, `Slider`

Toggle: on = black track with a white knob holding a check mark; off = `light` track
with a black outline and a black knob. Slider: 12 px track, black fill, white knob with a
black ring (black while dragging, DU), round minus/plus buttons. Geometry unchanged.

### Overlays: `Dialog`, `Popup`, `Menu`, `BottomSheet`, `Toast`, `Scrim`

- `Dialog`: modal question; `radius_lg`, 3 px outline, heading-role title left-aligned,
  outlined buttons and one filled primary (last). Put a `Scrim` under it.
- `Popup`: anchored box (footnotes, dictionary, selection tools) with a solid `subtle`
  offset shadow (no blur) and outlined pill buttons.
- `Menu`: list of actions in a rounded frame, 3 px `light` dividers. A label that
  starts with "\u{2713} " marks the chosen row: the widget draws `Icon::Check` in a
  gutter on the left (all rows keep the gutter), so the mark does not need a font glyph.
- `BottomSheet`: panel standing on the bottom edge ("Aa"), rounded top corners, black
  edge, `subtle` grab handle (120 x 12), heading-role title; a swipe down reports
  `close`. Content goes in `BottomSheet::content(frame)`.
- `Toast`: black pill with white text near the bottom, DU, removes itself.

### Settings pages and settings popups (`crates/ink-app/src/screens/settings`)

Every settings page and every settings popup is built from the same rows; nothing is
drawn by hand for one setting:

- `rows::Row` / `rows::SettingsList`: headers, notes, switches (`Row::toggle`), choices
  (`Row::choice`), links to sub-pages (`Row::nav`), actions (`Row::item`), ordered items
  (`Trail::Order`), sliders (`Row::slider`), Wi-Fi signal bars and locks. A list jump is
  one DU (the rows are black and white), a changed row repaints alone; the scroll bar is
  `ink_ui::ScrollBar`.
- `page`: the full page, `add_bar` (app bar with back), `add_list`, `add_status`.
  Shown whole with REAGL (`Change::LargePanel`), never a GC16 flash.
- `popup::SettingsPopup` + `popup::PopupCatcher`: title, Close button, optional note and
  a `SettingsList`; a tap outside closes. Opens and closes with GL16 of its box only.
  Control-center long presses (all of them, plugin tiles too) and every plugin menu
  (`plugin_host::MenuBox`) use it. Plugin menus become rows with `popup::menu_rows`.

### Icons: `Icon`, `IconView`

42 vector icons on a 24-unit grid with 2-unit round-ended strokes, drawn at any size
(`Icon::draw`, `PaintCx::icon`): navigation (Back, Forward, Up, Down, Close, Menu, More),
reading (Book, Library, Toc, Bookmark, Font, Search, Chat), views (Grid, ListView, Sort,
Filter, Folder), status (Light, Night, Wifi, Battery, Clock, Sync, Download, Chart),
editing and keyboard keys. Draw icons at 44-56 px in bars and rows, about half the
button side in icon buttons. `Icon::ALL` lists them for the gallery.

## E-ink rules

1. **Grays**: only the five palette values in UI chrome. No gradients, no blurred
   shadows, no anti-aliased gray fills larger than a stroke edge.
2. **Lines**: black lines may be 2-3 px; gray lines (dividers, tracks) are at least
   `rule_px` (3) thick. 1 px gray lines ghost and disappear under DU; never use them.
3. **Touch**: every tappable thing at least 96 x 96 px; leave `space(2)` between
   neighbouring targets.
4. **Refresh classes**:
   - press feedback, keys, toasts, slider drag: DU (2 levels). Anything shown with
     DU must read in pure black and white: pressed states invert, they never use gray.
   - widget changes: GL16 (the default `Widget::refresh_class`); report only the
     rectangles that changed (`Widget::damage`).
   - page turns: REAGL via `RefreshScheduler::page_turn()`.
   - popups, menus, dialogs, sheets and keyboards never flash, opening or closing:
     one GL16 of the rectangle they cover (REAGL over page text). See "Flash policy".
5. **Night mode**: test every screen with `Theme::night()` (the gallery's moon button).
   `light` fills become near-black (0x22) at night: they must never carry information
   alone; pair them with a black outline or black content. Progress and slider tracks
   use `Theme::track()`: `light` by day, `subtle` at night (shown as 0x55). The app's
   theme follows the panel's night flag, so `theme.night` is true while night mode is on.
6. **Text**: body text in `fg`; `muted` only for secondary lines at 28 px or more.

## Motion

There is no multi-frame animation: no wipe, no slide-in, no fade, no animated scroll.
The user tested those on the Kindle and they made it feel slow. The rule is one change,
one update:

- Every change of the screen is drawn into the canvas in its final state, then sent
  with one `Screen::update(rect, class)` for the rectangle that changed. Overlays,
  sheets, menus and panels appear whole in that one update; closing one repaints only
  the rectangle it covered (one update).
- A page turn is one update of the page area, including the status bars, so the
  progress bar never refreshes on its own. The page-turn direction wave
  (`Screen::update_wave`, `ink_device::Wave`) is done by the panel controller inside
  that one update; the app sends no extra frames.
- Lists jump half a page per swipe, at once; the library views (list, grid, collections,
  files) jump one screenful per swipe or page key, and a tap or drag on their scroll bar
  jumps there. Nothing scrolls under the finger. A scroll step is one GL16 of the
  scrolling region only (rows and bar), not the whole screen.
- A slider may repaint while it is dragged, with DU and only its own rectangle; only
  the newest value is drawn, no frames are replayed. The release gives the final state.
- Press feedback (a button inverting) is one DU update of that button.

The refresh class is chosen per change (see "E-ink rules", item 4):

| change | class |
| --- | --- |
| press feedback, keyboard keys, toasts, slider drag | DU (pure black and white) |
| widget, list, menu, sheet, panel opening or closing | GL16 of the changed rectangle |
| page turn, jump (TOC, Go to, link, search hit) | REAGL from `RefreshScheduler::page_turn()`; GC16 every N turns, on image pages, or when ghosting debt is due |
| night mode, rotation, entering a full screen, leaving one, "Refresh screen" | one GC16 of the screen |

### Flash policy

A GC16 flash takes about 300 ms on the PW12 and blinks the whole panel. Only these flash:

- page turns and jumps, every N turns ("Full refresh: every N pages", ghost debt);
- the explicit full refresh ("Refresh screen", "Clear ghosting");
- night mode on or off, a rotation (the panel shows the old image everywhere);
- a full screen pushed or popped (library -> reader, reader -> library, settings ->
  statistics): the whole image changes;
- a frame whose pixels really changed over 80 % of the screen
  (`ink_ui::refresh::FLASH_PERCENT`: a library tab or page of covers).

Never a flash: a popup, menu, dialog, sheet or keyboard opening or closing, and closing
a panel screen (the control center). The app loop keeps a copy of what the panel shows
(`crates/ink-app/src/shown.rs`); a frame that would flash is first cut to the pixels
that differ from it (`ink_ui::refresh::trim_to_changes`), so a screen that repaints all
of its canvas for a menu (the library) shows a GL16 of the menu. After the pop of a
panel screen the app shows the screen below's GC16 as GL16. Full-screen overlays (Go to,
search, plugin viewers) open and close with REAGL; the reader adds ghost debt instead of
a flash. Streamed text replacing a plugin view in place (an AI answer arriving in
batches) refreshes only the lines that changed (GL16); a batch that moves a large view
to a new page is REAGL. Tests: `popups_open_and_close_with_a_partial_gl16_of_their_rectangle`
(library), `library_popups_and_the_control_center_never_flash` and
`reader_menu_opens_and_closes_without_a_flash` (`tests/screens.rs`).

Do not add animation settings. The panel has no A2 class in `ink_device::RefreshClass`.

## Screens and ownership

`crates/ink-ui` is owned by the design system; screens are owned by their agents
(`docs/ARCHITECTURE.md`, "Ownership map"). Screens that paint directly should move to
`AppBar`, `NavBar`, `paint_button`, `ProgressBar`, `Card`/`paint_cover` and the palette
tokens instead of their own `button`/`progress_bar` helpers and hard-coded 1-2 px gray
lines.
