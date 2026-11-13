# Contributing to CircuitCraft

Thanks for helping. CircuitCraft is early, so the most useful contributions right now are the open
decisions and the next milestone in [ROADMAP.md](ROADMAP.md).

## Before you start

- Read [AGENTS.md](AGENTS.md). Its rules apply to everyone, whether you write code by hand or with
  an agent: never crash, clean room, everything is a command, strict crate layers.
- Look for an existing issue or pull request first. Comment on the issue to claim it, and release
  the claim if you won't finish.
- Security problems go through private reporting, not public issues; see [SECURITY.md](SECURITY.md).

## Making a change

1. Branch from `main`. `main` only changes through pull requests.
2. Keep the change narrow and add tests. A crash fix needs a test that crashed before the fix.
3. Run `cargo xtask ci` before you push: formatting, clippy, tests, asset attribution, dependency
   licences, crate layers and the browser build. CI runs the same checks on Linux, macOS and
   Windows, and all three must pass.
4. Open a pull request that says what changed, why, and what you tested. Say what you could not
   run.
5. Resolve every review conversation. Pull requests are squash-merged.

A new dependency needs a licence on the allowlist in AGENTS.md. A new asset (icon, image, font,
3D model) needs a row in [ATTRIBUTION.md](ATTRIBUTION.md) in the same pull request. Changes to CI
go in their own pull request.

## Sign off your commits (DCO)

CircuitCraft uses the [Developer Certificate of Origin 1.1](https://developercertificate.org/).
By signing off a commit you certify that you wrote it, or otherwise have the right to submit it,
under the project's licence (MIT OR Apache-2.0).

Sign off by adding a trailer with your name and email to every commit:

```text
Signed-off-by: Your Name <you@example.com>
```

`git commit -s` adds it for you. To sign off commits you already made on your branch:

```sh
git commit --amend -s --no-edit         # the last commit
git rebase --signoff main               # every commit since main
git push --force-with-lease             # update your pull request branch
```

Use a name you are known by and an email you can be reached at; it doesn't have to be your legal
name. Commits from Dependabot don't need a sign-off.

If an agent helped write the change, you are still the one signing off and the one accountable for
it, so review what it wrote before you sign.

## Licence

Contributions are licensed under MIT OR Apache-2.0, the same as the rest of the project, without
additional terms.
