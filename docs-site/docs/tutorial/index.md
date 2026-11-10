# Tutorial: release a repository

In this tutorial you write a wizard that releases a git repository: it checks the working
tree, picks a version, tags it and pushes the tag. Each page adds to the same file, and
introduces a few parts of korin.

1. [Look before you change](1-look.md): stages, `read`, captures, `assert`.
2. [Values](2-values.md): params, `ask`, capture modes, `each`.
3. [Expects](3-expects.md): expected failures, and `when`.
4. [Make changes](4-changes.md): `mutate`, gates, `confirm`, `record`, and running again.
5. [Reuse](5-reuse.md): templates, modules, `allow`, and `--from`.

Everything happens in a scratch directory, with a local "remote" repository. Nothing touches
your own repositories or any server.

## What you need

- korin. See [Install](../get-started/install.md).
- git 2.28 or later.
- A terminal. korin asks you to approve each stage, so it needs one.

## Set up the playground

Make a directory for the tutorial, then download and run the setup script in it:

```sh
mkdir ~/korin-tutorial && cd ~/korin-tutorial
curl -fsSLO https://raw.githubusercontent.com/zerodahero/korin/main/docs-site/examples/tutorial/setup.sh
sh setup.sh
```

??? note "What setup.sh does"

    ```sh title="setup.sh"
    --8<-- "docs-site/examples/tutorial/setup.sh"
    ```

You now have:

```text
~/korin-tutorial/
  setup.sh
  remote.git/   a bare repository: the "server"
  work/         a clone with three commits and one release, v0.1.0
```

You will write `~/korin-tutorial/release.korin`, beside `work/` rather than in it, so the
wizard is not part of the repository it releases. Run it from inside `work/`:

```sh
cd ~/korin-tutorial/work
korin run ../release.korin
```

To start over at any point, delete `~/korin-tutorial/work` and `remote.git`, then run
`sh setup.sh` again.
