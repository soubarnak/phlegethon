# PROTOTYPE — download queue UI

Throwaway fidelity for [Prototype: clean download queue and progress](https://github.com/soubarnak/phlegethon/issues/9).

**Question:** What does a Geist-inspired, low-clutter download queue look and feel like (progress, Paused/Failed/Retry, batch grouping)?

**Variants** (URL `?variant=` or bottom bar / ← →):

| Key | Name | Structure |
| --- | --- | --- |
| A | Dense rows | Flat list; batch as a quiet header row only |
| B | Batch tree | Collapsible groups are the primary hierarchy |
| C | Split inspector | Compact strip + detail pane for the selected job |

## Run

```bash
python3 -m http.server 5179 --directory prototypes/download-queue
```

Open http://localhost:5179/?variant=A

State is in-memory only. Not for merge to product UI as-is.
