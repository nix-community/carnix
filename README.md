Git mirror of Carnix
====================

This is a git mirror of the source code for `carnix`, a tool used to
generate Nix-expressions for Rust crates.

The original source repository is
[here](https://nest.pijul.com/pmeunier/carnix), but does not contain
versions older than `0.7.2`.

Various fixes for smaller issues in Carnix are being added on the
`devel`-branch of this repository. They will be submitted to the
upstream repository once it is back online.

~~Carnix was originally released by [Pijul][], however the [old source
repository][] was never updated after version `0.5` and is now offline
entirely.~~

~~There are still releases happening on [crates.io][] and I am simply
mirroring these into this git repository to make the source code
easily accessible, but for now there won't be any changes happening
here directly.~~

## Reporting issues

Issues should be reported in the [discussions][] in the source
repository.

## "Changelog"

Based on the `git diff` output between the versions I've attempted to
recreate a changelog of what has changed in between these versions of
Carnix.

This overview is probably missing some things as I only took a very
cursory look for now.

### v0.6.7

Repository was bootstrapped from `crates.io` sources at this version,
not sure what the difference to the previous versions is.

### v0.6.8

This version seems to contain minor changes to how `nix-prefetch-git`
errors are handled.

### v0.6.9

This version seems to contain changes to how Carnix handles the local
"workspace" of a crate and which files are included into the Nix
sources when building a derivation.

### v0.7.0

This version seems to contain the most major changes yet, with a new
CLI structure and a Cargo subcommand provided by Carnix.

I actually found a [pull request][] to `nixpkgs` in which the author
of Carnix, @P-E-Meunier, says the following about v0.7.0:

> Carnix 0.7 improves support for workspaces, environment variables, and
> introduces the external cargo command cargo generate-nixfile.
>
> The src field for workspaces, in the generated code, is now split
> between an src field, pointing to the root of the workspace, and a
> member field, specifying the remainder of the path. This fixes issues
> with members pointing to each other, and to the root.
>
> Also, environment variables generated by build scripts are now handled
> properly.

This PR bumps the version straight to `0.7.2` though, so the summary
probably includes "all of the above".

### v0.7.1

This version seems to contain refactorings related to error handling
and something about handling replaced packages in Cargo correctly.

### v0.7.2

This version seems to contain minor changes to workspace handling
again.

--------------

Again, just to be clear - I'm not the author of this project, I'm only
mirroring it here to ensure that there's an easily available source
repository.

All credit for `buildRustCrate` and `carnix` itself goes to the people
behind Pijul!

[Pijul]: https://pijul.org/2017/12/12/buildrustcrate/
[old source repository]: https://nest.pijul.com/pmeunier/nix-rust
[crates.io]: https://crates.io/crates/carnix
[pull request]: https://github.com/NixOS/nixpkgs/pull/39003
[discussions]: https://nest.pijul.com/pmeunier/carnix:master/discussions
