## Workflow

The trx-rs project is organized into three main components:
1. **trx-core**: core library providing the basic functionalities,
2. **trx-server**: server component that hooks up the transceiver,
3. **trx-client**: client component that connects with the trx-server and exposes selected frontends.

When contributing to the project, please follow these guidelines:
- Fork the repository and create a new branch for your changes.
- Make sure your code follows the project's coding style and conventions.
- Write clear and concise commit messages.
- Submit a pull request with a detailed description of your changes.
- Ensure that your changes are tested and pass all existing tests.

## Commit Guidelines
- Use imperative mood in commit messages.
- Keep commit messages short and descriptive.
- Use a maximum of 80 characters per line.
- Use a blank line between the commit message and the body.
- Sign your commits with `git commit -s`.
- Explicitly mark LLM usage in commit messages with 'Co-authored-by:'.

Use the format below for commit titles:
[<type>](<crate>): <description>
e.g.
- [feat](trx-core): add new feature xyz
- [fix](trx-frontend): fix http frontend xyz issue
- [docs](trx-rs): update README

Use `(trx-rs)` for repo-wide changes that are not specific to any crate.

Allowed types:
- feat: new feature
- fix: bug fix
- docs: documentation changes
- style: code style changes
- refactor: code refactoring
- test: test changes
- chore: build or maintenance changes

Write isolated commits for each crate.
