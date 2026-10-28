# PowerPoint editing and business template generation

Quillwork can turn an existing business deck into a new editable PowerPoint candidate while preserving its design. An agent reads anchored text, changes the required content, and returns the original and candidate for review. A sales proposal, quarterly report, onboarding pack or training update can reuse the company's approved presentation instead of recreating its branding every time.

The first API edits existing text, including grouped shapes, table cells and speaker notes. It fills literal template slots across formatted runs. It does not create slides or invent a layout; prepare the appropriate template before filling it. Variable slide counts, image replacement and chart-data editing remain future work.

## Priorities

1. Preserve customer content and package bytes outside the intended edit. No-op saves are exact.
2. Refuse ambiguous or stale edits and unsupported structures rather than report success with lost content.
3. Keep existing text editable in PowerPoint and preserve layouts, masters, assets and run formatting.
4. Give the agent compact anchored text and return the changed text rather than resending the entire file.
5. Make review refer to exact source/candidate bytes and support an independent visual check.

Speed and package size remain budgets. Small JSON requests and precise replacements matter more than reconstructing every PowerPoint feature in an object model.

## Python

```python
from pathlib import Path
from quillwork import Presentation

original = Path("proposal-template.pptx").read_bytes()
deck = Presentation.open(original)
print(deck.read())

deck.fill(
    {"client": "Example client", "period": "November 2026", "fee": "EUR 4,500"},
    expected_counts={"client": 2, "period": 1, "fee": 1},
)
candidate = deck.save()
report = deck.review()
Path("proposal.candidate.pptx").write_bytes(candidate)
```

Generate the invented two-slide template and run a complete walkthrough:

```sh
python corpus/synthetic/gen_presentation.py business-template.pptx
python examples/fill_presentation.py business-template.pptx business-proposal.pptx
```

The example proves byte identity before editing, verifies unrelated ZIP entries retain their original stored bytes, and writes a review JSON report. Keep the template unchanged. Rendering and human approval happen after generation.

For an agent-selected edit, inspect `describe()` and pass both guards:

```python
model = deck.describe()
deck.apply({
    "base_version": model["version"],
    "base_sha256": model["sha256"],
    "ops": [{
        "op": "replace_text", "anchor": "s256.sh2.p1",
        "find": "Example client", "replace": "Updated client",
        "expect": "Proposal for Example client",
    }],
})
```

Discover anchors from the actual template. Native slide IDs are stable under reordering; they are not slide numbers. Session versions restart on reopen, so the content hash is necessary for requests persisted between sessions. See the [presentation contract](../spec/presentation/README.md) for exact limits and errors.

## Foundworks integration

Use the same job pattern for Word and PowerPoint: collect business facts, select an approved template, generate a candidate with guarded edits, review it, then publish the exact approved bytes. Word offers native tracked changes. Presentations use a source/candidate report and a visual comparison of affected slides. Microsoft no longer provides Compare/Merge in recent Microsoft 365 PowerPoint for Windows, and it is unavailable on Mac/web. [Microsoft guidance](https://support.microsoft.com/en-au/powerpoint/track-changes-in-your-presentation).

Store the original/candidate bytes and their hashes together. Before committing, compare the host's current document version/digest atomically; an engine object does not coordinate other processes. Record approval against `candidate_sha256`, then verify those bytes again before download or delivery. Rejecting the proposal keeps the source intact.

The library reports changed text and preserves package structure. Foundworks chooses the business workflow, permissions, source facts, template, renderer and approval UI. The current Word reference HTTP/MCP service and TypeScript viewer do not yet route or display this presentation API.

## Validation and limitations

Run the Rust/Python tests, inspect the saved package independently, and render edited decks before delivery. Text can overflow an unchanged box; replacing a short placeholder with a long paragraph does not reflow the slide or shrink the font. The review report is a text diff, not proof of visual fit. Matching actual PowerPoint rendering requires compatibility checks in PowerPoint.

Only transitional `.pptx` is supported. Existing notes are editable; adding notes parts is not. Empty/field/break paragraphs are read-only in this slice. Unsupported alternate content, SmartArt and chart text are preserved without complete projection. Formatting inheritance and visibility through layouts/masters are not fully resolved. `read()` currently returns the whole deck; hosts can select slides/paragraphs from `describe()` before sending context to an agent. Native paging and search remain future work. No `.potx`, `.pptm`, legacy `.ppt`, new slide structure, image editing, presentation comments or native tracked changes are promised.

The next useful additions are safe slide duplication from templates with correct dependency ownership, slide ordering/deletion, image replacement with alt text, and chart-data updates with linked workbook integrity. Each needs its own format contract, locality tests and independent render checks.
