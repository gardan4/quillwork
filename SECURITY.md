# Security policy

## Supported versions

Quillwork is alpha software. Only the latest `0.0.x` release receives fixes.

| Version | Supported |
|---|---|
| latest 0.0.x | yes |
| anything older | no |

## Reporting a vulnerability

Please **do not open a public issue** for a security problem. Report it privately through
GitHub instead: go to the repository's **Security** tab and choose **Report a vulnerability**
([direct link](https://github.com/gardan4/quillwork/security/advisories/new)).

Include what you can of:

- the affected component (`quillwork-core`, the Python binding, `quillwork-server`, the viewer),
- a description of the problem and its impact,
- steps or a synthetic document that reproduces it. Never attach a confidential document.

You should get a first reply within 7 days. Once a fix is ready we publish a GitHub security
advisory and credit you, unless you would rather stay anonymous.

## Scope

Quillwork opens untrusted `.docx` files and acts on instructions from language models, so these
are in scope:

- crashes, hangs or excessive memory use caused by a crafted file (zip bombs, deep nesting,
  huge parts),
- path traversal or reads outside the document store in `quillwork-server`,
- an edit that changes parts of the document it should not touch, or hides a change from the
  tracked-change record,
- hidden or near-invisible text that the projection fails to flag, so that text a reader cannot
  see can steer an agent (prompt injection through the document),
- script injection through the viewer.

Out of scope: problems in Word, LibreOffice or other software that opens the files Quillwork
writes, unless Quillwork wrote something invalid.
