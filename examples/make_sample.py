"""Write a small synthetic contract (``sample-contract.docx``) to edit in the other examples.

    python examples/make_sample.py [OUT.docx]

It has the things a real-world contract has and a careless editor loses: a letterhead in the page
header, a heading style, a numbered clause list, bold and italic runs and a table. Every name in it
is made up. Needs ``python-docx`` (``pip install python-docx``); the edits themselves do not.
"""

from __future__ import annotations

import sys
from datetime import datetime
from pathlib import Path

from docx import Document
from docx.enum.text import WD_ALIGN_PARAGRAPH
from docx.shared import Pt, RGBColor


def build(out: Path) -> None:
    doc = Document()
    core = doc.core_properties
    core.author = "Quillwork examples"
    core.title = "Sample services agreement"
    core.created = core.modified = datetime(2026, 1, 1)
    core.last_modified_by = "Quillwork examples"
    core.revision = 1

    header = doc.sections[0].header.paragraphs[0]
    header.alignment = WD_ALIGN_PARAGRAPH.RIGHT
    brand = header.add_run("EXAMPLE CONSULTING LTD")
    brand.bold = True
    brand.font.color.rgb = RGBColor(0x1F, 0x4E, 0x79)
    header.add_run("\n1 Sample Street, Exampletown").font.size = Pt(8)

    doc.add_heading("Services Agreement", level=1)
    intro = doc.add_paragraph("This agreement is made between ")
    intro.add_run("Example Consulting Ltd").bold = True
    intro.add_run(' ("the Consultant") and ')
    intro.add_run("Sample Client BV").bold = True
    intro.add_run(' ("the Client").')

    doc.add_heading("1 Scope and fees", level=2)
    for clause in (
        "The Consultant provides advice on employment policy for 16 hours per week.",
        "The hourly rate is EUR 95, excluding VAT.",
        "Invoices are due within 30 days of the invoice date.",
    ):
        doc.add_paragraph(clause, style="List Number")

    doc.add_heading("2 Term", level=2)
    term = doc.add_paragraph("The agreement starts on 1 March 2027 and runs for ")
    term.add_run("twelve months").italic = True
    term.add_run(". Either party may end it with one month's notice.")

    table = doc.add_table(rows=3, cols=3)
    table.style = "Light Grid Accent 1"
    for row, cells in zip(
        table.rows,
        (
            ("Role", "Hours", "Rate"),
            ("Senior adviser", "12", "EUR 95"),
            ("Adviser", "4", "EUR 75"),
        ),
    ):
        for cell, text in zip(row.cells, cells):
            cell.text = text

    doc.add_paragraph("Signed in Exampletown on the date of the last signature.")
    doc.save(out)


if __name__ == "__main__":
    target = Path(sys.argv[1]) if len(sys.argv) > 1 else Path("sample-contract.docx")
    build(target)
    print(f"wrote {target}")
