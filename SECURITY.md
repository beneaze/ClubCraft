# Security policy

## Reporting a vulnerability

Please **do not open a public issue** for security problems. Report them privately through
GitHub's [private vulnerability reporting](https://github.com/beneaze/ClubCraft/security/advisories/new)
("Report a vulnerability" on the Security tab). You will get an answer as soon as possible.

## Scope

ClubCraft is a desktop application. It reads audio files, its own library database, DJ.Studio
project files (read-only) and VirtualDJ `database.xml` files, and it runs ONNX / CoreML models from
its `models/` folder. Reports about parsing untrusted files, the model loading path or the
release packages are especially welcome.

Only the latest release is supported.
