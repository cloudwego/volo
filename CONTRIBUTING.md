# Contributing to Volo

:balloon: Thanks for your help improving the project! We are so happy to have you!

There are opportunities to contribute to Volo at any level. It doesn't matter if
you are just getting started with Rust or are the most weathered expert, we can
use your help.

**No contribution is too small and all contributions are valued.**

This guide will help you get started. **Do not let this guide intimidate you**.
It should be considered a map to help you navigate the process.

The develop discussion group on Feishu is available for any concerns not covered in this
guide, please join us!

<img src="https://github.com/cloudwego/volo/raw/main/.github/assets/volo-feishu-dev-group.png" alt="Volo dev group" width="50%" height="50%" />

_This guide is adapted from the [Tokio contributing guide](https://github.com/tokio-rs/tokio/blob/master/CONTRIBUTING.md)_.

## Conduct

See the [Volo Code of Conduct](https://github.com/cloudwego/volo/blob/main/CODE_OF_CONDUCT.md).

## Contributing in Issues

For any issue, there are fundamentally three ways an individual can contribute:

1. By opening the issue for discussion: For instance, if you believe that you
   have discovered a bug in Volo, creating a new issue in [the cloudwego/volo
   issue tracker][issue] is the way to report it.

2. By helping to triage the issue: This can be done by providing
   supporting details (a test case that demonstrates a bug), providing
   suggestions on how to address the issue, or ensuring that the issue is tagged
   correctly.

3. By helping to resolve the issue: Typically this is done either in the form of
   demonstrating that the issue reported is not a problem after all, or more
   often, by opening a Pull Request that changes some bit of something in
   Volo in a concrete and reviewable manner.

[issue]: https://github.com/cloudwego/volo/issues

**Anybody can participate in any stage of contribution**. We urge you to
participate in the discussion around bugs and participate in reviewing PRs.

### Asking for General Help

If you have reviewed existing documentation and still have questions or are
having problems, you can either:

1. Submit a new issue and describe your problem, or
2. Join the [Volo user group on Feishu](https://applink.feishu.cn/client/chat/chatter/add_by_link?link_token=b34v5470-8e4d-4c7d-bf50-8b2917af026b) and ask for help there.

In exchange for receiving help, we expect you to contribute back a documentation
PR that helps others avoid the problems that you encountered.

### Submitting a Bug Report

When opening a new issue in the Volo issue tracker, you will be presented
with a basic template that should be filled in. If you believe that you have
uncovered a bug, please fill out this form, following the template to the best
of your ability. Do not worry if you cannot answer every detail, just fill in
what you can.

The two most important pieces of information we need in order to properly
evaluate the report are a description of the behavior you are seeing and a simple
test case we can use to recreate the problem on our own. If we cannot recreate
the issue, it becomes impossible for us to fix.

In order to rule out the possibility of bugs introduced by userland code, test
cases should be limited, as much as possible, to using only Volo APIs.

See [How to create a Minimal, Complete, and Verifiable example][mcve].

[mcve]: https://stackoverflow.com/help/mcve

### Triaging a Bug Report

Once an issue has been opened, it is not uncommon for there to be discussion
around it. Some contributors may have differing opinions about the issue,
including whether the behavior being seen is a bug or a feature. This discussion
is part of the process and should be kept focused, helpful, and professional.

Short, clipped responses—that provide neither additional context nor supporting
detail—are not helpful or professional. To many, such responses are simply
annoying and unfriendly.

Contributors are encouraged to help one another make forward progress as much as
possible, empowering one another to solve issues collaboratively. If you choose
to comment on an issue that you feel either is not a problem that needs to be
fixed, or if you encounter information in an issue that you feel is incorrect,
explain why you feel that way with additional supporting context, and be willing
to be convinced that you may be wrong. By doing so, we can often reach the
correct outcome much faster.

### Resolving a Bug Report

In the majority of cases, issues are resolved by opening a Pull Request. The
process for opening and reviewing a Pull Request is similar to that of opening
and triaging issues, but carries with it a necessary review and approval
workflow that ensures that the proposed changes meet the minimal quality and
functional guidelines of the Volo project.

## Pull Requests

Pull Requests are the way concrete changes are made to the code, documentation,
and dependencies in the Volo repository.

Even tiny pull requests (e.g., one character pull request fixing a typo in API
documentation) are greatly appreciated. Before making a large change, it is
usually a good idea to first open an issue describing the change to solicit
feedback and guidance. This will increase the likelihood of the PR getting
merged.

### Tests

If the change being proposed alters code (as opposed to only documentation for
example), it is either adding new functionality to Volo or it is fixing
existing, broken functionality. In both of these cases, the pull request should
include one or more tests to ensure that Volo does not regress in the future.

#### Documentation tests

Ideally, every API has at least one [documentation test] that demonstrates how to
use the API. Documentation tests are run with `cargo test --doc`. This ensures
that the example is correct and provides additional test coverage.

The trick to documentation tests is striking a balance between being succinct
for a reader to understand and actually testing the API.

### Commits

It is a recommended best practice to keep your changes as logically grouped as
possible within individual commits. There is no limit to the number of commits
any single Pull Request may have, and many contributors find it easier to review
changes that are split across multiple commits.

That said, if you have a number of commits that are "checkpoints" and don't
represent a single logical change, please squash those together.

Note that multiple commits often get squashed when they are landed (see the
notes about [commit squashing](#commit-squashing)).

We also recommend you to update the [CHANGELOG.md](CHANGELOG.md) of the crate
you are changing together with your commits to keep track of every change.

#### Commit message guidelines

A good commit message should describe what changed and why, also known as the
[Conventional Commits](https://www.conventionalcommits.org/en/v1.0.0/).

1. The first line should:

  * contain the type and a short description of the change
  (preferably 50 characters or less, and no more than 72 characters)
  * be entirely in lowercase with the exception of proper nouns, acronyms, and
    the words that refer to code, like function/variable names

  Examples:

  * feat: allow users to set timeout for RPC requests
  * fix(codec): fix panic when decoding invalid data

2. Keep the second line blank.
3. Wrap all other lines at 72 columns (except for long URLs).
4. If your patch fixes an open issue, you can add a reference to it at the end
   of the log. Use the `Fixes: #` prefix and the issue number. For other
   references use `Refs: #`. `Refs` may include multiple issues, separated by a
   comma.

   Examples:

   - `Fixes: #1337`
   - `Refs: #1234`

Sample complete commit message:

```txt
fix(codec): fix panic when decoding invalid data

Body of commit message is a few lines of text, explaining things
in more detail, possibly giving some background about the issue
being fixed, etc.

The body of the commit message can be several paragraphs, and
please do proper word-wrap and keep columns shorter than about
72 characters or so. That way, `git log` will show things
nicely even when it is indented.

Fixes: #1337
Refs: #453, #154
```

### Opening the Pull Request

From within GitHub, opening a new Pull Request will present you with a
[template] that should be filled out. Please try to do your best at filling out
the details, but feel free to skip parts if you're not sure what to put.

[template]: .github/PULL_REQUEST_TEMPLATE.md

### Discuss and update

You will probably get feedback or requests for changes to your Pull Request.
This is a big part of the submission process so don't be discouraged! Some
contributors may sign off on the Pull Request right away, others may have
more detailed comments or feedback. This is a necessary part of the process
in order to evaluate whether the changes are correct and necessary.

**Any community member can review a PR and you might get conflicting feedback**.
Keep an eye out for comments from code owners to provide guidance on conflicting
feedback.

**Once the PR is open, do not rebase the commits**. See [Commit Squashing](#commit-squashing) for
more details.

### Commit Squashing

In most cases, **do not squash commits that you add to your Pull Request during
the review process**. When the commits in your Pull Request land, they may be
squashed into one commit per logical change. Metadata will be added to the
commit message (including links to the Pull Request, links to relevant issues,
and the names of the reviewers). The commit history of your Pull Request,
however, will stay intact on the Pull Request page.

## Reviewing Pull Requests

**Any Volo community member is welcome to review any pull request**.

All Volo contributors who choose to review and provide feedback on Pull
Requests have a responsibility to both the project and the individual making the
contribution. Reviews and feedback must be helpful, insightful, and geared
towards improving the contribution as opposed to simply blocking it. If there
are reasons why you feel the PR should not land, explain what those are. Do not
expect to be able to block a Pull Request from advancing simply because you say
"No" without giving an explanation. Be open to having your mind changed. Be open
to working with the contributor to make the Pull Request better.

Reviews that are dismissive or disrespectful of the contributor or any other
reviewers are strictly counter to the Code of Conduct.

When reviewing a Pull Request, the primary goals are for the codebase to improve
and for the person submitting the request to succeed. **Even if a Pull Request
does not land, the submitters should come away from the experience feeling like
their effort was not wasted or unappreciated**. Every Pull Request from a new
contributor is an opportunity to grow the community.

### Review a bit at a time.

Do not overwhelm new contributors.

It is tempting to micro-optimize and make everything about relative performance,
perfect grammar, or exact style matches. Do not succumb to that temptation.

Focus first on the most significant aspects of the change:

1. Does this change make sense for Volo?
2. Does this change make Volo better, even if only incrementally?
3. Are there clear bugs or larger scale issues that need attending to?
4. Is the commit message readable and correct? If it contains a breaking change
   is it clear enough?

Note that only **incremental** improvement is needed to land a PR. This means
that the PR does not need to be perfect, only better than the status quo. Follow
up PRs may be opened to continue iterating.

When changes are necessary, *request* them, do not *demand* them, and **do not
assume that the submitter already knows how to add a test or run a benchmark**.

Specific performance optimization techniques, coding styles and conventions
change over time. The first impression you give to a new contributor never does.

Nits (requests for small changes that are not essential) are fine, but try to
avoid stalling the Pull Request. Most nits can typically be fixed by the Volo
Collaborator landing the Pull Request but they can also be an opportunity for
the contributor to learn a bit more about the project.

It is always good to clearly indicate nits when you comment: e.g.
`Nit: change foo() to bar(). But this is not blocking.`

If your comments were addressed but were not folded automatically after new
commits or if they proved to be mistaken, please, [hide them][hiding-a-comment]
with the appropriate reason to keep the conversation flow concise and relevant.

### Be aware of the person behind the code

Be aware that *how* you communicate requests and reviews in your feedback can
have a significant impact on the success of the Pull Request. Yes, we may land
a particular change that makes Volo better, but the individual might just not
want to have anything to do with Volo ever again. The goal is not just having
good code.

### Abandoned or Stalled Pull Requests

If a Pull Request appears to be abandoned or stalled, it is polite to first
check with the contributor to see if they intend to continue the work before
checking if they would mind if you took it over (especially if it just has nits
left). When doing so, it is courteous to give the original contributor credit
for the work they started (either by preserving their name and email address in
the commit log, or by using an `Author: ` meta-data tag in the commit.

_Adapted from the [Node.js contributing guide][node]_.

[node]: https://github.com/nodejs/node/blob/master/CONTRIBUTING.md
[hiding-a-comment]: https://help.github.com/articles/managing-disruptive-comments/#hiding-a-comment
[documentation test]: https://doc.rust-lang.org/rustdoc/documentation-tests.html

## Keeping track of issues and PRs

The Volo GitHub repository has a lot of issues and PRs to keep track of. This
section explains the meaning of various labels. The section is primarily targeted
at maintainers. Most contributors aren't able to set these labels.

### Area

The area label describes the crates relevant to this issue or PR.

 - **A-volo** This issue concerns the main `volo` crate.
 - **A-volo-build** This issue concerns the `volo-build` crate.
 - **A-volo-cli** This issue concerns the `volo-cli` crate.
 - **A-volo-ecosystem** This issue concerns the volo ecosystem(a.k.a. volo-rs).
 - **A-volo-grpc** This issue concerns the `volo-grpc` crate.
 - **A-volo-macros** This issue concerns the `volo-macros` crate.
 - **A-volo-thrift** The issue concerns the `volo-thrift` crate.
 - **A-ci** This issue concerns our GitHub Actions setup.

### Category

 - **C-bug** This is a bug-report. Bug-fix PRs use `C-enhancement` instead.
 - **C-enhancement** This is a PR that adds a new feature or fixes a bug.
 - **C-maintenance** This is an issue or PR about stuff such as documentation,
   GitHub Actions or code quality.
 - **C-feature-request** This is a feature request issue. Implementations of feature
   requests use `C-enhancement` instead.
 - **C-feature-accepted** If you submit a PR for this feature request, we wont
   close it with the reason "we don't want this". Issues with this label should
   also have the `C-feature-request` label.
 - **C-musing** Stuff like tracking issues or roadmaps. "musings about a better
   world".
 - **C-proposal** A proposal of some kind, and a request for comments.
 - **C-question** A user question.
 - **C-request** A non-feature request, e.g. "please document the usage of xx".

### Calls for participation

 - **E-help-wanted** Stuff where we want help. Often seen together with `C-bug`
   or `C-feature-accepted`.
 - **E-easy** This is easy, ranging from quick documentation fixes to stuff you
   can do after reading the tutorial on our website.
 - **E-medium** This is neither `E-easy` not `E-hard`.
 - **E-hard** This either involves very tricky code, is something we don't know
   how to solve, or is difficult for some other reason.
 - **E-needs-mvce** This bug is missing a minimal complete and verifiable
   example.

The "E-" prefix is the same as used in the Rust compiler repository. Some
issues are missing a difficulty rating, but feel free to ask in our dev
group if you want to know how difficult an issue likely is.

### Tags

There are also some tags that are used to provide additional information.

- **T-duplicate** This is a duplicate of another issue.
- **T-wontfix** This issue is not a bug, which will not be worked on.
- **T-invalid** This issue is invalid.
- **T-good-first-issue** This issue is good for newcomers.

## Versioning Policy

We adhere to the [Semantic Versioning 2.0](https://semver.org/).

## Releasing

Since the Volo project consists of a number of crates, many of which depend on
each other, releasing new versions to crates.io can involve some complexities.
When releasing a new version of a crate, follow these steps:

1. Publish the new version to crates.io follow this order: `volo-macros`, `volo`,
   `volo-build`, `volo-cli`, `volo-thrift`, `volo-grpc`.
2. **Bump the version.** After releasing all the crates, update the
   `version` field in `Cargo.toml` to a new version.
