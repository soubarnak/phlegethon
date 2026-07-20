# PROTOTYPE — Probe-then-Confirm UI

Throwaway fidelity for [Prototype: Probe-then-Confirm selection surface](https://github.com/soubarnak/phlegethon/issues/10).

**Question:** What does the Probe-then-Confirm UI look like for a channel/playlist (item list, quality picker, select-all default, confirm CTA) without visual clutter?

**Variants** (`?variant=` / bottom bar / ← →):

| Key | Name | Structure |
| --- | --- | --- |
| A | Dense sheet | Flat checklist + per-row quality; sticky Confirm footer |
| B | Bulk quality bar | Checklist + one quality applied to selection; rare per-row override |
| C | Two-pane | Items left, quality + enqueue summary right |

Aligned with locked dense-row queue language from the queue prototype.

## Run

```bash
python3 -m http.server 5180 --directory prototypes/probe-then-confirm
```

Open http://localhost:5180/?variant=A
