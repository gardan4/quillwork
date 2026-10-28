# Examples

The examples use invented documents and Python 3.12+ with a local Quillwork build.

```sh
pip install python-docx    # plus quillwork: see "Install" in the main README
python examples/make_sample.py sample-contract.docx
python examples/edit_contract.py sample-contract.docx
```

- `make_sample.py` writes a small services agreement with a letterhead, numbered clauses, a
  table and mixed formatting. It is invented text; nothing in it comes from a real document.
- `edit_contract.py` opens it, proves a no-op save is byte-identical, applies one transaction
  (a replace, a new numbered clause and a comment, all as tracked changes) and reports which
  zip entries changed. It prints the projection an agent reads before and after, and writes
  `sample-contract.edited.docx`, which you can open in Word to accept or reject the changes.

`edit_contract.py` works on any `.docx`, but its anchors (`b103`, `b104`, `b105`) and expected
text match the sample.

For a lower-level look at what one tracked replace writes into the XML, see
[`crates/quillwork-py/examples/replace_text.py`](../crates/quillwork-py/examples/replace_text.py).

For PowerPoint template generation (unreleased):

```sh
python corpus/synthetic/gen_presentation.py business-template.pptx
python examples/fill_presentation.py business-template.pptx business-proposal.pptx
```

The generator uses only the Python standard library. The walkthrough fills client, period and
fee slots across formatted runs, table cells and speaker notes. It verifies untouched ZIP
entries and saves the candidate alongside a hash-bound review report. See
[PowerPoint editing](../docs/POWERPOINT.md) for the approval workflow and limits.
