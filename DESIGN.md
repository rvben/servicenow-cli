---
name: ServiceNow CLI TUI
description: A keyboard-first operations ledger for locating, scanning, and inspecting ServiceNow records.
colors:
  primary-mint: "#65f0ca"
  secondary-amber: "#f6b752"
  tertiary-coral: "#ff7e77"
  midnight-canvas: "#080e12"
  selection-ink: "#04181b"
  header-surface: "#0f1b20"
  hairline-slate: "#354a4d"
  muted-slate: "#7b9191"
  header-text: "#9ab1ae"
  body-text: "#d3dcda"
  high-text: "#eef6f2"
typography:
  body:
    fontFamily: "monospace"
    fontWeight: 400
  title:
    fontFamily: "monospace"
    fontWeight: 700
  label:
    fontFamily: "monospace"
    fontWeight: 700
rounded:
  square: "0"
spacing:
  cell: "1ch"
  inset: "2ch"
  row: "1lh"
  chrome-band: "3lh"
components:
  brand-stamp:
    backgroundColor: "{colors.primary-mint}"
    textColor: "{colors.selection-ink}"
    typography: "{typography.label}"
    rounded: "{rounded.square}"
    padding: "0 1ch"
  ledger-column-header:
    backgroundColor: "{colors.header-surface}"
    textColor: "{colors.header-text}"
    typography: "{typography.label}"
    rounded: "{rounded.square}"
    height: "{spacing.row}"
  ledger-row-selected:
    backgroundColor: "{colors.primary-mint}"
    textColor: "{colors.selection-ink}"
    typography: "{typography.label}"
    rounded: "{rounded.square}"
    height: "{spacing.row}"
  record-sheet:
    backgroundColor: "{colors.midnight-canvas}"
    textColor: "{colors.body-text}"
    typography: "{typography.body}"
    rounded: "{rounded.square}"
    padding: "{spacing.cell}"
  incident-workspace:
    backgroundColor: "{colors.midnight-canvas}"
    textColor: "{colors.body-text}"
    typography: "{typography.body}"
    rounded: "{rounded.square}"
    padding: "{spacing.cell}"
  incident-tab-active:
    backgroundColor: "{colors.midnight-canvas}"
    textColor: "{colors.primary-mint}"
    typography: "{typography.label}"
    rounded: "{rounded.square}"
    height: "{spacing.row}"
  input-sheet:
    backgroundColor: "{colors.midnight-canvas}"
    textColor: "{colors.body-text}"
    typography: "{typography.body}"
    rounded: "{rounded.square}"
    padding: "{spacing.cell}"
    height: "7lh"
---

# Design System: ServiceNow CLI TUI

## Overview

**Creative North Star: "The Operations Ledger"**

The TUI treats live ServiceNow data as an operator's ledger: located, inspectable, dense, and calm. Midnight ink holds the field while mint marks identity, indexed fields, success, and the one active record. Amber is reserved for executable keys and attention; coral names failure. This is an operational instrument, not a miniature browser dashboard.

Information density is intentional. One-row records, uppercase ledger labels, square hairline enclosures, and persistent location and status bands make the screen read like a precise terminal artifact. Detail is progressive: a wide terminal shows an index preview beside the ledger; generic records unfold into an all-fields sheet, while incidents unfold into a four-view workspace that loads related context only when requested.

The implementation follows the direction contract in `src/tui.rs` (operations-ledger direction, seed `221c1ea6`). Its visual semantics remain meaningful without color through borders, labels, copy, position, and the punched `▌` selection marker.

**Key Characteristics:**

- Midnight terminal canvas with restrained, role-specific accents.
- Dense one-row record indexing and visibly punched selection.
- Square sheets and single-cell hairline rules instead of card chrome.
- Wide split view that collapses to a ledger-first compact view.
- Full record, help, and input sheets that preserve keyboard context.
- Incident detail organized as Overview, Activity, Attachments, and SLAs without weakening generic-table browsing.
- Explicit loading, failure, empty, and undersized-terminal states.
- Keyboard-only operation with a complete no-color fallback.

## Colors

The palette is a dark operational spectrum: cool neutrals establish hierarchy, mint carries active state, amber identifies keys, and coral is kept exclusively urgent.

### Primary

- **Index Mint** (`primary-mint`, `#65f0ca`): Brand stamp, selected row, active sheet borders, active incident tab, field labels, detail title, and success notices. Mint means current, indexed, or successfully resolved.

### Secondary

- **Key Amber** (`secondary-amber`, `#f6b752`): Keyboard glyphs and the input chevron. It points to an action the operator can take; explanatory text beside it remains muted.

### Tertiary

- **Failure Coral** (`tertiary-coral`, `#ff7e77`): Error notices, failure headings, and the failed-load enclosure. It is never used as decoration or for ordinary attention.

### Neutral

- **Midnight Canvas** (`midnight-canvas`, `#080e12`): The root terminal field and sheet background.
- **Selection Ink** (`selection-ink`, `#04181b`): Foreground on mint-filled brand and selection states, preserving hard contrast.
- **Header Surface** (`header-surface`, `#0f1b20`): The only tonal panel fill, used behind ledger column headings.
- **Hairline Slate** (`hairline-slate`, `#354a4d`): Default borders, dividers, and inactive enclosures.
- **Muted Slate** (`muted-slate`, `#7b9191`): Location metadata, prompts, descriptions, query context, disabled pagination keys, and quiet notices.
- **Header Text** (`header-text`, `#9ab1ae`): Bold ledger column labels over the header surface.
- **Body Text** (`body-text`, `#d3dcda`): Record values and normal explanatory copy.
- **High Text** (`high-text`, `#eef6f2`): Bold titles and state headings that need priority without implying action.

**The One Active Mark Rule.** Mint may fill the selected ledger row and the brand stamp; elsewhere it stays a foreground or hairline. Do not create competing mint panels.

**The Semantic Accent Rule.** Amber always denotes a key or prompt, coral always denotes failure, and neither substitutes for generic emphasis.

**The Structural Fallback Rule.** When color is disabled, foreground and background colors resolve to terminal defaults while structural glyphs, bold weight, underline, labels, and recovery copy remain. Selection, focus, state, and recovery must never depend on hue.

## Typography

**Display Font:** Host terminal monospace
**Body Font:** Host terminal monospace
**Label/Mono Font:** Host terminal monospace

**Character:** Typography inherits the operator's terminal face and cell size. Hierarchy comes from weight, uppercase ledger language, spacing, and enclosure titles rather than from multiple fonts or scalable display sizes.

### Hierarchy

- **Title** (bold, terminal default size): Product identity, state headings, selected record title, and enclosure titles.
- **Body** (regular, terminal default size): Record values, instructions, descriptions, and notices.
- **Label** (bold, uppercase where it names ledger structure): Column headings, field names, incident tabs, `ALL FIELDS`, and keyboard glyphs. The active incident tab adds underline.
- **Muted metadata** (regular, terminal default size): Profile, instance, table, page, query, counts, scroll position, and supporting copy.

**The Terminal Owns the Typeface Rule.** Do not prescribe a downloaded font, a pixel size, or proportional text. The host monospace grid is part of the product contract.

**The Ledger Case Rule.** Uppercase is for short structural labels and decisive state headings, never for record values or explanatory prose.

## Layout

The base shell is three vertical bands: a 3-row header, a body with at least 8 rows, and a 3-row footer. A bottom hairline closes the header and a top hairline opens the footer. Body content touches its ledger frame; loading and failed-load panels are inset by 2 columns and 2 rows.

At 104 columns and wider, the body splits into a 62% record index and a 38% persistent index preview. Below 104 columns, the ledger owns the body and detail appears only when explicitly opened. Header metadata also compresses: at 80 columns and wider it shows profile, compact instance, table, and page; below 80 it omits the instance but preserves profile, table, and page.

Ledger columns progressively disclose according to available ledger width: 2 columns below 58, 3 below 82, 4 below 112, and up to 6 at 112 or wider. Description fields receive twice the ordinary column weight; system IDs receive one-and-a-half times the ordinary weight. Rows and column headings are exactly one terminal row high, with a one-column gap between columns.

Expanded record detail is centered, inset by at least 2 cells on each side, and capped at 112 columns by 38 rows. An incident workspace reserves a 3-row internal header for its record title, tab rail, and bottom rule; the active view body takes the rest with a one-column horizontal inset. At workspace widths below 72 columns, `ATTACHMENTS` shortens to `FILES`. Per-tab counts appear only at 78 columns and wider so navigation labels remain intact. The table and query input sheet is centered at up to 76 columns by 7 rows. The keyboard map is centered at up to 66 columns by 22 rows. These sheets clear their target rectangle and use an active mint border so modality is evident.

Below 50 columns or 12 rows, the application replaces the shell with a centered recovery message asking for at least `50 × 12`; only quit guidance remains. This tiny-terminal state is a deliberate mode, not a clipped version of the ledger.

**The Ledger-First Rule.** Responsive reduction removes preview detail and low-priority columns before it compromises the record index.

**The Fixed Chrome Rule.** Preserve the 3-row header and footer bands; give the remaining vertical space to records and sheets.

## Elevation & Depth

The system is entirely flat. It uses no shadows, gradients, blur, translucency, or simulated elevation. Depth comes from square box borders, darker header tonality, full-rectangle clearing beneath modal sheets, and the shift from slate to mint rules when a sheet is active.

**The Hairline Depth Rule.** A single terminal-cell rule is the only separator at rest; active modality changes its color, never its thickness.

## Shapes

All forms are square and terminal-native. Panels, ledger frames, loading and failure states, record sheets, incident workspaces, help, and inputs use Ratatui's single-line box geometry with zero corner radius. The selected ledger row and active incident tab are punched with a leading `▌` glyph, a vertical mark that remains visible even in no-color mode. The active tab also uses bold underline, with mint added when color is available. Separators inside status and title strings use the middle dot (`·`), while missing values use an em dash (`—`).

There are no pills, floating cards, badges, rounded controls, or decorative icons. Geometry serves structure: frames group a region, horizontal rules separate fixed chrome, and the selection bar locates the current row.

**The Square Instrument Rule.** Keep every enclosure flush, rectangular, and one cell thick; rounded web-control silhouettes do not belong in this system.

## Components

### Identity Header

The header combines a mint-filled `SERVICENOW` stamp with a high-text `OPERATIONS LEDGER` title. Its second row locates the operator in profile, instance when space allows, table, and page. A slate bottom rule separates identity from work without adding a panel fill.

### Record Ledger

The record ledger is the primary surface. Its border title includes the visible record range. Column headers are bold header text on the only dark tonal surface. Data rows are regular body text, one row high, and selected state becomes bold selection ink on mint with a leading `▌`. Arrow keys or `j`/`k` move one row; `g`/`G` jump to the first or last row. Page Up/Page Down mirror `p`/`n` pagination when a neighboring page exists.

### Record Sheet

In a wide layout, the sheet is a persistent index preview titled with the current record and the instruction `ENTER FOR ALL FIELDS`. Enter, Right Arrow, or `l` opens detail. For generic tables, expanded mode remains an all-fields record sheet: it uses a mint rule, names the record, states field count and scroll position, and exposes `j`/`k`, Page Up/Page Down, and `g`/`G` scrolling. Escape, Left Arrow, or `h` folds it back; `o` opens the selected record in ServiceNow. Field labels are bold mint and values are body text. Sensitive fields render as `[REDACTED]`.

### Incident Workspace

Incidents replace the generic expanded sheet with one square, mint-ruled workspace containing four views: `1 OVERVIEW`, `2 ACTIVITY`, `3 ATTACHMENTS`, and `4 SLAs`. The current tab is simultaneously marked by `▌`, bold weight, underline, and mint when color is enabled; inactive tabs stay muted. Tab advances and Shift-Tab reverses cyclically, while `1`–`4` jumps directly. Switching views resets scroll to the top. Counts appear beside tabs only when the workspace is wide enough; a truncated related view reports `100+` rather than implying a complete total.

Overview loads the complete incident record. If that request fails, it remains usable as a degraded `INDEX FIELDS ONLY` view with a coral `COMPLETE RECORD UNAVAILABLE` heading, the sanitized error, an amber `r` retry, and the ledger projection still visible. Activity shows newest comments and work notes with mint type labels, muted author/time metadata, and body copy. Attachments shows mint file names with size, content type, creator, and timestamp. SLAs show stage, progress, dates, and duration, with `ON TRACK` in mint or `BREACHED` in coral.

Activity, Attachments, and SLAs load lazily the first time they are selected. Each retains its own idle, loading, ready, empty, and failed state; a failure in one view never displaces Overview or another ready view. `r` reloads only the current incident view. Related requests fetch 101 items as a sentinel, render at most the latest 100, and disclose truncation both as `100+` in the tab and `latest 100 …; more available` in the view metadata and notice.

All workspace bodies wrap without trimming content and scroll by their visual wrapped-line extent rather than raw record count. This keeps long journal entries, file metadata, errors, and field values reachable at every supported width. ServiceNow strings and failure messages are stripped of control characters before display.

**The Active Tab Redundancy Rule.** Every active incident tab carries a structural mark and underline; mint reinforces that state but never owns it alone.

**The Lazy View Rule.** Load related views only when entered, preserve ready siblings, and make `r` retry the current view in place.

**The Hundred-Plus Rule.** Never present a truncated related view as complete: cap rendering at the latest 100 and disclose the sentinel as `100+` and `more available`.

### Footer Status and Keyboard Rail

The footer dedicates its first row to the latest notice and, at 90 columns or wider, the active query. Long notice and query text are truncated with an ellipsis to protect the key rail. The second row persistently lists movement, inspection, filtering, table selection, pagination, help, and quit keys. Available keys are amber; unavailable previous or next actions are muted and relabeled `start` or `end`.

### Input Sheet

`t` opens table input and `/` opens query input. The centered 7-row sheet uses a mint border, an uppercase task title, inline `ENTER APPLY` and `ESC CANCEL` instructions, a muted prompt, an amber `›`, and bold body input. Long input scrolls horizontally to keep its tail and cursor visible. Enter applies; Escape cancels; Backspace edits.

### Keyboard Map

`?` opens a centered active-border sheet pairing amber key groups with body-text descriptions. It includes Tab/Shift-Tab, direct `1`–`4` incident navigation, and the context-sensitive meaning of `r`. `?`, Escape, or `q` closes it and returns to the prior ledger, generic record sheet, or incident workspace. Its closing read-only statement appears in mint, reinforcing product safety without relying on an icon.

### Loading, Error, and Empty States

- **Loading:** An inset framed panel states `INDEXING RECORDS` and names the table and instance being read. Generic complete-record loading stays inside the expanded sheet. Each incident view names its own loading task without blocking sibling views.
- **Error:** A coral framed panel distinguishes ledger load failure from emptiness, repeats the safe error notice, and offers `r`, `/`, and `t` recovery keys. Incident Overview degrades to `INDEX FIELDS ONLY`; each related incident view keeps its failure and retry local while stating that Overview remains available.
- **Empty:** A normally framed `RECORD INDEX` states that no records match and offers query or table changes. Activity, Attachments, and SLAs each use a specific empty heading and an `r reload this view` action.
- **No selection:** The preview sheet plainly asks the operator to select a record.
- **Tiny terminal:** The shell is replaced with the minimum-size instruction and quit keys.

### Interaction Grammar

The base ledger accepts `↑`/`k`, `↓`/`j`, `g`/`G`, Enter/Right/`l`, `r`, `n`/`p`, Page Down/Page Up, `t`, `/`, `?`, `o`, `q`, and Ctrl-C. Overlays narrow that vocabulary to their local task: inputs accept text editing plus Enter/Escape; the help sheet accepts only close keys; generic detail accepts scroll, close, help, open, and quit; incident detail adds Tab/Shift-Tab, `1`–`4`, and current-view reload with `r`. Ctrl-C remains a global exit. All navigation operates on key press and requires no mouse.

## Do's and Don'ts

### Do:

- **Do** preserve the operations-ledger hierarchy: location, index, contextual detail, then status and keys.
- **Do** keep mint for active/indexed/success state, amber for keys, and coral for errors.
- **Do** show focus and availability with glyphs, labels, position, and copy in addition to color.
- **Do** keep record rows at one terminal row and disclose fewer columns as width contracts.
- **Do** make loading, error, empty, no-selection, and undersized-terminal states visually and verbally distinct.
- **Do** retain the operator's context when opening and closing sheets, including returning help to an expanded record sheet.
- **Do** keep incident loading, failure, empty, and retry state local to each view; reset scroll on view changes and calculate it from the active view's wrapped content.
- **Do** disclose the 100-item display boundary with both `100+` and latest-100 language whenever the sentinel is present.
- **Do** sanitize terminal control characters and redact sensitive field values before rendering them.

### Don't:

- **Don't** turn the TUI into a miniature web dashboard with rounded cards, sidebars, charts, shadows, gradients, or decorative iconography.
- **Don't** use a filled accent surface anywhere except the brand stamp and selected record row.
- **Don't** make color the only signal for selection, failure, disabled pagination, focus, or success.
- **Don't** preserve secondary detail at the expense of the ledger on compact terminals.
- **Don't** turn incident tabs into decorative color-only navigation or report a truncated related view as a complete total.
- **Don't** hide essential actions behind mouse interaction or undocumented shortcuts.
- **Don't** let long notices, queries, input, or record values displace fixed chrome and recovery guidance.
- **Don't** render secrets, raw control characters, or credential-bearing configuration.
