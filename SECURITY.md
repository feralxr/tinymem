# Security

tinymem runs locally and makes no network requests. It reads the folder you point it at and writes only to `.tinymem/` inside it.

Things worth knowing:

- **The index contains your file contents.** `.tinymem/index.db` holds the text of every indexed file. A `.gitignore` is created inside `.tinymem/` so it is not committed by accident; do not share the file if the project is sensitive.
- **Exclude secrets.** Files ignored by git are skipped. Use `--exclude` for anything else you do not want indexed, such as `*.pem` or `.env*`.
- **MCP server scope.** `tinymem serve` exposes search, memory and re-index over stdio to the client that launched it, and reads only the project folder recorded in its index.

To report a vulnerability, please use GitHub's private vulnerability reporting on this repository rather than a public issue.
