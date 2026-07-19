# Restream Console Design System

## Contents

- [1. Atmosphere & Identity](#1-atmosphere--identity)
- [2. Color](#2-color)
- [3. Typography](#3-typography)
- [4. Spacing & Layout](#4-spacing--layout)
- [5. Components](#5-components)
- [6. Motion & Interaction](#6-motion--interaction)
- [7. Depth & Surface](#7-depth--surface)
- [8. Accessibility Constraints & Accepted Debt](#8-accessibility-constraints--accepted-debt)

## 1. Atmosphere & Identity

Restream is a compact broadcast operations console. It should feel quiet,
precise, and immediately scannable under time pressure. Its signature is
status-led density: restrained surfaces, clear state color, and media that
appears only when an operator asks for it.

## 2. Color

The console uses DaisyUI's `dim` theme. Components use semantic Tailwind and
DaisyUI tokens rather than fixed palette values.

| Role | Token | Usage |
|---|---|---|
| Page surface | `base-300` | Application background |
| Section surface | `base-200` | Primary panels |
| Item surface | `base-100` | Rows, cards, menus |
| Primary text | `base-content` | Labels and values |
| Secondary text | `base-content/70` | Supporting information |
| Border | `base-content/10` | Panel and row separation |
| Primary action | `primary` | Create and submit actions |
| Operational action | `accent` | Start, select, preview, and promote |
| Healthy | `success` | Connected and forwarding |
| Attention | `warning` | Recovering and awaiting media |
| Failure | `error` | Failed and destructive actions |
| Information | `info` | Neutral runtime notices |

Status color always carries meaning. Decorative color and gradients are not
part of the console language.

## 3. Typography

The primary stack is Aptos, Segoe UI Variable, Segoe UI, IBM Plex Sans,
Helvetica Neue, Arial, and sans-serif. The monospace stack is Cascadia Mono,
Cascadia Code, JetBrains Mono, SFMono-Regular, Consolas, Liberation Mono, and
monospace.

| Level | Size | Weight | Usage |
|---|---|---|---|
| Page title | 18px | 600 | Workspace title |
| Section title | 16px | 600 | Panel heading |
| Item title | 14px | 600 | Input, output, or monitor name |
| Body | 15px | 400 | Default console text |
| Supporting | 14px | 400 | Descriptions |
| Metadata | 12px | 400-600 | Runtime and protocol details |
| Compact label | 11px | 600 | Uppercase field labels |

Letter spacing is zero except the existing compact uppercase labels, which use
the theme's `tracking-wide` utility.

## 4. Spacing & Layout

Spacing follows Tailwind's 4px base unit. Standard panel padding is 16px,
compact row padding is 8-12px, and section gaps are 12-16px. Content is limited
to `max-w-7xl`.

The workspace owns document scroll. Monitor cards use a responsive intrinsic
grid and fixed 16:9 media frames. At 375px, controls and rows reflow to one
column without horizontal page scrolling. Stream keys and URLs may scroll or
wrap within their own bounded field.

## 5. Components

### Dashboard Section
- **Structure**: semantic section, optional header, full-width content.
- **Variants**: standard, table, compact.
- **States**: loading, empty, populated, error.
- **Accessibility**: labelled by its heading.
- **Layout**: stack; document is the scroll owner.

### Operational Row
- **Structure**: title and state, metadata, command cluster.
- **Variants**: selected, standby, disabled, disconnected.
- **States**: default, hover, focus, busy, error.
- **Accessibility**: full command labels include the affected input or output.
- **Layout**: wrapping cluster that becomes a vertical stack on narrow widths.

### Status Badge
- **Structure**: short state label and optional detail.
- **Variants**: healthy, attention, failure, neutral.
- **States**: static; color is never the only state indicator.
- **Accessibility**: readable text at WCAG AA contrast.

### Monitor Card
- **Structure**: input/output title, state, actions, 16:9 media frame.
- **Variants**: input, pipeline program, output monitor, empty.
- **States**: idle, loading, playing, offline, error.
- **Accessibility**: media controls are keyboard reachable; buttons identify
  their monitor.
- **Motion**: state feedback only.
- **Layout**: frame inside an intrinsic grid; no nested card.

### Input Manager
- **Structure**: compact input rows plus an add command.
- **Variants**: selected primary, selected backup, standby, disabled.
- **States**: loading, empty, busy, error, maximum reached.
- **Accessibility**: labels, stream keys, URLs, and actions remain available to
  keyboard and screen-reader users.
- **Layout**: stack of rows with wrapping command clusters.

## 6. Motion & Interaction

Interactions use existing DaisyUI hover, active, and focus behavior. State
transitions use 100-200ms transform or opacity changes only. Loading media may
use the existing spinner. `prefers-reduced-motion` is respected by the
framework and no decorative motion is added.

## 7. Depth & Surface

The strategy is borders plus tonal shifts. Sections use `base-200`, rows use
`base-100`, and `base-content/10` borders provide separation. Shadows are
reserved for popovers and menus. Cards use at most an 8px radius; existing
legacy monitor cards are accepted until their surrounding control-room
redesign is separately scoped.

## 8. Accessibility Constraints & Accepted Debt

The target is WCAG 2.2 AA: 4.5:1 body text contrast, 3:1 large text contrast,
visible focus, full keyboard reachability, semantic buttons and labels, and
no color-only status. Touch targets remain at least 36px in compact operator
surfaces.

| Item | Location | Why accepted | Owner / Exit |
|---|---|---|---|
| Legacy monitor card radius exceeds 8px | `web/ts/features/control-room.ts` | Existing visual contract outside this feature | Consolidate during a dedicated control-room redesign |
