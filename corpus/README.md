# Corpus

- `synthetic/`: generated files we own (`gen.py` makes `doc50.docx` / `doc200.docx`). Committed.
- Real client documents are **never committed**. Tests read them from the directory in
  `QUILLWORK_PRIVATE_CORPUS` (default: `../quillwork-private-corpus`) and skip when it is absent.
  Report only numbers from them (sizes, pass/fail, timings), never their text.
