# Contributing

Community contributions to `libdatadog` are welcome 😃! See below for some basic guidelines.

## Want to request a new feature?

Many great ideas for new features come from the community, and we'd be happy to consider yours!

To share your request, you can [open a Github issue](https://github.com/datadog/libdatadog/issues/new) with the details
about what you'd like to see. At a minimum, please provide:

* The goal of the new feature
* A description of how it might be used or behave
* Links to any important resources (e.g. GitHub repos, websites, screenshots, specifications, diagrams)

Additionally, if you can, include:

* A description of how it could be accomplished
* Code snippets that might demonstrate its use or implementation
* Screenshots or mockups that visually demonstrate the feature
* Links to similar features that would serve as a good comparison
* (Any other details that would be useful for implementing this feature!)

## Found a bug?

For any urgent matters (such as outages) or issues concerning the Datadog service or UI, contact our support team via
https://docs.datadoghq.com/help/ for direct, faster assistance.

You can submit bug reports concerning `libdatadog` by
[opening a Github issue](https://github.com/datadog/libdatadog/issues/new). At a minimum, please provide:

* A description of the problem
* Steps to reproduce
* Expected behavior
* Actual behavior
* Errors or warnings received
* Any details you can share about your configuration

If at all possible, also provide:

* Logs (from the library/profiler/application/agent) or other diagnostics
* Screenshots, links, or other visual aids that are publicly accessible
* Code sample or test that reproduces the problem
* An explanation of what causes the bug and/or how it can be fixed

Reports that include rich detail are better, and ones with code that reproduce the bug are best.

## Have a patch?

We welcome code contributions to the library, which you can
[submit as a pull request](https://github.com/datadog/libdatadog/pull/new/main).
To create a pull request:

1. **Fork the repository** from <https://github.com/datadog/libdatadog>
2. **Make any changes** for your patch
3. **Write tests** that demonstrate how the feature works or how the bug is fixed
4. **Update any documentation** especially for new features.
5. **Submit the pull request** from your fork back to the latest revision of the `main` branch on
   <https://github.com/datadog/libdatadog>

The pull request will be run through our CI pipeline, and a project member will review the changes with you.
At a minimum, to be accepted and merged, pull requests must:

* Have a stated goal and detailed description of the changes made
* Include thorough test coverage and documentation, where applicable
* Pass all tests and code quality checks (linting/coverage/benchmarks) on CI
* Receive at least one approval from a project member with push permissions

We also recommend that you share in your description:

* Any motivations or intent for the contribution
* Links to any issues/pull requests it might be related to
* Links to any webpages or other external resources that might be related to the change
* Screenshots, code samples, or other visual aids that demonstrate the changes or how they are implemented
* Benchmarks if the feature is anticipated to have performance implications
* Any limitations, constraints or risks that are important to consider

If at any point you have a question or need assistance with your pull request, feel free to mention a project member!
We're always happy to help contributors with their pull requests.

## Code Formatting

All Rust code must be formatted with `rustfmt` using the project's configuration in `rustfmt.toml`. You can format your
code locally by running:

```bash
cargo +nightly fmt --all
```

If you'd like CI to automatically format your code and commit the changes to your PR, add the `commit-rustfmt-changes`
label to your pull request. This will trigger an automatic formatting commit if any changes are needed.

## Commit Message Guidelines

This project uses [Conventional Commits](https://www.conventionalcommits.org/) for commit messages and pull request titles.
This format helps us automatically generate changelogs and determine semantic versioning.

### Format

Commit messages and PR titles should follow this structure:

```
<type>[optional scope]: <description>

[optional body]

[optional footer(s)]
```

### Common Types

- **feat**: Code that adds features to the end user
- **fix**: A bug fix
- **docs**: Documentation changes only
- **style**: Code style changes (formatting, missing semicolons, etc.) that don't affect functionality
- **refactor**: Code changes that neither fix a bug nor add a feature. Removing a public interface is considered a refactor and should be marked with `!`.
- **perf**: Performance improvements
- **test**: Adding or updating tests
- **build**: Changes to the build system or external dependencies
- **ci**: Changes to CI configuration files and scripts
- **chore**: Other changes that don't modify src or test files

### Scope (Optional)

The scope provides additional context about which part of the codebase is affected:

```
feat(crashtracker): add signal handler for SIGSEGV
fix(profiling): correct memory leak in stack unwinding
docs(readme): update installation instructions
```

### Breaking Changes

Breaking changes should be indicated by a `!` after the type/scope:

```
feat!: remove deprecated API endpoint
```

### Examples

Good commit messages:
- `feat: add support for custom metadata tags`
- `fix(profiling): resolve deadlock in thread sampling`
- `docs: add examples for exception tracking`
- `chore: update dependencies to latest versions`
- `test(crashtracker): add integration tests for signal handling`

Poor commit messages:
- `update code` (not descriptive, missing type)
- `Fixed bug` (missing type format, not descriptive)
- `WIP` (not meaningful)

### Pull Request Titles

When your pull request is merged, all commits will be squashed into a single commit. The PR title will become the final
commit message, so it's important that it accurately describes your changes.For that reason your pull request title must
follow the conventional commit format described above. Our CI pipeline will automatically validate the PR title and fail
if it doesn't comply with the format. You can update the PR title at any time to fix any validation issues.

## Final word

Many thanks to all of our contributors, and looking forward to seeing you on Github! :tada:
