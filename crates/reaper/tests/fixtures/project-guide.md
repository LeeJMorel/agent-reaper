# Working in this repository

This file is for anyone making changes here, people and coding agents alike. Read it before you start. The sections are roughly in the order you will need them. If something here is out of date or contradicts the code, say so in your summary instead of quietly working around it.

## How we work

- Read the code you are about to change, and the code that calls it, before you change it. Most bad fixes come from changing a function without knowing who relies on it.
- Keep each change to what was asked. If you notice something else that should change, mention it when you finish instead of fixing it on the way. Unrelated edits make a change harder to review and harder to revert.
- Prefer the smallest change that fixes the problem properly. A one-line fix with a clear reason is better than a refactor that happens to include the fix.
- Don't rename, move or reorganise files as part of an unrelated change.
- If a request can be read more than one way, pick the reading that changes the least, and say which one you picked.
- If you can't finish, stop and say what is left, what you tried and where you got stuck. A half-done change described honestly is more useful than one that looks finished and isn't.
- Run what you changed. A fix that hasn't been run isn't done, however obvious it looks.
- Don't leave debugging output, commented-out code, or TODOs without a name and a reason.
- Don't install tools, upgrade the language runtime or change the machine's setup to get something working. Ask instead; the next person won't have your changes.
- When you are unsure whether something is allowed, it is cheaper to ask than to undo it.

## Project layout

- Application code lives at the top level and under `src/`. Scripts that operate on the repository itself (setup, formatting, database migrations) are Python files at the top level, each with a docstring saying what it does and how to run it.
- [told] Do not edit generated code (anything under generated/). Change its source and rerun the generator.
- Each module should have one job. If a file grows past a few hundred lines it is usually doing two, and it is worth saying so in your summary.
- Put shared code in a module named for what it does. Don't create `utils`, `helpers` or `misc` modules; they become places where unrelated code goes to hide.
- Keep entry points thin. A script's top level should read its arguments, call into functions and report the result.
- Data files that programs read sit next to the code that reads them, in plain text where possible, so they can be reviewed in a diff.

## Python

- Python 3.10 or later. Standard library only unless a dependency has been agreed (see Dependencies).
- Follow PEP 8 naming: `snake_case` for functions and variables, `CapWords` for classes, `UPPER_CASE` for module-level constants.
- Use f-strings for building strings, `pathlib` for paths, and `with` blocks for files and database connections, so they are closed even when something fails.
- Don't catch bare `Exception` unless you re-raise it or report it. Catch the specific error you expect, close to where it happens.
- A script that fails should exit with a non-zero status and a one-line message on stderr. `raise SystemExit("error: ...")` does both.
- Keep logic in functions so it can be imported and tested; don't run work at import time beyond defining things.
- Give public functions a docstring: one line saying what it returns or does, and more only when the reason isn't obvious from the code.
- Type hints are welcome on new functions. Don't add them to code you aren't otherwise changing.
- Compare with `None` using `is` and `is not`.
- Avoid mutable default arguments (`def f(items=[])`); use `None` and create the list inside.

## JavaScript

- ES modules everywhere (`"type": "module"` in `package.json`): `import` and `export`, never `require`.
- Named exports only. A module's exports are its public interface, so keep them few and name them well.
- `const` by default, `let` when you reassign, never `var`.
- Prefer `async`/`await` to raw promise chains, and always handle a rejected promise somewhere.
- Use `??` and `?.` instead of `||` and `&&` when 0, an empty string or `false` are valid values.
- [told] Never use localStorage directly. Use getItem, setItem and removeItem from src/lib/storage.js.
- Run scripts with `node` directly. There is no build step and no bundler; keep it that way unless we decide otherwise.
- Keep modules free of side effects on import where you can, so they can be imported in tests and scripts without starting anything.
- Compare with `===` and `!==`.

## Formatting

- Python files are formatted by the project's formatter where a project has one (`fmt.py`). It turns tabs into four spaces and strips trailing whitespace. Don't hand-format code around it, and don't argue with it in review.
- [told] Do not run the formatter over the whole tree. Format only the files you changed.
- Keep lines under 100 characters when that doesn't hurt readability. Long strings and URLs can stay long.
- JavaScript uses two-space indents, double quotes and semicolons.
- One blank line between methods, two between top-level functions and classes in Python.
- Group imports: standard library, then third-party, then our own, with a blank line between groups.

## Tests

- Tests live in `tests/` and run with `./test.sh` where a project has one. Run them before you say you're done, and say whether they passed.
- Read a failure's output in full before changing anything. The first failure is usually the one that matters; later ones often follow from it.
- [told] Do not change tests to make them pass. Change the code under test.
- Keep tests fast, independent of each other and independent of the network. A test that needs a service running isn't a unit test.
- Don't add sleeps to tests. Wait for the condition you need, with a timeout.
- Name tests for the behaviour they check: `test_rejects_empty_name`, not `test_name_2`.
- One behaviour per test where practical, so a failure points at one thing.
- Tests use plain `assert`; the runner reports the expression that failed.

## Database

- Development uses SQLite. Database files (`*.db`) are never committed.
- Schema changes go through `migrations/`, applied in order by `migrate.py`. Migrations are SQL files numbered `NNN_description.sql`.
- [told] Do not edit existing migrations. Add a new migration for each change.
- Use parameterised queries with `?` placeholders. Never build SQL by formatting strings, even for values you think are safe.
- Name tables in the plural (`users`) and columns in the singular (`email`). Every table has an integer `id` primary key.
- Keep each query's intent obvious: select the columns you need, not `*`, in application code.

## Dependencies

- Prefer the standard library. Don't add a third-party package without asking first. If one is agreed, pin its exact version and say why it was needed.
- [told] Do not edit vendored libraries under vendor/. Work around their bugs in our own code.
- Don't copy code from elsewhere into the repository without saying where it came from and under what licence.
- Don't upgrade a dependency as part of an unrelated change. Upgrades get their own change, with the release notes read.

## Security

- Treat anything from outside the program as untrusted: files, environment variables, network responses and user input. Check it where it comes in.
- [told] Do not put API keys or other secrets in files git tracks. Keep them in a git-ignored .env file.
- Don't log personal data or credentials, even at debug level, and don't include them in error messages.
- Never pass user input to a shell. Use argument lists instead of command strings when running other programs.
- When a check fails for a security reason, fail closed: refuse the action instead of falling back to something more permissive.
- Report anything that looks like a vulnerability in your summary, even if it is outside what you were asked to change.

## Errors and logging

- Error messages say what went wrong and what to do about it: "config file not found: settings.toml" rather than "error 3".
- Programs print results to stdout and problems to stderr, so output can be piped without the errors mixed in.
- Don't swallow errors to keep a script running. Let it fail loudly unless the caller can do something useful with the failure.
- No `print` debugging in committed code. If something needs to be visible while running, it is output, and it should be designed as output.

## Git

- Commit only when you are asked to.
- Commit messages have a short summary line in the imperative mood ("Fix rounding of negative amounts"), under 72 characters, then a blank line and the reason for the change if it isn't obvious.
- [told] Other sessions edit this working tree too. Do not use `git add -A`, `git add .` or `git commit -a`; stage the files you changed by path.
- Don't push, force-push, amend or rebase unless you are asked to.
- Databases, caches and editor files stay out of commits; `.gitignore` covers the usual ones. If something new needs ignoring, say so instead of committing it.
- Don't rewrite history on shared branches, ever.

## Reviews and pull requests

- A pull request does one thing. Its description says what changed, why, and how you checked it.
- Keep pull requests small enough to review in one sitting. Split them when they aren't.
- Answer every review comment, either with a change or with a reason for not making one.
- Don't merge your own pull request without a review.
- Point out the risky part of your change in the description, so reviewers spend their attention there.

## Documentation

- Comments and docstrings explain why, not what; the code already says what.
- Write for someone new to the project: plain words, short sentences, and no internal jargon without a definition.
- Examples in docstrings should be runnable as written.

## Performance

- Measure before optimising, and say what you measured and how.
- Don't read whole large files into memory when you can process them line by line.
- Move work that doesn't change between iterations out of loops.
- Don't add caching without a way to invalidate it.

## Naming

- Names say what a thing is or does, in the project's own words. Avoid abbreviations except widely known ones (`id`, `url`, `db`).
- Booleans read as questions: `is_ready`, `has_items`.
- Functions that return a value are named for the value (`total`, `user_count`); functions that act are named for the action (`save`, `send_invite`).
- Don't encode types in names (`name_str`, `items_list`).

## Money, time and text

- Store money as integer cents and format it only for display. Never use floats for money.
- Store times in UTC and convert to local time only for display.
- Text is UTF-8 everywhere. Don't assume one byte per character when slicing or measuring strings.

## User interface

- Keep text that users see together, not scattered through logic, so it can be translated later.
- Use sentence case for headings, labels and buttons.
- Every interactive element must be reachable by keyboard and have a visible focus state.
- Colour is never the only way something is shown. The dark theme meets the same contrast requirements as the light one.

## When you finish

- Summarise what you changed, file by file, and how you checked it.
- Say what you didn't do and why, and anything you noticed that someone should look at.
- If you changed behaviour that someone might rely on, say that first.
