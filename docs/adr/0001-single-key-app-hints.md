# Single-key app hints

Each app row in the popup shows a one-character "hint" badge. Pressing the hint key activates that app and dismisses the popup — same effect as `Enter` on that row. Hints are assigned sequentially in row order from a fixed priority list optimised for the left hand (mouse-in-right-hand workflow), with no fallback when the list is exhausted.

## The four decisions

**Source: sequential by row order, not mnemonic and not user-configured.** The Nth app in the popup gets the Nth letter from the priority list. Hints are always visible, so the user reads them rather than recalls them — perfect predictability across sessions isn't required, and avoiding manual config keeps the feature zero-setup.

**Pool order: `f s a r e w t v c x b z h l n m u i o p y`.** Left-hand keys first (the user's right hand is on the mouse), ordered home-row → top-row → bottom-row, index → middle → ring → pinky within each. Right-hand keys are the spillover and ordered alphabetically since they're all roughly equally inconvenient when reaching off the mouse. Already-bound single keys (`j k g d q 0–9`) are excluded from the pool. Assumes QWERTY/QWERTZ; Dvorak/Colemak would need a different list (deferred — add a config option if anyone asks).

**Overflow: cap at 21.** Apps 22+ render without a hint and are reached via `j/k` or click. Multi-letter hints (vim-style prefix chords) were rejected — they introduce state that interacts awkwardly with the existing `dd`/`gg` pending modes for a power-user case that's better served by type-to-filter (if that ever lands).

**Action: activate + close, like `Enter` on the row.** Single keystroke to teleport. Cursor-only movement was rejected because two keystrokes (hint + Enter) would defeat the ergonomic point. Activating but keeping the popup open was rejected because the popup would then float over the new desktop until manually dismissed.

## Rejected alternatives worth remembering

- **Mnemonic hints (first letter of `WM_CLASS`)** — break the moment two apps share a starting letter, and the meaning of any given hint shifts as apps come and go.
- **Hint-mode toggle (press `;` to enter, letters appear)** — same complexity as always-on for less utility once the user is already inside the popup.
- **Runtime marks (`m{letter}` to set, `'{letter}` to jump)** — requires the user to set up marks every session.
- **`Shift+letter` for 22+ overflow** — adds friction (shift) and conflicts with the existing `G` jump-to-bottom binding pattern.

## Consequences

- Muscle memory builds against *positions* in the row order, not against apps. Closing or opening windows reshuffles hints. This is acceptable because hints are always visible.
- The pool is hardcoded to QWERTY/QWERTZ left-hand geometry. Non-QWERTY users will find the priority order suboptimal until a config knob is added.
- The hint badge takes a fixed inset on the left of each app row (~18–22px), making rows slightly narrower for the title.
