# Security Policy

## Supported versions

Rueca is in alpha. Security fixes are made on `main` and released in the next
version; only the latest release is supported.

## Reporting a vulnerability

Please **do not open a public issue**. Report privately through GitHub's
[private vulnerability reporting](https://github.com/DevArKaDiA/rueca/security/advisories/new)
form. Include the version (`rueca --version`), platform, and a reproduction
(a minimal repository or file is ideal).

You can expect an acknowledgement within 7 days. We will keep you informed
while a fix is prepared and credit you in the advisory unless you prefer
otherwise.

## Scope

Rueca reads untrusted input by design: it parses arbitrary source files and
repository metadata. Relevant issues include, for example:

- crashes, hangs or unbounded memory use triggered by a crafted file or
  repository (parsing is budgeted by size, time and depth — bypasses count);
- reading or writing files outside the indexed working tree or the store
  directory;
- path traversal through symlinks, git metadata or tool arguments;
- the MCP server exposing content outside the indexed roots.

Segment and pack files in the local store are treated as trusted output of
Rueca itself; a crafted segment that crashes the reader is a bug, but not a
vulnerability, unless it can be planted by indexing a repository.
